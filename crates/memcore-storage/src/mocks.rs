use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{Fact, TenantContext};
use serde_json::Value;
use uuid::Uuid;

use crate::keyword_search::event_matches_keyword;
use memcore_core::pagination::{is_after_cursor_in_desc_order, page_fetch_limit};
use memcore_core::ports::{
    ApiKeyListQuery, ApiKeyStore, DedupInsert, FactFulltextQuery, FactSearchQuery, FactStore,
    MemoryEventQuery, MemoryEventStore, OrgMemoryEventQuery, OrgUserListQuery, OrgUserSummary,
    PointPage, PointScope, RetentionPruneResult, ScoredFact, StoredFactState, StoredPoint,
    VectorPointState, VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore,
};

use memcore_core::{ApiKeyRecord, MemoryEvent};

fn event_matches_tenant(event: &MemoryEvent, tenant: &TenantContext) -> bool {
    event.org_id == tenant.org_id && event.user_id == tenant.user_id
}

fn ensure_event_tenant(event: &MemoryEvent, tenant: &TenantContext) -> MemcoreResult<()> {
    if event_matches_tenant(event, tenant) {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn normalize_event_list_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_core::ports::{DEFAULT_MEMORY_EVENT_LIST_LIMIT, MAX_MEMORY_EVENT_LIST_LIMIT};

    if limit == 0 {
        return Ok(DEFAULT_MEMORY_EVENT_LIST_LIMIT);
    }

    if limit > MAX_MEMORY_EVENT_LIST_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_MEMORY_EVENT_LIST_LIMIT}"
        )));
    }

    Ok(limit)
}

fn tenant_key(tenant: &TenantContext) -> (String, String) {
    (tenant.org_id.clone(), tenant.user_id.clone())
}

fn fact_matches_tenant(fact: &Fact, tenant: &TenantContext) -> bool {
    fact.org_id == tenant.org_id && fact.user_id == tenant.user_id
}

fn record_matches_tenant(record: &VectorRecord, tenant: &TenantContext) -> bool {
    record.org_id == tenant.org_id && record.user_id == tenant.user_id
}

fn ensure_fact_tenant(fact: &Fact, tenant: &TenantContext) -> MemcoreResult<()> {
    if fact_matches_tenant(fact, tenant) {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn ensure_record_tenant(record: &VectorRecord, tenant: &TenantContext) -> MemcoreResult<()> {
    if record_matches_tenant(record, tenant) {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn metadata_matches(record_metadata: &Value, filter: &Value) -> bool {
    let Some(filter_obj) = filter.as_object() else {
        return false;
    };

    let Some(record_obj) = record_metadata.as_object() else {
        return false;
    };

    filter_obj
        .iter()
        .all(|(key, value)| record_obj.get(key) == Some(value))
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[derive(Debug, Default)]
pub struct MockFactStore {
    facts: RwLock<HashMap<Uuid, Fact>>,
    deleted: RwLock<HashSet<Uuid>>,
    /// The exact-dedup key of facts added by ingestion (as the SQL column).
    dedup_keys: RwLock<HashMap<Uuid, String>>,
}

impl MockFactStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn is_deleted(&self, fact_id: &Uuid) -> bool {
        self.deleted
            .read()
            .expect("deleted lock poisoned")
            .contains(fact_id)
    }
}

#[async_trait]
impl FactStore for MockFactStore {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        ensure_fact_tenant(&fact, tenant)?;

        let mut facts = self.facts.write().expect("facts lock poisoned");
        if facts.contains_key(&fact.id) {
            return Err(MemcoreError::Conflict(format!(
                "fact already exists: {}",
                fact.id
            )));
        }

        facts.insert(fact.id, fact.clone());
        Ok(fact)
    }

    async fn insert_fact_deduplicated(
        &self,
        tenant: &TenantContext,
        fact: Fact,
        dedup_key: &str,
    ) -> MemcoreResult<DedupInsert> {
        ensure_fact_tenant(&fact, tenant)?;
        // One write lock across the check and the insert: the same atomicity as the SQL index.
        let mut facts = self.facts.write().expect("facts lock poisoned");
        if facts.contains_key(&fact.id) {
            return Err(MemcoreError::Conflict(format!(
                "fact already exists: {}",
                fact.id
            )));
        }
        let deleted = self.deleted.read().expect("deleted lock poisoned");
        let mut keys = self.dedup_keys.write().expect("dedup keys lock poisoned");
        let holder = keys.iter().find_map(|(id, key)| {
            let held = key == dedup_key
                && !deleted.contains(id)
                && facts
                    .get(id)
                    .is_some_and(|f| fact_matches_tenant(f, tenant) && f.is_active());
            held.then_some(*id)
        });
        if let Some(existing_fact_id) = holder {
            return Ok(DedupInsert::Duplicate { existing_fact_id });
        }
        keys.insert(fact.id, dedup_key.to_string());
        facts.insert(fact.id, fact.clone());
        Ok(DedupInsert::Inserted(fact))
    }

    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        ensure_fact_tenant(&fact, tenant)?;

        let mut facts = self.facts.write().expect("facts lock poisoned");
        let existing = facts
            .get(&fact.id)
            .ok_or_else(|| MemcoreError::NotFound(format!("fact not found: {}", fact.id)))?;

        if !fact_matches_tenant(existing, tenant) {
            return Err(MemcoreError::Forbidden);
        }

        // An update never keeps an exact-dedup key (as the SQL stores).
        self.dedup_keys
            .write()
            .expect("dedup keys lock poisoned")
            .remove(&fact.id);
        facts.insert(fact.id, fact.clone());
        Ok(fact)
    }

    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let Some(fact) = facts.get(&fact_id) else {
            return Ok(None);
        };

        if !fact_matches_tenant(fact, tenant) {
            return Ok(None);
        }

        if self.is_deleted(&fact_id) {
            return Ok(None);
        }

        Ok(Some(fact.clone()))
    }

    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let Some(fact) = facts.get(&fact_id) else {
            return Ok(None);
        };
        if !fact_matches_tenant(fact, tenant) {
            return Ok(None);
        }
        Ok(Some(StoredFactState {
            fact: fact.clone(),
            deleted: self.is_deleted(&fact_id),
        }))
    }

    async fn list_fact_states(
        &self,
        tenant: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let mut rows: Vec<&Fact> = facts
            .values()
            .filter(|fact| fact_matches_tenant(fact, tenant))
            .filter(|fact| after.is_none_or(|after| fact.id > after))
            .collect();
        rows.sort_by_key(|fact| fact.id);
        Ok(rows
            .into_iter()
            .take(limit)
            .map(|fact| StoredFactState {
                fact: fact.clone(),
                deleted: self.is_deleted(&fact.id),
            })
            .collect())
    }

    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool> {
        Ok(self
            .facts
            .read()
            .expect("facts lock poisoned")
            .contains_key(&fact_id))
    }

    async fn search_facts(&self, query: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted = self.deleted.read().expect("deleted lock poisoned");

        let mut results: Vec<Fact> = facts
            .values()
            .filter(|fact| fact_matches_tenant(fact, &query.tenant))
            .filter(|fact| {
                if query.include_deleted {
                    true
                } else {
                    !deleted.contains(&fact.id)
                }
            })
            .filter(|fact| {
                query
                    .memory_types
                    .as_ref()
                    .is_none_or(|types| types.contains(&fact.memory_type))
            })
            .filter(|fact| {
                query.query_text.as_ref().is_none_or(|text| {
                    let needle = text.to_ascii_lowercase();
                    fact.content.to_ascii_lowercase().contains(&needle)
                        || fact
                            .summary
                            .as_ref()
                            .is_some_and(|summary| summary.to_ascii_lowercase().contains(&needle))
                })
            })
            .filter(|fact| {
                query.cursor.as_ref().is_none_or(|cursor| {
                    is_after_cursor_in_desc_order(fact.updated_at, &fact.id.to_string(), cursor)
                })
            })
            .cloned()
            .collect();

        results.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        results.truncate(page_fetch_limit(query.limit));
        Ok(results)
    }

    /// Term overlap over content, subject and time expression; ties in the same stable order as the
    /// SQL stores: content, source message ids, subject, then id.
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>> {
        if query.query.is_empty() {
            return Ok(Vec::new());
        }
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted = self.deleted.read().expect("deleted lock poisoned");
        let mut results: Vec<ScoredFact> = facts
            .values()
            .filter(|fact| fact_matches_tenant(fact, &query.tenant))
            .filter(|fact| !deleted.contains(&fact.id))
            .filter(|fact| {
                query
                    .memory_types
                    .as_ref()
                    .is_none_or(|types| types.contains(&fact.memory_type))
            })
            .filter_map(|fact| {
                let text = format!(
                    "{} {} {}",
                    fact.content,
                    fact.structure.subject.as_deref().unwrap_or(""),
                    fact.structure.event_time_text.as_deref().unwrap_or("")
                );
                let score = query.query.overlap_score(&text);
                (score > 0).then(|| ScoredFact {
                    fact: fact.clone(),
                    score: score as f32,
                })
            })
            .collect();
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.fact.content.cmp(&b.fact.content))
                .then_with(|| {
                    a.fact
                        .structure
                        .source_message_ids
                        .cmp(&b.fact.structure.source_message_ids)
                })
                .then_with(|| a.fact.structure.subject.cmp(&b.fact.structure.subject))
                .then(a.fact.id.cmp(&b.fact.id))
        });
        results.truncate(query.limit);
        Ok(results)
    }

    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let Some(fact) = facts.get(&fact_id) else {
            return Err(MemcoreError::NotFound(format!("fact not found: {fact_id}")));
        };

        if !fact_matches_tenant(fact, tenant) {
            return Err(MemcoreError::Forbidden);
        }

        self.deleted
            .write()
            .expect("deleted lock poisoned")
            .insert(fact_id);
        Ok(())
    }

    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        let key = tenant_key(tenant);
        let mut facts = self.facts.write().expect("facts lock poisoned");
        facts.retain(|_, fact| (fact.org_id.as_str(), fact.user_id.as_str()) != (&key.0, &key.1));

        let mut deleted = self.deleted.write().expect("deleted lock poisoned");
        let remaining_ids: HashSet<Uuid> = facts.keys().copied().collect();
        deleted.retain(|id| remaining_ids.contains(id));
        Ok(())
    }

    async fn delete_facts_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        let matching_ids: Vec<Uuid> = {
            let facts = self.facts.read().expect("facts lock poisoned");
            let deleted_set = self.deleted.read().expect("deleted lock poisoned");
            facts
                .values()
                .filter(|fact| fact_matches_tenant(fact, tenant))
                .filter(|fact| !deleted_set.contains(&fact.id))
                .filter(|fact| fact.updated_at < cutoff)
                .map(|fact| fact.id)
                .collect()
        };

        if dry_run {
            return Ok(RetentionPruneResult {
                count: matching_ids.len(),
                fact_ids: Vec::new(),
            });
        }

        let mut deleted = self.deleted.write().expect("deleted lock poisoned");
        for fact_id in &matching_ids {
            deleted.insert(*fact_id);
        }

        Ok(RetentionPruneResult {
            count: matching_ids.len(),
            fact_ids: matching_ids,
        })
    }

    async fn count_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted_set = self.deleted.read().expect("deleted lock poisoned");
        Ok(facts
            .values()
            .filter(|fact| fact.org_id == org_id)
            .filter(|fact| !deleted_set.contains(&fact.id))
            .count())
    }

    async fn count_active_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted_set = self.deleted.read().expect("deleted lock poisoned");
        Ok(facts
            .values()
            .filter(|fact| fact.org_id == org_id)
            .filter(|fact| !deleted_set.contains(&fact.id))
            .filter(|fact| fact.is_active())
            .count())
    }

    async fn count_facts_by_user(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted_set = self.deleted.read().expect("deleted lock poisoned");
        Ok(facts
            .values()
            .filter(|fact| fact_matches_tenant(fact, tenant))
            .filter(|fact| !deleted_set.contains(&fact.id))
            .count())
    }

    async fn count_users_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted_set = self.deleted.read().expect("deleted lock poisoned");
        let mut users = HashSet::new();
        for fact in facts.values() {
            if fact.org_id == org_id && !deleted_set.contains(&fact.id) {
                users.insert(fact.user_id.clone());
            }
        }
        Ok(users.len())
    }

    async fn list_users_by_org(
        &self,
        query: OrgUserListQuery,
    ) -> MemcoreResult<Vec<OrgUserSummary>> {
        let facts = self.facts.read().expect("facts lock poisoned");
        let deleted_set = self.deleted.read().expect("deleted lock poisoned");

        let mut aggregates: HashMap<String, (usize, Option<DateTime<Utc>>)> = HashMap::new();
        for fact in facts.values() {
            if fact.org_id != query.org_id || deleted_set.contains(&fact.id) {
                continue;
            }
            let entry = aggregates.entry(fact.user_id.clone()).or_insert((0, None));
            entry.0 += 1;
            entry.1 = Some(match entry.1 {
                Some(current) => current.max(fact.updated_at),
                None => fact.updated_at,
            });
        }

        let mut users: Vec<OrgUserSummary> = aggregates
            .into_iter()
            .map(|(user_id, (memory_count, last_memory_at))| OrgUserSummary {
                user_id,
                memory_count,
                last_memory_at,
            })
            .filter(|user| {
                query.cursor.as_ref().is_none_or(|cursor| {
                    let sort_value = user.last_memory_at.unwrap_or(DateTime::<Utc>::MIN_UTC);
                    is_after_cursor_in_desc_order(sort_value, &user.user_id, cursor)
                })
            })
            .collect();

        users.sort_by(|a, b| {
            let a_sort = a.last_memory_at.unwrap_or(DateTime::<Utc>::MIN_UTC);
            let b_sort = b.last_memory_at.unwrap_or(DateTime::<Utc>::MIN_UTC);
            b_sort.cmp(&a_sort).then_with(|| b.user_id.cmp(&a.user_id))
        });
        users.truncate(page_fetch_limit(query.limit));
        Ok(users)
    }
}

