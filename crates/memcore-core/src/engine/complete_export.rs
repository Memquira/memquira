//! Opt-in COMPLETE user export (the default export keeps its documented caps).
//!
//! - Every section is read to its end in bounded keyset pages with a deterministic order: facts
//!   (updated_at, id) descending, memory events (created_at, id) descending, episodes (recorded_at,
//!   id) ascending; entities, links and relations are read per user and sorted by id.
//! - Record count is bounded: a running total over all sections is checked after every page, and
//!   an export larger than `max_records` is refused with an explicit error, never returned
//!   partially. This bounds RECORDS, not bytes: both reads are held in memory, and a record's size
//!   is limited only by the request-body limit of the write that created it.
//! - Consistency: the whole paginated read is done twice and the two reads must be identical (no
//!   duplicate ids either). A write that lands while the export runs normally makes them differ;
//!   after `COMPLETE_EXPORT_ATTEMPTS` unequal attempts the export is refused (Conflict). This is
//!   change DETECTION, not a point-in-time snapshot: a multi-statement write that stays half-applied
//!   across both reads is captured half-applied, and a change reverted between the reads is not
//!   seen. Supported use is a quiescent user. Qdrant vectors are not part of the export.
//! - Any store error during a page is returned as the export's error (no partial archive).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use uuid::Uuid;

use super::MemoryEngine;
use crate::{
    COMPLETE_EXPORT_ATTEMPTS, COMPLETE_EXPORT_MAX_PAGE_SIZE, CompleteExportInput,
    EXPORT_COMPLETE_MODE, EntityExport, Episode, ExportCompleteness, ExportSectionCounts, Fact,
    FactSearchQuery, MemoryEvent, MemoryEventQuery, PageCursor, TenantContext, UserMemoryExport,
};

#[derive(Debug, PartialEq)]
struct Sections {
    facts: Vec<Fact>,
    events: Vec<MemoryEvent>,
    episodes: Vec<Episode>,
    entities: EntityExport,
}

struct Budget {
    used: usize,
    max: usize,
}

impl Budget {
    fn add(&mut self, n: usize) -> MemcoreResult<()> {
        self.used = self.used.saturating_add(n);
        if self.used > self.max {
            return Err(MemcoreError::ValidationError(format!(
                "EXPORT_TOO_LARGE: the complete export exceeds MEMCORE_EXPORT_MAX_RECORDS ({}); \
                 nothing was returned",
                self.max
            )));
        }
        Ok(())
    }
}

impl MemoryEngine {
    /// Exports every record of the user (see the module documentation), with a `completeness`
    /// manifest. The archive is always v3-shaped: episodes and entities are present, possibly empty.
    pub async fn export_user_data_complete(
        &self,
        input: CompleteExportInput,
    ) -> MemcoreResult<UserMemoryExport> {
        super::validate_tenant(&input.tenant)?;
        if input.page_size == 0 || input.page_size > COMPLETE_EXPORT_MAX_PAGE_SIZE {
            return Err(MemcoreError::ValidationError(format!(
                "page_size must be between 1 and {COMPLETE_EXPORT_MAX_PAGE_SIZE}"
            )));
        }
        if input.max_records == 0 {
            return Err(MemcoreError::ValidationError(
                "max_records must be greater than 0".to_string(),
            ));
        }
        for _ in 0..COMPLETE_EXPORT_ATTEMPTS {
            let first = self.read_all_sections(&input).await?;
            let second = self.read_all_sections(&input).await?;
            if first == second && unique_ids(&first) {
                return Ok(build_export(&input, first));
            }
        }
        Err(MemcoreError::Conflict(format!(
            "EXPORT_CONCURRENT_MODIFICATION: the user's data changed during {COMPLETE_EXPORT_ATTEMPTS} \
             complete-export attempts; retry when writes have stopped"
        )))
    }