#[derive(Debug, Default)]
pub struct MockVectorStore {
    records: RwLock<HashMap<Uuid, VectorRecord>>,
}

impl MockVectorStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MockVectorStore {
    /// Test support: stores `record` as is, WITHOUT the tenant check every
    /// write makes, so tests can build damaged or foreign points (missing tenant payload, a payload
    /// naming another tenant's id). Never used by product code.
    pub fn insert_unchecked_for_test(&self, record: VectorRecord) {
        self.records
            .write()
            .expect("records lock poisoned")
            .insert(record.id, record);
    }
}

fn point_in_scope(record: &VectorRecord, scope: &PointScope) -> bool {
    match scope {
        PointScope::Tenant(tenant) => record_matches_tenant(record, tenant),
        PointScope::TenantEntity(tenant, entity) => {
            record_matches_tenant(record, tenant) && record.fact_id == *entity
        }
        PointScope::Unattributed => {
            record.org_id.trim().is_empty() || record.user_id.trim().is_empty()
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

#[async_trait]
impl VectorStore for MockVectorStore {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        ensure_record_tenant(&record, tenant)?;
        self.records
            .write()
            .expect("records lock poisoned")
            .insert(record.id, record);
        Ok(())
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        let records = self.records.read().expect("records lock poisoned");

        let mut scored: Vec<VectorSearchResult> = records
            .values()
            .filter(|record| record_matches_tenant(record, &query.tenant))
            .filter(|record| {
                query
                    .memory_types
                    .as_ref()
                    .is_none_or(|types| types.contains(&record.memory_type))
            })
            .filter(|record| {
                query
                    .metadata_filter
                    .as_ref()
                    .is_none_or(|filter| metadata_matches(&record.metadata, filter))
            })
            .map(|record| VectorSearchResult {
                fact_id: record.fact_id,
                content: record.content.clone(),
                score: cosine_similarity(&query.embedding, &record.embedding),
                memory_type: record.memory_type,
                metadata: record.metadata.clone(),
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(query.limit);
        Ok(scored)
    }

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        let mut records = self.records.write().expect("records lock poisoned");
        let before = records.len();
        records.retain(|_, record| {
            !(record_matches_tenant(record, tenant) && record.fact_id == fact_id)
        });

        if records.len() == before {
            return Err(MemcoreError::NotFound(format!(
                "vector record not found for fact: {fact_id}"
            )));
        }

        Ok(())
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        let key = tenant_key(tenant);
        self.records
            .write()
            .expect("records lock poisoned")
            .retain(|_, record| {
                (record.org_id.as_str(), record.user_id.as_str()) != (&key.0, &key.1)
            });
        Ok(())
    }

    /// Every point the tenant has for `fact_id` (the store reports points,
    /// as Qdrant does, so write verification and the retry heal run against the mock too).
    /// Point ids are the records' own ids; nothing is synthesised. Ordered by point id.
    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        let records = self.records.read().expect("records lock poisoned");
        let mut points: Vec<VectorPointState> = records
            .values()
            .filter(|record| record_matches_tenant(record, tenant) && record.fact_id == fact_id)
            .map(|record| VectorPointState {
                point_id: record.id.to_string(),
                content: record.content.clone(),
                memory_type: record.memory_type,
            })
            .collect();
        points.sort_by(|left, right| left.point_id.cmp(&right.point_id));
        Ok(Some(points))
    }

    /// Ascending point id, strictly after `cursor` (the last point id of the previous page).
    async fn scroll_points(
        &self,
        scope: &PointScope,
        cursor: Option<String>,
        limit: usize,
    ) -> MemcoreResult<PointPage> {
        let records = self.records.read().expect("records lock poisoned");
        let mut matching: Vec<&VectorRecord> = records
            .values()
            .filter(|record| point_in_scope(record, scope))
            .filter(|record| {
                cursor
                    .as_deref()
                    .is_none_or(|cursor| record.id.to_string().as_str() > cursor)
            })
            .collect();
        matching.sort_by_key(|record| record.id.to_string());
        let more = matching.len() > limit;
        let points: Vec<StoredPoint> = matching
            .into_iter()
            .take(limit)
            .map(|record| StoredPoint {
                point_id: record.id.to_string(),
                entity_id: Some(record.fact_id.to_string()),
                org_id: non_empty(&record.org_id),
                user_id: non_empty(&record.user_id),
                content: (!matches!(scope, PointScope::Unattributed))
                    .then(|| record.content.clone()),
                memory_type: Some(record.memory_type),
            })
            .collect();
        let next_cursor = if more {
            points.last().map(|point| point.point_id.clone())
        } else {
            None
        };
        Ok(PointPage {
            points,
            next_cursor,
        })
    }

    async fn count_points(&self, scope: &PointScope) -> MemcoreResult<u64> {
        Ok(self
            .records
            .read()
            .expect("records lock poisoned")
            .values()
            .filter(|record| point_in_scope(record, scope))
            .count() as u64)
    }

    async fn delete_points(
        &self,
        tenant: &TenantContext,
        point_ids: &[String],
    ) -> MemcoreResult<()> {
        self.records
            .write()
            .expect("records lock poisoned")
            .retain(|id, record| {
                !(record_matches_tenant(record, tenant) && point_ids.contains(&id.to_string()))
            });
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct MockMemoryEventStore {
    events: RwLock<Vec<MemoryEvent>>,
}

impl MockMemoryEventStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryEventStore for MockMemoryEventStore {
    async fn record_event(
        &self,
        tenant: &TenantContext,
        event: MemoryEvent,
    ) -> MemcoreResult<MemoryEvent> {
        ensure_event_tenant(&event, tenant)?;
        self.events
            .write()
            .expect("events lock poisoned")
            .push(event.clone());
        Ok(event)
    }

    async fn list_events(&self, query: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        let _ = normalize_event_list_limit(query.limit)?;
        let events = self.events.read().expect("events lock poisoned");

        let mut results: Vec<MemoryEvent> = events
            .iter()
            .filter(|event| event_matches_tenant(event, &query.tenant))
            .filter(|event| {
                query
                    .fact_id
                    .is_none_or(|fact_id| event.fact_id == Some(fact_id))
            })
            .filter(|event| {
                query
                    .operation
                    .is_none_or(|operation| event.operation == operation)
            })
            .filter(|event| {
                query
                    .created_after
                    .is_none_or(|created_after| event.created_at >= created_after)
            })
            .filter(|event| {
                query
                    .created_before
                    .is_none_or(|created_before| event.created_at < created_before)
            })
            .filter(|event| {
                query
                    .query_text
                    .as_ref()
                    .is_none_or(|text| event_matches_keyword(event, text, false))
            })
            .filter(|event| {
                query.cursor.as_ref().is_none_or(|cursor| {
                    is_after_cursor_in_desc_order(event.created_at, &event.id.to_string(), cursor)
                })
            })
            .cloned()
            .collect();

        results.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        results.truncate(page_fetch_limit(query.limit));
        Ok(results)
    }

    async fn list_events_by_org(
        &self,
        query: OrgMemoryEventQuery,
    ) -> MemcoreResult<Vec<MemoryEvent>> {
        let limit = normalize_event_list_limit(query.limit)?;
        let events = self.events.read().expect("events lock poisoned");

        let mut results: Vec<MemoryEvent> = events
            .iter()
            .filter(|event| event.org_id == query.org_id)
            .filter(|event| {
                query
                    .user_id
                    .as_ref()
                    .is_none_or(|user_id| event.user_id == *user_id)
            })
            .filter(|event| {
                query
                    .fact_id
                    .is_none_or(|fact_id| event.fact_id == Some(fact_id))
            })
            .filter(|event| {
                query
                    .operation
                    .is_none_or(|operation| event.operation == operation)
            })
            .filter(|event| {
                query
                    .created_after
                    .is_none_or(|created_after| event.created_at >= created_after)
            })
            .filter(|event| {
                query
                    .created_before
                    .is_none_or(|created_before| event.created_at < created_before)
            })
            .filter(|event| {
                query
                    .query_text
                    .as_ref()
                    .is_none_or(|text| event_matches_keyword(event, text, true))
            })
            .filter(|event| {
                query.cursor.as_ref().is_none_or(|cursor| {
                    is_after_cursor_in_desc_order(event.created_at, &event.id.to_string(), cursor)
                })
            })
            .cloned()
            .collect();

        results.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        results.truncate(page_fetch_limit(limit));
        Ok(results)
    }

    async fn delete_events_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize> {
        let mut events = self.events.write().expect("events lock poisoned");
        let before_len = events.len();

        if dry_run {
            let count = events
                .iter()
                .filter(|event| event_matches_tenant(event, tenant))
                .filter(|event| event.created_at < cutoff)
                .count();
            return Ok(count);
        }

        events.retain(|event| !(event_matches_tenant(event, tenant) && event.created_at < cutoff));

        Ok(before_len - events.len())
    }

    async fn count_events_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let events = self.events.read().expect("events lock poisoned");
        Ok(events.iter().filter(|event| event.org_id == org_id).count())
    }

    async fn erase_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let mut events = self.events.write().expect("events lock poisoned");
        let before = events.len();
        events.retain(|event| !is_user_audit_content(event, tenant));
        Ok(before - events.len())
    }

    async fn count_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let events = self.events.read().expect("events lock poisoned");
        Ok(events
            .iter()
            .filter(|event| is_user_audit_content(event, tenant))
            .count())
    }
}

/// An event of exactly this (org, user) that is not a content-free forget-user tombstone.
fn is_user_audit_content(event: &memcore_core::MemoryEvent, tenant: &TenantContext) -> bool {
    event.org_id == tenant.org_id
        && event.user_id == tenant.user_id
        && !event.is_content_free_tombstone()
}

#[derive(Debug, Default)]
pub struct MockApiKeyStore {
    keys: RwLock<Vec<ApiKeyRecord>>,
}

impl MockApiKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ApiKeyStore for MockApiKeyStore {
    async fn find_by_hash(&self, key_hash: &str) -> MemcoreResult<Option<ApiKeyRecord>> {
        let keys = self.keys.read().expect("api keys lock poisoned");
        Ok(keys
            .iter()
            .find(|record| record.key_hash == key_hash && record.is_active())
            .cloned())
    }

    async fn insert_api_key(&self, record: ApiKeyRecord) -> MemcoreResult<ApiKeyRecord> {
        self.keys
            .write()
            .expect("api keys lock poisoned")
            .push(record.clone());
        Ok(record)
    }

    async fn revoke_api_key(&self, org_id: &str, key_id: Uuid) -> MemcoreResult<()> {
        let mut keys = self.keys.write().expect("api keys lock poisoned");
        let Some(record) = keys
            .iter_mut()
            .find(|record| record.id == key_id && record.org_id == org_id && record.is_active())
        else {
            return Err(MemcoreError::NotFound(format!(
                "api key not found: {key_id}"
            )));
        };

        record.revoked_at = Some(Utc::now());
        Ok(())
    }

    async fn list_api_keys(&self, query: ApiKeyListQuery) -> MemcoreResult<Vec<ApiKeyRecord>> {
        let keys = self.keys.read().expect("api keys lock poisoned");
        let mut results: Vec<ApiKeyRecord> = keys
            .iter()
            .filter(|record| record.org_id == query.org_id)
            .filter(|record| query.include_revoked || record.is_active())
            .filter(|record| {
                query.cursor.as_ref().is_none_or(|cursor| {
                    is_after_cursor_in_desc_order(record.created_at, &record.id.to_string(), cursor)
                })
            })
            .cloned()
            .collect();
        results.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        results.truncate(page_fetch_limit(query.limit));
        Ok(results)
    }
}

/// In-memory ingest journal with the SQL stores' semantics (tests).
#[derive(Debug, Default)]
pub struct MockIngestJournalStore {
    operations: RwLock<HashMap<Uuid, memcore_core::ports::IngestOperation>>,
    receipts: RwLock<HashMap<(String, String, String), memcore_core::ports::IngestReceipt>>,
}

impl MockIngestJournalStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every operation in the journal (tests).
    pub fn operations(&self) -> Vec<memcore_core::ports::IngestOperation> {
        self.operations.read().unwrap().values().cloned().collect()
    }

    /// Sets an operation's `updated_at` (tests: make it stale).
    pub fn set_updated_at(&self, operation_id: Uuid, updated_at: DateTime<Utc>) {
        if let Some(operation) = self.operations.write().unwrap().get_mut(&operation_id) {
            operation.updated_at = updated_at;
        }
    }
}

fn mock_claimed_conflict() -> MemcoreError {
    MemcoreError::Conflict(
        "the write was rolled back by recovery before it completed; retry the request".to_string(),
    )
}

fn mock_receipt_key(tenant: &TenantContext, request_key: &str) -> (String, String, String) {
    (
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        request_key.to_string(),
    )
}

fn mock_in_tenant(
    operation: &memcore_core::ports::IngestOperation,
    tenant: &TenantContext,
) -> bool {
    operation.org_id == tenant.org_id && operation.user_id == tenant.user_id
}

#[async_trait]
impl memcore_core::ports::IngestJournalStore for MockIngestJournalStore {
    async fn begin(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        step: &memcore_core::ports::IngestJournalStep,
    ) -> MemcoreResult<()> {
        let now = Utc::now();
        let mut operations = self.operations.write().unwrap();
        if operations.contains_key(&operation_id) {
            return Err(MemcoreError::StorageError(
                "ingest journal operation already exists".to_string(),
            ));
        }
        operations.insert(
            operation_id,
            memcore_core::ports::IngestOperation {
                id: operation_id,
                org_id: tenant.org_id.clone(),
                user_id: tenant.user_id.clone(),
                status: memcore_core::ports::IngestOperationStatus::InProgress,
                attempts: 0,
                created_at: now,
                updated_at: now,
                steps: vec![step.clone()],
            },
        );
        Ok(())
    }