    async fn read_all_sections(&self, input: &CompleteExportInput) -> MemcoreResult<Sections> {
        let tenant = &input.tenant;
        let page = input.page_size;
        let mut budget = Budget {
            used: 0,
            max: input.max_records,
        };

        // Facts: (updated_at, id) descending keyset pages; stores return one look-ahead row.
        let mut facts = Vec::new();
        let mut cursor: Option<PageCursor> = None;
        loop {
            let mut rows = self
                .fact_store
                .search_facts(FactSearchQuery {
                    tenant: tenant.clone(),
                    memory_types: None,
                    query_text: None,
                    limit: page,
                    cursor: cursor.clone(),
                    include_deleted: input.include_deleted,
                })
                .await?;
            let more = rows.len() > page;
            rows.truncate(page);
            budget.add(rows.len())?;
            cursor = rows.last().map(|f| PageCursor {
                last_id: f.id.to_string(),
                last_sort_value: f.updated_at,
            });
            facts.extend(rows);
            if !more {
                break;
            }
        }

        // Memory events: (created_at, id) descending keyset pages.
        let mut events = Vec::new();
        if let Some(store) = &self.event_store {
            let mut cursor: Option<PageCursor> = None;
            loop {
                let mut query = MemoryEventQuery::new(tenant.clone(), page);
                query.cursor = cursor.clone();
                let mut rows = store.list_events(query).await?;
                let more = rows.len() > page;
                rows.truncate(page);
                budget.add(rows.len())?;
                cursor = rows.last().map(|e| PageCursor {
                    last_id: e.id.to_string(),
                    last_sort_value: e.created_at,
                });
                events.extend(rows);
                if !more {
                    break;
                }
            }
        }

        // Episodes: (recorded_at, id) ascending keyset pages.
        let mut episodes = Vec::new();
        if let Some(store) = self.episode_data().map(|e| e.store) {
            let mut after: Option<(DateTime<Utc>, Uuid)> = None;
            loop {
                let rows = store.list_episodes_after(tenant, after, page).await?;
                budget.add(rows.len())?;
                after = rows.last().map(|e| (e.recorded_at, e.id));
                let full = rows.len() == page;
                episodes.extend(rows);
                if !full {
                    break;
                }
            }
        }

        // Entities, fact links, relations: per user, sorted by id (deterministic order).
        let mut entities = match self.entity_data_store() {
            Some(store) => self.export_entities(tenant, store.as_ref()).await?,
            None => EntityExport::default(),
        };
        budget.add(entities.entities.len() + entities.links.len() + entities.relations.len())?;
        entities.entities.sort_by(|a, b| a.id.cmp(&b.id));
        entities
            .links
            .sort_by(|a, b| (a.entity_id, a.fact_id).cmp(&(b.entity_id, b.fact_id)));
        entities.relations.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(Sections {
            facts,
            events,
            episodes,
            entities,
        })
    }
}

fn unique_ids(s: &Sections) -> bool {
    s.facts.iter().map(|f| f.id).collect::<HashSet<_>>().len() == s.facts.len()
        && s.events.iter().map(|e| e.id).collect::<HashSet<_>>().len() == s.events.len()
        && s.episodes
            .iter()
            .map(|e| e.id)
            .collect::<HashSet<_>>()
            .len()
            == s.episodes.len()
        && s.entities
            .entities
            .iter()
            .map(|e| e.id)
            .collect::<HashSet<_>>()
            .len()
            == s.entities.entities.len()
}

fn build_export(input: &CompleteExportInput, s: Sections) -> UserMemoryExport {
    let counts = ExportSectionCounts {
        facts: s.facts.len(),
        memory_events: s.events.len(),
        episodes: s.episodes.len(),
        entities: s.entities.entities.len(),
        entity_links: s.entities.links.len(),
        entity_relations: s.entities.relations.len(),
    };
    let tenant: &TenantContext = &input.tenant;
    UserMemoryExport::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        s.facts,
        s.events,
    )
    .with_episodes(s.episodes)
    .with_entities(s.entities)
    .with_completeness(ExportCompleteness {
        mode: EXPORT_COMPLETE_MODE.to_string(),
        include_deleted: input.include_deleted,
        page_size: input.page_size,
        verified_reads: 2,
        counts,
    })
}