    async fn append(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        step: &memcore_core::ports::IngestJournalStep,
    ) -> MemcoreResult<()> {
        let mut operations = self.operations.write().unwrap();
        match operations.get_mut(&operation_id) {
            Some(operation)
                if mock_in_tenant(operation, tenant)
                    && operation.status
                        == memcore_core::ports::IngestOperationStatus::InProgress =>
            {
                operation.updated_at = Utc::now();
                operation.steps.push(step.clone());
                Ok(())
            }
            _ => Err(mock_claimed_conflict()),
        }
    }

    async fn commit(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        receipt: Option<&memcore_core::ports::IngestReceipt>,
    ) -> MemcoreResult<()> {
        let mut operations = self.operations.write().unwrap();
        match operations.get(&operation_id) {
            Some(operation)
                if mock_in_tenant(operation, tenant)
                    && operation.status
                        == memcore_core::ports::IngestOperationStatus::InProgress =>
            {
                operations.remove(&operation_id);
                if let Some(receipt) = receipt {
                    self.receipts
                        .write()
                        .unwrap()
                        .entry(mock_receipt_key(tenant, &receipt.request_key))
                        .or_insert_with(|| receipt.clone());
                }
                Ok(())
            }
            _ => Err(mock_claimed_conflict()),
        }
    }

    async fn record_receipt(
        &self,
        tenant: &TenantContext,
        receipt: &memcore_core::ports::IngestReceipt,
    ) -> MemcoreResult<()> {
        self.receipts
            .write()
            .unwrap()
            .entry(mock_receipt_key(tenant, &receipt.request_key))
            .or_insert_with(|| receipt.clone());
        Ok(())
    }

    async fn find_receipt(
        &self,
        tenant: &TenantContext,
        request_key: &str,
    ) -> MemcoreResult<Option<memcore_core::ports::IngestReceipt>> {
        Ok(self
            .receipts
            .read()
            .unwrap()
            .get(&mock_receipt_key(tenant, request_key))
            .cloned())
    }

    async fn purge_receipts(&self, committed_before: DateTime<Utc>) -> MemcoreResult<usize> {
        let mut receipts = self.receipts.write().unwrap();
        let before = receipts.len();
        receipts.retain(|_, receipt| receipt.committed_at >= committed_before);
        Ok(before - receipts.len())
    }

    async fn claim(
        &self,
        operation_id: Uuid,
        stale_before: Option<DateTime<Utc>>,
    ) -> MemcoreResult<Option<memcore_core::ports::IngestOperation>> {
        let mut operations = self.operations.write().unwrap();
        let Some(operation) = operations.get_mut(&operation_id) else {
            return Ok(None);
        };
        let claimable = match stale_before {
            None => operation.status == memcore_core::ports::IngestOperationStatus::InProgress,
            Some(cutoff) => operation.updated_at < cutoff,
        };
        if !claimable {
            return Ok(None);
        }
        operation.status = memcore_core::ports::IngestOperationStatus::RollingBack;
        operation.attempts += 1;
        operation.updated_at = Utc::now();
        Ok(Some(operation.clone()))
    }

    async fn finish(&self, operation_id: Uuid) -> MemcoreResult<()> {
        self.operations.write().unwrap().remove(&operation_id);
        Ok(())
    }

    async fn release(&self, operation_id: Uuid) -> MemcoreResult<()> {
        if let Some(operation) = self.operations.write().unwrap().get_mut(&operation_id) {
            operation.updated_at = Utc::now();
        }
        Ok(())
    }

    async fn list_stale(
        &self,
        stale_before: DateTime<Utc>,
        limit: usize,
    ) -> MemcoreResult<Vec<Uuid>> {
        let operations = self.operations.read().unwrap();
        let mut stale: Vec<_> = operations
            .values()
            .filter(|operation| operation.updated_at < stale_before)
            .map(|operation| (operation.updated_at, operation.id))
            .collect();
        stale.sort();
        Ok(stale.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    async fn find_in_flight(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
        exclude: Uuid,
    ) -> MemcoreResult<Option<Uuid>> {
        let operations = self.operations.read().unwrap();
        Ok(operations
            .values()
            .find(|operation| {
                operation.id != exclude
                    && mock_in_tenant(operation, tenant)
                    && operation
                        .steps
                        .iter()
                        .any(|step| step.fact_ids().contains(&fact_id))
            })
            .map(|operation| operation.id))
    }

    async fn operation_state(
        &self,
        operation_id: Uuid,
    ) -> MemcoreResult<Option<memcore_core::ports::IngestOperationState>> {
        Ok(self
            .operations
            .read()
            .unwrap()
            .get(&operation_id)
            .map(|operation| memcore_core::ports::IngestOperationState {
                status: operation.status,
                updated_at: operation.updated_at,
            }))
    }

    async fn count_operations(&self) -> MemcoreResult<usize> {
        Ok(self.operations.read().unwrap().len())
    }

    async fn backlog(&self) -> MemcoreResult<memcore_core::ports::IngestJournalBacklog> {
        let operations = self.operations.read().unwrap();
        Ok(memcore_core::ports::IngestJournalBacklog {
            pending: operations.len(),
            rolling_back: operations
                .values()
                .filter(|o| o.status == memcore_core::ports::IngestOperationStatus::RollingBack)
                .count(),
            oldest_updated_at: operations.values().map(|o| o.updated_at).min(),
            max_attempts: operations.values().map(|o| o.attempts).max().unwrap_or(0),
        })
    }

    async fn delete_user_operations(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.receipts
            .write()
            .unwrap()
            .retain(|(org, user, _), _| *org != tenant.org_id || *user != tenant.user_id);
        let mut operations = self.operations.write().unwrap();
        let before = operations.len();
        operations.retain(|_, operation| !mock_in_tenant(operation, tenant));
        Ok(before - operations.len())
    }
}

#[cfg(test)]
mod api_key_store_tests {
    use chrono::Utc;
    use memcore_common::hash_api_key;
    use memcore_core::ApiKeyScope;
    use uuid::Uuid;

    use super::MockApiKeyStore;
    use crate::traits::ApiKeyStore;
    use memcore_core::{ApiKeyListQuery, ApiKeyRecord};

    #[tokio::test]
    async fn mock_list_api_keys_by_org() {
        let store = MockApiKeyStore::new();
        let active = ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: "org_a".to_string(),
            name: "active".to_string(),
            key_hash: hash_api_key("pepper", "active"),
            scopes: vec![ApiKeyScope::MemoryRead],
            created_at: Utc::now(),
            revoked_at: None,
        };
        let revoked = ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: "org_a".to_string(),
            name: "revoked".to_string(),
            key_hash: hash_api_key("pepper", "revoked"),
            scopes: vec![ApiKeyScope::MemoryRead],
            created_at: Utc::now(),
            revoked_at: Some(Utc::now()),
        };
        let other_org = ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: "org_b".to_string(),
            name: "other".to_string(),
            key_hash: hash_api_key("pepper", "other"),
            scopes: vec![ApiKeyScope::MemoryRead],
            created_at: Utc::now(),
            revoked_at: None,
        };

        store.insert_api_key(active).await.expect("insert");
        store.insert_api_key(revoked).await.expect("insert");
        store.insert_api_key(other_org).await.expect("insert");

        let active_only = store
            .list_api_keys(ApiKeyListQuery {
                org_id: "org_a".to_string(),
                include_revoked: false,
                limit: 100,
                cursor: None,
            })
            .await
            .expect("list");
        assert_eq!(active_only.len(), 1);
        assert_eq!(active_only[0].name, "active");

        let with_revoked = store
            .list_api_keys(ApiKeyListQuery {
                org_id: "org_a".to_string(),
                include_revoked: true,
                limit: 100,
                cursor: None,
            })
            .await
            .expect("list");
        assert_eq!(with_revoked.len(), 2);

        let org_b = store
            .list_api_keys(ApiKeyListQuery {
                org_id: "org_b".to_string(),
                include_revoked: false,
                limit: 100,
                cursor: None,
            })
            .await
            .expect("list");
        assert_eq!(org_b.len(), 1);
        assert_eq!(org_b[0].org_id, "org_b");
    }

    #[tokio::test]
    async fn mock_api_key_store_insert_find_revoke() {
        let store = MockApiKeyStore::new();
        let record = ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: "org_mock".to_string(),
            name: "mock".to_string(),
            key_hash: hash_api_key("pepper", "token"),
            scopes: vec![ApiKeyScope::MemoryRead],
            created_at: Utc::now(),
            revoked_at: None,
        };

        store
            .insert_api_key(record.clone())
            .await
            .expect("insert should succeed");
        let found = store
            .find_by_hash(&record.key_hash)
            .await
            .expect("find should succeed")
            .expect("record should exist");
        assert_eq!(found.id, record.id);

        store
            .revoke_api_key("org_mock", record.id)
            .await
            .expect("revoke should succeed");
        assert!(
            store
                .find_by_hash(&record.key_hash)
                .await
                .expect("find should succeed")
                .is_none()
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use memcore_core::{MemorySource, MemoryType, OrgUserListQuery};
    use serde_json::json;
    use uuid::Uuid;

    use super::{MockFactStore, MockVectorStore};
    use crate::queries::FactSearchQuery;
    use crate::traits::{FactStore, VectorStore};
    use crate::vector::{VectorRecord, VectorSearchQuery};

    fn tenant(org_id: &str, user_id: &str) -> memcore_core::TenantContext {
        memcore_core::TenantContext::new(org_id, user_id).expect("tenant should be valid")
    }

    fn sample_fact(
        org_id: &str,
        user_id: &str,
        content: &str,
        memory_type: MemoryType,
    ) -> memcore_core::Fact {
        let now = Utc::now();
        memcore_core::Fact::new(
            Uuid::new_v4(),
            org_id,
            user_id,
            memory_type,
            content,
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            now,
            now,
            json!({}),
        )
        .expect("fact should be valid")
    }

    fn sample_vector(
        org_id: &str,
        user_id: &str,
        fact_id: Uuid,
        embedding: Vec<f32>,
        content: &str,
        memory_type: MemoryType,
    ) -> VectorRecord {
        VectorRecord {
            id: Uuid::new_v4(),
            fact_id,
            org_id: org_id.to_string(),
            user_id: user_id.to_string(),
            embedding,
            content: content.to_string(),
            memory_type,
            metadata: json!({ "topic": "rust" }),
        }
    }

    #[tokio::test]
    async fn insert_and_get_fact() {
        let store = MockFactStore::new();
        let tenant = tenant("org_a", "user_a");
        let fact = sample_fact("org_a", "user_a", "learning rust", MemoryType::Skill);

        let inserted = store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");
        assert_eq!(inserted.id, fact.id);

        let fetched = store
            .get_fact(&tenant, fact.id)
            .await
            .expect("get should succeed")
            .expect("fact should exist");
        assert_eq!(fetched.content, "learning rust");
    }

    #[tokio::test]
    async fn tenant_isolation_for_facts() {
        let store = MockFactStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");
        let fact = sample_fact("org_a", "user_a", "private memory", MemoryType::Profile);

        store
            .insert_fact(&tenant_a, fact.clone())
            .await
            .expect("insert should succeed");

        let cross_tenant = store
            .get_fact(&tenant_b, fact.id)
            .await
            .expect("get should succeed");
        assert!(cross_tenant.is_none());
    }

    #[tokio::test]
    async fn search_facts_by_tenant() {
        let store = MockFactStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_b", "user_b");

        store
            .insert_fact(
                &tenant_a,
                sample_fact("org_a", "user_a", "rust backend", MemoryType::Skill),
            )
            .await
            .expect("insert should succeed");
        store
            .insert_fact(
                &tenant_b,
                sample_fact("org_b", "user_b", "rust backend", MemoryType::Skill),
            )
            .await
            .expect("insert should succeed");

        let query = FactSearchQuery {
            tenant: tenant_a,
            memory_types: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        };

        let results = store
            .search_facts(query)
            .await
            .expect("search should succeed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn search_facts_by_keyword_is_case_insensitive() {
        let store = MockFactStore::new();
        let tenant = tenant("org_a", "user_a");
        store
            .insert_fact(
                &tenant,
                sample_fact("org_a", "user_a", "RUST backend", MemoryType::Skill),
            )
            .await
            .expect("insert");

        let results = store
            .search_facts(FactSearchQuery {
                tenant,
                memory_types: None,
                query_text: Some("rust".to_string()),
                limit: 10,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("search");

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_facts_by_keyword_matches_summary() {
        let store = MockFactStore::new();
        let tenant = tenant("org_a", "user_a");
        let mut fact = sample_fact("org_a", "user_a", "body text", MemoryType::Profile);
        fact.summary = Some("rust summary".to_string());
        store.insert_fact(&tenant, fact).await.expect("insert");

        let results = store
            .search_facts(FactSearchQuery {
                tenant,
                memory_types: None,
                query_text: Some("summary".to_string()),
                limit: 10,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("search");

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_facts_keyword_respects_memory_type_filter() {
        let store = MockFactStore::new();
        let tenant = tenant("org_a", "user_a");
        store
            .insert_fact(
                &tenant,
                sample_fact("org_a", "user_a", "rust skill", MemoryType::Skill),
            )
            .await
            .expect("insert");
        store
            .insert_fact(
                &tenant,
                sample_fact("org_a", "user_a", "rust profile", MemoryType::Profile),
            )
            .await
            .expect("insert");

        let results = store
            .search_facts(FactSearchQuery {
                tenant,
                memory_types: Some(vec![MemoryType::Profile]),
                query_text: Some("rust".to_string()),
                limit: 10,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_type, MemoryType::Profile);
    }

    #[tokio::test]
    async fn soft_delete_fact_hides_from_get() {
        let store = MockFactStore::new();
        let tenant = tenant("org_a", "user_a");
        let fact = sample_fact("org_a", "user_a", "to delete", MemoryType::Task);

        store
            .insert_fact(&tenant, fact.clone())
            .await
            .expect("insert should succeed");
        store
            .soft_delete_fact(&tenant, fact.id)
            .await
            .expect("soft delete should succeed");

        let fetched = store
            .get_fact(&tenant, fact.id)
            .await
            .expect("get should succeed");
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn retention_dry_run_counts_old_facts_without_deleting() {
        let store = MockFactStore::new();
        let tenant = tenant("org_a", "user_a");
        let cutoff = Utc::now() - chrono::Duration::days(30);

        let mut old_fact = sample_fact("org_a", "user_a", "old", MemoryType::Profile);
        old_fact.updated_at = Utc::now() - chrono::Duration::days(60);
        store
            .insert_fact(&tenant, old_fact)
            .await
            .expect("insert old fact");

        store
            .insert_fact(
                &tenant,
                sample_fact("org_a", "user_a", "recent", MemoryType::Profile),
            )
            .await
            .expect("insert recent fact");

        let result = store
            .delete_facts_older_than(&tenant, cutoff, true)
            .await
            .expect("dry-run should succeed");

        assert_eq!(result.count, 1);
        assert!(result.fact_ids.is_empty());

        let listed = store
            .search_facts(FactSearchQuery::new(tenant, 10))
            .await
            .expect("search");
        assert_eq!(listed.len(), 2);
    }

    #[tokio::test]
    async fn retention_apply_soft_deletes_only_matching_tenant() {
        let store = MockFactStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");
        let cutoff = Utc::now() - chrono::Duration::days(30);

        let mut old_a = sample_fact("org_a", "user_a", "old a", MemoryType::Profile);
        old_a.updated_at = Utc::now() - chrono::Duration::days(60);
        store.insert_fact(&tenant_a, old_a).await.expect("insert");

        let mut old_b = sample_fact("org_a", "user_b", "old b", MemoryType::Profile);
        old_b.updated_at = Utc::now() - chrono::Duration::days(60);
        store.insert_fact(&tenant_b, old_b).await.expect("insert");

        let result = store
            .delete_facts_older_than(&tenant_a, cutoff, false)
            .await
            .expect("apply should succeed");

        assert_eq!(result.count, 1);

        let remaining_b = store
            .search_facts(FactSearchQuery::new(tenant_b, 10))
            .await
            .expect("search b");
        assert_eq!(remaining_b.len(), 1);
    }

    #[tokio::test]
    async fn delete_user_data_removes_tenant_facts() {
        let store = MockFactStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");

        store
            .insert_fact(
                &tenant_a,
                sample_fact("org_a", "user_a", "user a memory", MemoryType::Profile),
            )
            .await
            .expect("insert should succeed");
        store
            .insert_fact(
                &tenant_b,
                sample_fact("org_a", "user_b", "user b memory", MemoryType::Profile),
            )
            .await
            .expect("insert should succeed");

        store
            .delete_user_data(&tenant_a)
            .await
            .expect("delete user data should succeed");

        let query_a = FactSearchQuery::new(tenant_a, 10);
        let query_b = FactSearchQuery::new(tenant_b, 10);

        assert!(
            store
                .search_facts(query_a)
                .await
                .expect("search")
                .is_empty()
        );
        assert_eq!(store.search_facts(query_b).await.expect("search").len(), 1);
    }

    #[tokio::test]
    async fn upsert_and_search_vector() {
        let store = MockVectorStore::new();
        let tenant = tenant("org_a", "user_a");
        let fact_id = Uuid::new_v4();
        let record = sample_vector(
            "org_a",
            "user_a",
            fact_id,
            vec![1.0, 0.0, 0.0],
            "rust vector",
            MemoryType::Skill,
        );

        store
            .upsert_vector(&tenant, record)
            .await
            .expect("upsert should succeed");

        let query = VectorSearchQuery {
            tenant,
            embedding: vec![1.0, 0.0, 0.0],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        };

        let results = store
            .search_vectors(query)
            .await
            .expect("search should succeed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact_id, fact_id);
        assert!(results[0].score > 0.99);
    }

    #[tokio::test]
    async fn tenant_isolation_for_vectors() {
        let store = MockVectorStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");
        let fact_id = Uuid::new_v4();

        store
            .upsert_vector(
                &tenant_a,
                sample_vector(
                    "org_a",
                    "user_a",
                    fact_id,
                    vec![0.0, 1.0, 0.0],
                    "private vector",
                    MemoryType::Preference,
                ),
            )
            .await
            .expect("upsert should succeed");

        let query = VectorSearchQuery {
            tenant: tenant_b,
            embedding: vec![0.0, 1.0, 0.0],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        };

        let results = store
            .search_vectors(query)
            .await
            .expect("search should succeed");
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn delete_vector_by_fact_id() {
        let store = MockVectorStore::new();
        let tenant = tenant("org_a", "user_a");
        let fact_id = Uuid::new_v4();
        let record = sample_vector(
            "org_a",
            "user_a",
            fact_id,
            vec![1.0, 1.0, 0.0],
            "delete me",
            MemoryType::Entity,
        );

        store
            .upsert_vector(&tenant, record)
            .await
            .expect("upsert should succeed");
        store
            .delete_by_fact_id(&tenant, fact_id)
            .await
            .expect("delete by fact id should succeed");

        let query = VectorSearchQuery {
            tenant,
            embedding: vec![1.0, 1.0, 0.0],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        };

        assert!(
            store
                .search_vectors(query)
                .await
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_vectors_by_user() {
        let store = MockVectorStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");

        store
            .upsert_vector(
                &tenant_a,
                sample_vector(
                    "org_a",
                    "user_a",
                    Uuid::new_v4(),
                    vec![1.0, 0.0],
                    "a",
                    MemoryType::System,
                ),
            )
            .await
            .expect("upsert should succeed");
        store
            .upsert_vector(
                &tenant_b,
                sample_vector(
                    "org_a",
                    "user_b",
                    Uuid::new_v4(),
                    vec![0.0, 1.0],
                    "b",
                    MemoryType::System,
                ),
            )
            .await
            .expect("upsert should succeed");

        store
            .delete_by_user(&tenant_a)
            .await
            .expect("delete by user should succeed");

        let query_a = VectorSearchQuery {
            tenant: tenant_a,
            embedding: vec![1.0, 0.0],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        };
        let query_b = VectorSearchQuery {
            tenant: tenant_b,
            embedding: vec![0.0, 1.0],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        };

        assert!(
            store
                .search_vectors(query_a)
                .await
                .expect("search")
                .is_empty()
        );
        assert_eq!(
            store.search_vectors(query_b).await.expect("search").len(),
            1
        );
    }

    #[tokio::test]
    async fn memory_event_store_enforces_tenant_on_record() {
        use super::MockMemoryEventStore;
        use crate::traits::MemoryEventStore;
        use memcore_common::MemcoreError;
        use memcore_core::MemoryEventOperation;

        let store = MockMemoryEventStore::new();
        let tenant_a = tenant("org_a", "user_a");
        let tenant_b = tenant("org_a", "user_b");
        let event = memcore_core::MemoryEvent::new(
            tenant_a.org_id.clone(),
            tenant_a.user_id.clone(),
            None,
            MemoryEventOperation::Add,
            None,
            Some("content".to_string()),
            None,
            None,
            json!({}),
        );

        store
            .record_event(&tenant_a, event.clone())
            .await
            .expect("record should succeed");

        let error = store
            .record_event(&tenant_b, event)
            .await
            .expect_err("cross-tenant record should fail");
        assert_eq!(error, MemcoreError::Forbidden);
    }

    #[tokio::test]
    async fn org_admin_mock_counts_are_org_scoped() {
        let store = MockFactStore::new();
        let tenant_a = tenant("org_mock_admin", "user_a");
        let tenant_b = tenant("org_mock_admin", "user_b");
        let other_org = tenant("org_other", "user_x");

        store
            .insert_fact(
                &tenant_a,
                sample_fact("org_mock_admin", "user_a", "one", MemoryType::Profile),
            )
            .await
            .expect("insert");
        store
            .insert_fact(
                &tenant_b,
                sample_fact("org_mock_admin", "user_b", "two", MemoryType::Profile),
            )
            .await
            .expect("insert");
        store
            .insert_fact(
                &other_org,
                sample_fact("org_other", "user_x", "other", MemoryType::Profile),
            )
            .await
            .expect("insert");

        assert_eq!(
            store
                .count_facts_by_org("org_mock_admin")
                .await
                .expect("count facts"),
            2
        );
        assert_eq!(
            store
                .count_users_by_org("org_mock_admin")
                .await
                .expect("count users"),
            2
        );

        let users = store
            .list_users_by_org(OrgUserListQuery {
                org_id: "org_mock_admin".to_string(),
                limit: 1,
                cursor: None,
            })
            .await
            .expect("list users");
        // Store over-fetches by one for cursor pagination; engine trims to `limit`.
        assert_eq!(users.len(), 2);
    }

    #[tokio::test]
    async fn mock_count_facts_by_user_excludes_deleted_other_users_and_orgs() {
        let store = MockFactStore::new();
        let tenant_a = tenant("org_count_user", "user_a");
        let tenant_b = tenant("org_count_user", "user_b");
        let other_org = tenant("org_other_count", "user_a");

        let deleted_fact = store
            .insert_fact(
                &tenant_a,
                sample_fact("org_count_user", "user_a", "deleted", MemoryType::Profile),
            )
            .await
            .expect("insert deleted");
        store
            .insert_fact(
                &tenant_a,
                sample_fact("org_count_user", "user_a", "active", MemoryType::Profile),
            )
            .await
            .expect("insert active");
        store
            .insert_fact(
                &tenant_b,
                sample_fact(
                    "org_count_user",
                    "user_b",
                    "other user",
                    MemoryType::Profile,
                ),
            )
            .await
            .expect("insert other user");
        store
            .insert_fact(
                &other_org,
                sample_fact(
                    "org_other_count",
                    "user_a",
                    "other org",
                    MemoryType::Profile,
                ),
            )
            .await
            .expect("insert other org");
        store
            .soft_delete_fact(&tenant_a, deleted_fact.id)
            .await
            .expect("delete");

        assert_eq!(
            store
                .count_facts_by_user(&tenant_a)
                .await
                .expect("count user"),
            1
        );
    }

    #[tokio::test]
    async fn org_audit_mock_list_events_by_org() {
        use super::MockMemoryEventStore;
        use crate::traits::MemoryEventStore;
        use memcore_core::MemoryEventOperation;
        use memcore_core::ports::OrgMemoryEventQuery;

        let store = MockMemoryEventStore::new();
        let tenant_a = tenant("org_mock_audit", "user_a");
        let tenant_b = tenant("org_mock_audit", "user_b");
        let other_org = tenant("org_other", "user_x");
        let fact_id = Uuid::new_v4();

        store
            .record_event(
                &tenant_a,
                memcore_core::MemoryEvent::new(
                    tenant_a.org_id.clone(),
                    tenant_a.user_id.clone(),
                    Some(fact_id),
                    MemoryEventOperation::Update,
                    None,
                    None,
                    None,
                    None,
                    serde_json::json!({}),
                ),
            )
            .await
            .expect("record");
        store
            .record_event(
                &tenant_b,
                memcore_core::MemoryEvent::new(
                    tenant_b.org_id.clone(),
                    tenant_b.user_id.clone(),
                    None,
                    MemoryEventOperation::Add,
                    None,
                    None,
                    None,
                    None,
                    serde_json::json!({}),
                ),
            )
            .await
            .expect("record");
        store
            .record_event(
                &other_org,
                memcore_core::MemoryEvent::new(
                    other_org.org_id.clone(),
                    other_org.user_id.clone(),
                    None,
                    MemoryEventOperation::Add,
                    None,
                    None,
                    None,
                    None,
                    serde_json::json!({}),
                ),
            )
            .await
            .expect("record");

        let events = store
            .list_events_by_org(OrgMemoryEventQuery::new("org_mock_audit".to_string(), 10))
            .await
            .expect("list");
        assert_eq!(events.len(), 2);
    }
}

/// In-memory episode storage with the same idempotency rule as the SQL stores: one row per
/// organization, user, session (none counts as one) and client message ID.
#[derive(Debug, Default)]
pub struct MockEpisodeStore {
    episodes: RwLock<Vec<memcore_core::Episode>>,
}

impl MockEpisodeStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn same_client_message(left: &memcore_core::Episode, right: &memcore_core::Episode) -> bool {
    left.message_id.is_some()
        && left.org_id == right.org_id
        && left.user_id == right.user_id
        && left.session_id.as_deref().unwrap_or("") == right.session_id.as_deref().unwrap_or("")
        && left.message_id == right.message_id
}

fn episode_in_tenant(episode: &memcore_core::Episode, tenant: &TenantContext) -> bool {
    episode.org_id == tenant.org_id && episode.user_id == tenant.user_id
}

#[async_trait]
impl memcore_core::ports::EpisodeStore for MockEpisodeStore {
    async fn insert_episodes(
        &self,
        tenant: &TenantContext,
        episodes: Vec<memcore_core::Episode>,
    ) -> MemcoreResult<Vec<memcore_core::Episode>> {
        let mut stored_rows = self.episodes.write().expect("episodes lock poisoned");
        let mut out = Vec::with_capacity(episodes.len());
        for episode in episodes {
            if !episode_in_tenant(&episode, tenant) {
                return Err(MemcoreError::Forbidden);
            }
            if let Some(existing) = stored_rows
                .iter()
                .find(|row| same_client_message(row, &episode))
            {
                out.push(existing.clone());
                continue;
            }
            stored_rows.push(episode.clone());
            out.push(episode);
        }
        Ok(out)
    }

    async fn insert_episodes_checked(
        &self,
        tenant: &TenantContext,
        episodes: Vec<memcore_core::Episode>,
    ) -> MemcoreResult<Vec<memcore_core::Episode>> {
        // One write lock for the whole batch: new rows are staged and only stored if every episode
        // passes.
        let mut stored_rows = self.episodes.write().expect("episodes lock poisoned");
        let mut staged: Vec<memcore_core::Episode> = Vec::new();
        let mut out = Vec::with_capacity(episodes.len());
        for (position, episode) in episodes.into_iter().enumerate() {
            if !episode_in_tenant(&episode, tenant) {
                return Err(MemcoreError::Forbidden);
            }
            if let Some(existing) = stored_rows
                .iter()
                .chain(staged.iter())
                .find(|row| same_client_message(row, &episode))
            {
                if !existing.same_occurrence(&episode) {
                    return Err(MemcoreError::message_id_conflict(position));
                }
                out.push(existing.clone());
                continue;
            }
            staged.push(episode.clone());
            out.push(episode);
        }
        stored_rows.extend(staged);
        Ok(out)
    }

    async fn get_episodes(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<Vec<memcore_core::Episode>> {
        Ok(self
            .episodes
            .read()
            .expect("episodes lock poisoned")
            .iter()
            .filter(|episode| episode_in_tenant(episode, tenant) && ids.contains(&episode.id))
            .cloned()
            .collect())
    }

    async fn mark_episodes_embedded(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<()> {
        for episode in self
            .episodes
            .write()
            .expect("episodes lock poisoned")
            .iter_mut()
        {
            if episode_in_tenant(episode, tenant) && ids.contains(&episode.id) {
                episode.embedded = true;
            }
        }
        Ok(())
    }

    async fn list_episodes(
        &self,
        tenant: &TenantContext,
        limit: usize,
    ) -> MemcoreResult<Vec<memcore_core::Episode>> {
        let mut episodes: Vec<_> = self
            .episodes
            .read()
            .expect("episodes lock poisoned")
            .iter()
            .filter(|episode| episode_in_tenant(episode, tenant))
            .cloned()
            .collect();
        episodes.sort_by(|a, b| a.recorded_at.cmp(&b.recorded_at).then(a.id.cmp(&b.id)));
        episodes.truncate(limit);
        Ok(episodes)
    }

    async fn list_episodes_after(
        &self,
        tenant: &TenantContext,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: usize,
    ) -> MemcoreResult<Vec<memcore_core::Episode>> {
        let mut episodes: Vec<_> = self
            .episodes
            .read()
            .expect("episodes lock poisoned")
            .iter()
            .filter(|episode| episode_in_tenant(episode, tenant))
            .filter(|episode| after.is_none_or(|key| (episode.recorded_at, episode.id) > key))
            .cloned()
            .collect();
        episodes.sort_by(|a, b| a.recorded_at.cmp(&b.recorded_at).then(a.id.cmp(&b.id)));
        episodes.truncate(limit);
        Ok(episodes)
    }

    async fn delete_user_episodes(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let mut episodes = self.episodes.write().expect("episodes lock poisoned");
        let before = episodes.len();
        episodes.retain(|episode| !episode_in_tenant(episode, tenant));
        Ok(before - episodes.len())
    }

    async fn delete_episodes_recorded_before(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<Vec<Uuid>> {
        let mut episodes = self.episodes.write().expect("episodes lock poisoned");
        let expired = |episode: &memcore_core::Episode| {
            episode_in_tenant(episode, tenant) && episode.recorded_at < cutoff
        };
        let ids: Vec<Uuid> = episodes
            .iter()
            .filter(|episode| expired(episode))
            .map(|episode| episode.id)
            .collect();
        if !dry_run {
            episodes.retain(|episode| !expired(episode));
        }
        Ok(ids)
    }

    async fn count_episodes_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        Ok(self
            .episodes
            .read()
            .expect("episodes lock poisoned")
            .iter()
            .filter(|episode| episode.org_id == org_id)
            .count())
    }

    async fn episode_id_exists_in_any_tenant(&self, episode_id: Uuid) -> MemcoreResult<bool> {
        Ok(self
            .episodes
            .read()
            .expect("episodes lock poisoned")
            .iter()
            .any(|episode| episode.id == episode_id))
    }
}

/// In-memory `EntityStore` for tests. Same contract as the SQLite and Postgres
/// stores: every read and write is scoped to one tenant user.
#[derive(Debug, Default)]
pub struct MockEntityStore {
    entities: RwLock<Vec<memcore_core::Entity>>,
    links: RwLock<Vec<memcore_core::FactEntityLink>>,
    relations: RwLock<Vec<memcore_core::EntityRelation>>,
}

impl MockEntityStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn in_tenant(org_id: &str, user_id: &str, tenant: &TenantContext) -> bool {
    org_id == tenant.org_id && user_id == tenant.user_id
}

#[async_trait]
impl memcore_core::ports::EntityStore for MockEntityStore {
    async fn upsert_entity(
        &self,
        tenant: &TenantContext,
        entity: memcore_core::Entity,
    ) -> MemcoreResult<memcore_core::Entity> {
        if !in_tenant(&entity.org_id, &entity.user_id, tenant) {
            return Err(MemcoreError::Forbidden);
        }
        let mut entities = self.entities.write().expect("entities lock poisoned");
        match entities.iter_mut().find(|stored| stored.id == entity.id) {
            Some(stored) if in_tenant(&stored.org_id, &stored.user_id, tenant) => {
                *stored = memcore_core::Entity {
                    created_at: stored.created_at,
                    ..entity.clone()
                };
                Ok(stored.clone())
            }
            Some(_) => Err(MemcoreError::Forbidden),
            None => {
                entities.push(entity.clone());
                Ok(entity)
            }
        }
    }

    async fn get_entity(
        &self,
        tenant: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<memcore_core::Entity>> {
        Ok(self
            .entities
            .read()
            .expect("entities lock poisoned")
            .iter()
            .find(|entity| entity.id == id && in_tenant(&entity.org_id, &entity.user_id, tenant))
            .cloned())
    }

    async fn list_all_entities(
        &self,
        tenant: &TenantContext,
    ) -> MemcoreResult<Vec<memcore_core::Entity>> {
        let mut entities: Vec<_> = self
            .entities
            .read()
            .expect("entities lock poisoned")
            .iter()
            .filter(|entity| in_tenant(&entity.org_id, &entity.user_id, tenant))
            .cloned()
            .collect();
        entities.sort_by_key(|entity| (entity.created_at, entity.id.to_string()));
        Ok(entities)
    }

    async fn list_entities_page(
        &self,
        tenant: &TenantContext,
        limit: usize,
        cursor: Option<&memcore_core::pagination::PageCursor>,
    ) -> MemcoreResult<Vec<memcore_core::Entity>> {
        let mut entities = self.list_all_entities(tenant).await?;
        entities.reverse();
        Ok(entities
            .into_iter()
            .filter(|entity| {
                cursor.is_none_or(|cursor| {
                    memcore_core::pagination::is_after_cursor_in_desc_order(
                        entity.created_at,
                        &entity.id.to_string(),
                        cursor,
                    )
                })
            })
            .take(limit)
            .collect())
    }

    async fn link_fact(
        &self,
        tenant: &TenantContext,
        link: memcore_core::FactEntityLink,
    ) -> MemcoreResult<()> {
        if !in_tenant(&link.org_id, &link.user_id, tenant) {
            return Err(MemcoreError::Forbidden);
        }
        let mut links = self.links.write().expect("links lock poisoned");
        if !links.iter().any(|stored| {
            stored.fact_id == link.fact_id
                && stored.entity_id == link.entity_id
                && stored.role == link.role
        }) {
            links.push(link);
        }
        Ok(())
    }

    async fn unlink_fact(
        &self,
        tenant: &TenantContext,
        link: &memcore_core::FactEntityLink,
    ) -> MemcoreResult<bool> {
        let mut links = self.links.write().expect("links lock poisoned");
        let before = links.len();
        links.retain(|stored| {
            !(in_tenant(&stored.org_id, &stored.user_id, tenant)
                && stored.fact_id == link.fact_id
                && stored.entity_id == link.entity_id
                && stored.role == link.role)
        });
        Ok(links.len() < before)
    }

    async fn links_for_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Vec<memcore_core::FactEntityLink>> {
        Ok(self
            .links
            .read()
            .expect("links lock poisoned")
            .iter()
            .filter(|link| {
                in_tenant(&link.org_id, &link.user_id, tenant) && link.entity_id == entity_id
            })
            .cloned()
            .collect())
    }

    async fn links_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<memcore_core::FactEntityLink>> {
        Ok(self
            .links
            .read()
            .expect("links lock poisoned")
            .iter()
            .filter(|link| {
                in_tenant(&link.org_id, &link.user_id, tenant) && fact_ids.contains(&link.fact_id)
            })
            .cloned()
            .collect())
    }

    async fn insert_relation(
        &self,
        tenant: &TenantContext,
        relation: memcore_core::EntityRelation,
    ) -> MemcoreResult<memcore_core::EntityRelation> {
        if !in_tenant(&relation.org_id, &relation.user_id, tenant) {
            return Err(MemcoreError::Forbidden);
        }
        self.relations
            .write()
            .expect("relations lock poisoned")
            .push(relation.clone());
        Ok(relation)
    }

    async fn relations_for_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Vec<memcore_core::EntityRelation>> {
        Ok(self
            .relations
            .read()
            .expect("relations lock poisoned")
            .iter()
            .filter(|relation| {
                in_tenant(&relation.org_id, &relation.user_id, tenant)
                    && (relation.source_entity_id == entity_id
                        || relation.target_entity_id == entity_id)
            })
            .cloned()
            .collect())
    }

    async fn delete_user_entities(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.links
            .write()
            .expect("links lock poisoned")
            .retain(|link| !in_tenant(&link.org_id, &link.user_id, tenant));
        self.relations
            .write()
            .expect("relations lock poisoned")
            .retain(|relation| !in_tenant(&relation.org_id, &relation.user_id, tenant));
        let mut entities = self.entities.write().expect("entities lock poisoned");
        let before = entities.len();
        entities.retain(|entity| !in_tenant(&entity.org_id, &entity.user_id, tenant));
        Ok(before - entities.len())
    }

    async fn delete_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<Uuid>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut links = self.links.write().expect("links lock poisoned");
        links.retain(|link| {
            !(in_tenant(&link.org_id, &link.user_id, tenant) && fact_ids.contains(&link.fact_id))
        });
        let mut relations = self.relations.write().expect("relations lock poisoned");
        relations.retain(|relation| {
            !(in_tenant(&relation.org_id, &relation.user_id, tenant)
                && relation
                    .source_fact_id
                    .is_some_and(|fact_id| fact_ids.contains(&fact_id)))
        });
        let mut entities = self.entities.write().expect("entities lock poisoned");
        let mut orphans: Vec<Uuid> = entities
            .iter()
            .filter(|entity| {
                in_tenant(&entity.org_id, &entity.user_id, tenant)
                    && !links.iter().any(|link| {
                        in_tenant(&link.org_id, &link.user_id, tenant)
                            && link.entity_id == entity.id
                    })
            })
            .map(|entity| entity.id)
            .collect();
        orphans.sort();
        relations.retain(|relation| {
            !(in_tenant(&relation.org_id, &relation.user_id, tenant)
                && (orphans.contains(&relation.source_entity_id)
                    || orphans.contains(&relation.target_entity_id)))
        });
        entities.retain(|entity| {
            !(in_tenant(&entity.org_id, &entity.user_id, tenant) && orphans.contains(&entity.id))
        });
        Ok(orphans)
    }

    async fn count_entities_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        Ok(self
            .entities
            .read()
            .expect("entities lock poisoned")
            .iter()
            .filter(|entity| entity.org_id == org_id)
            .count())
    }
}
