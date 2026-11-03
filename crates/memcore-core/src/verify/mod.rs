//! `verify-vectors`, tenant-safe verification and repair of the SQL /
//! vector-store state of one tenant (organization + user).
//!
//! SQL is the authority. Three passes, all paginated (bounded memory):
//!
//! 1. **SQL-driven.** Every fact row of the tenant (live and soft-deleted, `list_fact_states`) and
//!    every episode row (`list_episodes_after`) is compared with ALL of its points (a fully
//!    paginated tenant + entity scroll; never the 256-point `fact_points` listing).
//! 2. **Vector-driven.** Every point whose payload names the tenant is checked for a row of the
//!    tenant. A point without one is an orphan of a hard-deleted row only if NO tenant has a row
//!    with that id (`*_id_exists_in_any_tenant`, existence only); otherwise it may be another
//!    tenant's and is a `tenant_attribution_conflict`, reported and never touched.
//! 3. **Global, metadata only.** Points without a usable tenant payload are counted and listed by
//!    point id (`unattributable_point`), never touched.
//!
//! A dry run reads only: no SQL or vector write, no embedding call. A repair needs the exact
//! confirmation `verify-vectors:<org>:<user>`, repairs each item where it is found (after
//! re-reading its row), never mutates a global item, never embeds into a collection whose
//! identity is not verified, and is followed by a fresh read-only verification that decides the
//! outcome. Re-running is the resume mechanism: nothing is kept between runs.
//!
//! Limits (stated, not hidden): no cross-store transaction; run a repair while the tenant is not
//! being written (the serialised-ingestion contract), otherwise items may be reported failed or
//! changed, never repaired into another tenant.

use std::collections::{BTreeMap, HashMap};

use memcore_common::{MemcoreError, MemcoreResult};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::lifecycle::{checked_embedding_from, vector_record_from_fact};
use crate::ports::{
    EmbeddingProvider, EpisodeStore, FactStore, PointScope, StoredPoint, VectorIdentityStatus,
    VectorRecord, VectorStore, VectorWriteIntent, point_id_order_key,
};
use crate::{Episode, Fact, MemoryType, TenantContext};

/// Counter of verification items by `collection`, `category` and `result` (fixed values only).
pub const VERIFY_ITEMS_METRIC: &str = "memcore_vector_verify_items_total";
/// Counter of verification runs by `mode` (`dry_run`, `repair`) and `result` (`complete`,
/// `incomplete`).
pub const VERIFY_SCANS_METRIC: &str = "memcore_vector_verify_scans_total";
/// `collection` label values.
pub const COLLECTIONS: &[&str] = &["facts", "episodes", "global"];
/// Every `category` label value.
pub const CATEGORIES: &[&str] = &[
    "consistent",
    "missing_vector",
    "stale_content",
    "duplicate_points",
    "soft_deleted_orphan",
    "hard_deleted_orphan",
    "episode_pending",
    "episode_short_exempt",
    "verification_error",
    "unattributable_point",
    "tenant_attribution_conflict",
];
/// `result` label values.
pub const ITEM_RESULTS: &[&str] = &["detected", "repaired", "failed", "skipped_unsafe"];

const FACT_CATEGORIES: &[&str] = &[
    "consistent",
    "missing_vector",
    "stale_content",
    "duplicate_points",
    "soft_deleted_orphan",
    "hard_deleted_orphan",
    "verification_error",
];
const EPISODE_CATEGORIES: &[&str] = &[
    "consistent",
    "missing_vector",
    "stale_content",
    "duplicate_points",
    "episode_pending",
    "episode_short_exempt",
    "hard_deleted_orphan",
    "verification_error",
];
const GLOBAL_CATEGORIES: &[&str] = &["unattributable_point", "tenant_attribution_conflict"];
/// Categories that are not a divergence of the tenant.
const NOT_DIVERGENT: &[&str] = &["consistent", "episode_short_exempt"];
/// Scan errors listed in the report (any error already makes the scan incomplete).
const MAX_ERRORS_LISTED: usize = 50;

/// The phrase an operator repeats to confirm a repair of exactly this tenant.
pub fn confirmation_phrase(tenant: &TenantContext) -> String {
    format!("verify-vectors:{}:{}", tenant.org_id, tenant.user_id)
}

/// The stores of one run. `None` vectors = that collection does not exist.
pub struct VerifyStores<'a> {
    pub facts: &'a dyn FactStore,
    pub episodes: Option<&'a dyn EpisodeStore>,
    pub fact_vectors: Option<&'a dyn VectorStore>,
    pub episode_vectors: Option<&'a dyn VectorStore>,
    /// Used only by a confirmed repair, and only for a collection whose identity is verified.
    pub embedding: Option<&'a dyn EmbeddingProvider>,
    pub fact_identity: VectorIdentityStatus,
    pub episode_identity: VectorIdentityStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerifyMode {
    DryRun,
    Repair { confirmation: String },
}

#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub mode: VerifyMode,
    /// Rows / points per page.
    pub page_size: usize,
    /// Items listed in the report (all are counted).
    pub max_items_listed: usize,
    /// Pages per scroll before the scan is declared incomplete (a cursor that never ends).
    pub max_pages: usize,
}

impl VerifyOptions {
    pub fn dry_run() -> Self {
        Self {
            mode: VerifyMode::DryRun,
            page_size: 256,
            max_items_listed: 200,
            max_pages: 1_000_000,
        }
    }

    pub fn repair(confirmation: impl Into<String>) -> Self {
        Self {
            mode: VerifyMode::Repair {
                confirmation: confirmation.into(),
            },
            ..Self::dry_run()
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IdentityReport {
    pub facts: VectorIdentityStatus,
    pub episodes: VectorIdentityStatus,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct ScanReport {
    /// True only when every row and point of the tenant (and every unattributed point) was read
    /// and every check succeeded.
    pub complete: bool,
    pub errors: Vec<String>,
    pub fact_rows: u64,
    pub fact_points: u64,
    pub episode_rows: u64,
    pub episode_points: u64,
}

/// One classified item. Ids and hashes only: never text, never another tenant's id.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ItemReport {
    pub collection: &'static str,
    pub category: &'static str,
    /// The tenant's own fact / episode id (`None` for global items).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    pub point_ids: Vec<String>,
    /// First 16 hex digits of sha256 of the authoritative SQL text, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// `none` (dry run or not repairable), `repaired`, `failed`, `failed_stale_removed`,
    /// `skipped_identity_unverified`, `skipped_changed` or `report_only`.
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct RepairCounts {
    pub attempted: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub skipped_unsafe: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AfterReport {
    pub scan: ScanReport,
    pub facts: BTreeMap<&'static str, u64>,
    pub episodes: BTreeMap<&'static str, u64>,
    pub global: BTreeMap<&'static str, u64>,
    pub remaining_inconsistent: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VerifyReport {
    pub operation: &'static str,
    pub mode: &'static str,
    pub org_id: String,
    pub user_id: String,
    pub identity: IdentityReport,
    pub scan: ScanReport,
    pub facts: BTreeMap<&'static str, u64>,
    pub episodes: BTreeMap<&'static str, u64>,
    /// Not attributed to the tenant: reported only, never mutated.
    pub global: BTreeMap<&'static str, u64>,
    /// Informational: groups of live facts of the tenant with identical text. Never merged.
    pub duplicate_content: u64,
    pub items: Vec<ItemReport>,
    pub items_truncated: bool,
    pub repairs: RepairCounts,
    /// Repair mode: the fresh read-only verification after the repairs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<AfterReport>,
    /// The tenant's divergent items left (after the repair, or found by the dry run).
    pub remaining_inconsistent: u64,
    /// `clean`, `repair_needed`, `repair_completed`, `repair_incomplete`, `attention_required`,
    /// `blocked` or `refused`.
    pub outcome: &'static str,
    /// 0 clean / repair_completed / attention_required, 1 repair_needed, 2 refused,
    /// 3 repair_incomplete, 4 blocked.
    pub exit_code: i32,
}

fn counts(categories: &[&'static str]) -> BTreeMap<&'static str, u64> {
    categories.iter().map(|category| (*category, 0)).collect()
}

fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    hex::encode(digest)[..16].to_string()
}

/// Error text for the report: no credentialed URL, bounded length.
fn scrub(error: &MemcoreError) -> String {
    let text = error.to_string();
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(char::is_whitespace) {
        if word.contains("://") && word.contains('@') {
            out.push_str("[redacted-url]");
            if word.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else {
            out.push_str(word);
        }
    }
    out.chars().take(300).collect()
}

fn divergent(map: &BTreeMap<&'static str, u64>) -> u64 {
    map.iter()
        .filter(|(category, _)| !NOT_DIVERGENT.contains(category))
        .map(|(_, n)| *n)
        .sum()
}

/// Every point of `scope`, fully paginated. A cursor that does not advance, or more than
/// `max_pages` pages, is an error (never a short, "clean" answer).
async fn all_points(
    store: &dyn VectorStore,
    scope: &PointScope,
    page_size: usize,
    max_pages: usize,
) -> MemcoreResult<Vec<StoredPoint>> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..max_pages {
        let page = store
            .scroll_points(scope, cursor.clone(), page_size)
            .await?;
        out.extend(page.points);
        match page.next_cursor {
            None => return Ok(out),
            Some(next) if Some(&next) == cursor.as_ref() => {
                return Err(MemcoreError::Internal(
                    "vector scroll did not advance; enumeration incomplete".to_string(),
                ));
            }
            Some(next) => cursor = Some(next),
        }
    }
    Err(MemcoreError::Internal(
        "vector scroll exceeded the page limit; enumeration incomplete".to_string(),
    ))
}

fn point_matches_fact(point: &StoredPoint, fact: &Fact) -> bool {
    point.content.as_deref() == Some(fact.content.as_str())
        && point.memory_type == Some(fact.memory_type)
}

fn point_matches_episode(point: &StoredPoint, episode: &Episode) -> bool {
    point.content.as_deref() == Some(episode.content.as_str())
}

fn smallest<'p>(points: impl Iterator<Item = &'p StoredPoint>) -> Option<&'p StoredPoint> {
    points.min_by_key(|point| point_id_order_key(&point.point_id))
}

fn episode_record(episode: &Episode, embedding: Vec<f32>) -> VectorRecord {
    // The same record `index_episodes` writes (point id = episode id).
    VectorRecord {
        id: episode.id,
        fact_id: episode.id,
        org_id: episode.org_id.clone(),
        user_id: episode.user_id.clone(),
        embedding,
        content: episode.content.clone(),
        memory_type: MemoryType::Conversation,
        metadata: serde_json::json!({ "kind": "episode" }),
    }
}

/// The working state of one pass over the stores.
struct Run<'s, 'a> {
    stores: &'s VerifyStores<'a>,
    tenant: &'s TenantContext,
    options: &'s VerifyOptions,
    repair: bool,
    scan: ScanReport,
    facts: BTreeMap<&'static str, u64>,
    episodes: BTreeMap<&'static str, u64>,
    global: BTreeMap<&'static str, u64>,
    duplicate_hashes: HashMap<String, u64>,
    items: Vec<ItemReport>,
    items_truncated: bool,
    repairs: RepairCounts,
    /// Per (collection, category): repaired / failed / skipped_unsafe.
    results: BTreeMap<(&'static str, &'static str, &'static str), u64>,
    /// Points the vector-driven pass found for rows of the tenant, per collection: a dry pass
    /// checks they equal the points the SQL-driven pass enumerated per row.
    owned_points: BTreeMap<&'static str, u64>,
}

impl<'s, 'a> Run<'s, 'a> {
    fn new(
        stores: &'s VerifyStores<'a>,
        tenant: &'s TenantContext,
        options: &'s VerifyOptions,
        repair: bool,
    ) -> Self {
        Self {
            stores,
            tenant,
            options,
            repair,
            scan: ScanReport {
                complete: true,
                ..ScanReport::default()
            },
            facts: counts(FACT_CATEGORIES),
            episodes: counts(EPISODE_CATEGORIES),
            global: counts(GLOBAL_CATEGORIES),
            duplicate_hashes: HashMap::new(),
            items: Vec::new(),
            items_truncated: false,
            repairs: RepairCounts::default(),
            results: BTreeMap::new(),
            owned_points: BTreeMap::new(),
        }
    }

    /// A repair runs only while everything read so far is complete: after any read failure the
    /// rest of the run only classifies (no repair on partial knowledge).
    fn may_repair(&self) -> bool {
        self.repair && self.scan.complete
    }

    fn incomplete(&mut self, what: &str, error: &MemcoreError) {
        self.scan.complete = false;
        if self.scan.errors.len() < MAX_ERRORS_LISTED {
            self.scan.errors.push(format!("{what}: {}", scrub(error)));
        }
    }

    fn count(&mut self, collection: &'static str, category: &'static str) {
        let map = match collection {
            "facts" => &mut self.facts,
            "episodes" => &mut self.episodes,
            _ => &mut self.global,
        };
        *map.entry(category).or_default() += 1;
    }

    /// Records an item (counted; listed up to the cap) with the action that was taken.
    #[allow(clippy::too_many_arguments)]
    fn item(
        &mut self,
        collection: &'static str,
        category: &'static str,
        entity_id: Option<Uuid>,
        point_ids: Vec<String>,
        content: Option<&str>,
        action: &'static str,
        detail: Option<String>,
    ) {
        self.count(collection, category);
        let result = match action {
            "repaired" => Some("repaired"),
            "failed" | "failed_stale_removed" => Some("failed"),
            "skipped_identity_unverified" | "skipped_changed" => Some("skipped_unsafe"),
            _ => None,
        };
        if let Some(result) = result {
            *self
                .results
                .entry((collection, category, result))
                .or_default() += 1;
            self.repairs.attempted += u64::from(result != "skipped_unsafe");
            match result {
                "repaired" => self.repairs.succeeded += 1,
                "failed" => self.repairs.failed += 1,
                _ => self.repairs.skipped_unsafe += 1,
            }
        }
        if self.items.len() >= self.options.max_items_listed {
            self.items_truncated = true;
            return;
        }
        self.items.push(ItemReport {
            collection,
            category,
            entity_id: entity_id.map(|id| id.to_string()),
            point_ids,
            content_sha256: content.map(content_hash),
            action,
            detail,
        });
    }

    fn ids(points: &[StoredPoint]) -> Vec<String> {
        let mut ids: Vec<String> = points.iter().map(|point| point.point_id.clone()).collect();
        ids.sort_by_key(|id| point_id_order_key(id));
        ids
    }

    async fn points_of(
        &self,
        store: &dyn VectorStore,
        entity: Uuid,
    ) -> MemcoreResult<Vec<StoredPoint>> {
        all_points(
            store,
            &PointScope::TenantEntity(self.tenant.clone(), entity),
            self.options.page_size,
            self.options.max_pages,
        )
        .await
    }

    // ---------------------------------------------------------------------------------------
    // Pass 1a: facts (SQL-driven)
    // ---------------------------------------------------------------------------------------

    async fn facts_pass(&mut self) {
        let Some(vectors) = self.stores.fact_vectors else {
            self.incomplete(
                "fact collection",
                &MemcoreError::NotFound("the fact collection does not exist".to_string()),
            );
            return;
        };
        let mut after = None;
        for _ in 0..self.options.max_pages {
            let page = match self
                .stores
                .facts
                .list_fact_states(self.tenant, after, self.options.page_size)
                .await
            {
                Ok(page) => page,
                Err(error) => return self.incomplete("fact rows", &error),
            };
            if page.is_empty() {
                return;
            }
            after = page.last().map(|state| state.fact.id);
            for state in page {
                self.scan.fact_rows += 1;
                let fact = state.fact;
                let points = match self.points_of(vectors, fact.id).await {
                    Ok(points) => points,
                    Err(error) => {
                        self.incomplete("fact points", &error);
                        self.item(
                            "facts",
                            "verification_error",
                            Some(fact.id),
                            Vec::new(),
                            None,
                            "none",
                            Some(scrub(&error)),
                        );
                        continue;
                    }
                };
                self.scan.fact_points += points.len() as u64;
                if state.deleted {
                    if !points.is_empty() {
                        self.soft_deleted_orphan(vectors, &fact, &points).await;
                    }
                    continue;
                }
                *self
                    .duplicate_hashes
                    .entry(content_hash(&fact.content))
                    .or_default() += 1;
                let consistent: Vec<&StoredPoint> = points
                    .iter()
                    .filter(|point| point_matches_fact(point, &fact))
                    .collect();
                if points.len() == 1 && consistent.len() == 1 {
                    self.count("facts", "consistent");
                } else if points.is_empty() {
                    self.fact_reembed(vectors, &fact, &points, "missing_vector")
                        .await;
                } else if consistent.is_empty() {
                    self.fact_reembed(vectors, &fact, &points, "stale_content")
                        .await;
                } else {
                    self.fact_duplicates(vectors, &fact, &points).await;
                }
            }
        }
        self.incomplete(
            "fact rows",
            &MemcoreError::Internal("fact row pagination exceeded the page limit".to_string()),
        );
    }

    async fn soft_deleted_orphan(
        &mut self,
        vectors: &dyn VectorStore,
        fact: &Fact,
        points: &[StoredPoint],
    ) {
        let ids = Self::ids(points);
        if !self.may_repair() {
            return self.item(
                "facts",
                "soft_deleted_orphan",
                Some(fact.id),
                ids,
                Some(&fact.content),
                "none",
                None,
            );
        }
        // Guard: the row is still this tenant's and still deleted.
        match self
            .stores
            .facts
            .get_fact_including_deleted(self.tenant, fact.id)
            .await
        {
            Ok(Some(state)) if state.deleted => {}
            Ok(_) => {
                return self.item(
                    "facts",
                    "soft_deleted_orphan",
                    Some(fact.id),
                    ids,
                    Some(&fact.content),
                    "skipped_changed",
                    None,
                );
            }
            Err(error) => {
                let detail = Some(scrub(&error));
                return self.item(
                    "facts",
                    "soft_deleted_orphan",
                    Some(fact.id),
                    ids,
                    Some(&fact.content),
                    "failed",
                    detail,
                );
            }
        }
        let action = self.delete_and_verify(vectors, fact.id, &ids, &[]).await;
        let (action, detail) = action;
        self.item(
            "facts",
            "soft_deleted_orphan",
            Some(fact.id),
            ids,
            Some(&fact.content),
            action,
            detail,
        );
    }

    /// Deletes `delete` (tenant-scoped) and checks the entity has exactly `keep` left.
    async fn delete_and_verify(
        &self,
        vectors: &dyn VectorStore,
        entity: Uuid,
        delete: &[String],
        keep: &[String],
    ) -> (&'static str, Option<String>) {
        if let Err(error) = vectors.delete_points(self.tenant, delete).await {
            return ("failed", Some(scrub(&error)));
        }
        match self.points_of(vectors, entity).await {
            Ok(left) => {
                let mut left = Self::ids(&left);
                let mut expected = keep.to_vec();
                left.sort();
                expected.sort();
                if left == expected {
                    ("repaired", None)
                } else {
                    ("failed", Some("points remain after the delete".to_string()))
                }
            }
            Err(error) => ("failed", Some(scrub(&error))),
        }
    }

    async fn fact_duplicates(
        &mut self,
        vectors: &dyn VectorStore,
        fact: &Fact,
        points: &[StoredPoint],
    ) {
        let ids = Self::ids(points);
        if !self.may_repair() {
            return self.item(
                "facts",
                "duplicate_points",
                Some(fact.id),
                ids,
                Some(&fact.content),
                "none",
                None,
            );
        }
        if !self.fact_unchanged(fact).await {
            return self.item(
                "facts",
                "duplicate_points",
                Some(fact.id),
                ids,
                Some(&fact.content),
                "skipped_changed",
                None,
            );
        }
        // Keep the smallest-id point that carries the row (the store's replacement rule);
        // delete every other point of the fact. No embedding is needed.
        let keep = smallest(
            points
                .iter()
                .filter(|point| point_matches_fact(point, fact)),
        )
        .map(|point| point.point_id.clone())
        .expect("a consistent point exists");
        let delete: Vec<String> = ids.iter().filter(|id| **id != keep).cloned().collect();
        let (action, detail) = self
            .delete_and_verify(vectors, fact.id, &delete, &[keep])
            .await;
        self.item(
            "facts",
            "duplicate_points",
            Some(fact.id),
            ids,
            Some(&fact.content),
            action,
            detail,
        );
    }

    /// True when the tenant's live row still has the text this pass classified.
    async fn fact_unchanged(&self, fact: &Fact) -> bool {
        matches!(
            self.stores.facts.get_fact(self.tenant, fact.id).await,
            Ok(Some(current)) if current.content == fact.content && current.memory_type == fact.memory_type
        )
    }

    async fn fact_reembed(
        &mut self,
        vectors: &dyn VectorStore,
        fact: &Fact,
        points: &[StoredPoint],
        category: &'static str,
    ) {
        let ids = Self::ids(points);
        if !self.may_repair() {
            return self.item(
                "facts",
                category,
                Some(fact.id),
                ids,
                Some(&fact.content),
                "none",
                None,
            );
        }
        let Some(provider) = self
            .stores
            .embedding
            .filter(|_| self.stores.fact_identity.allows_embedding())
        else {
            return self.item(
                "facts",
                category,
                Some(fact.id),
                ids,
                Some(&fact.content),
                "skipped_identity_unverified",
                None,
            );
        };
        if !self.fact_unchanged(fact).await {
            return self.item(
                "facts",
                category,
                Some(fact.id),
                ids,
                Some(&fact.content),
                "skipped_changed",
                None,
            );
        }
        let embedding = match checked_embedding_from(provider, &fact.content).await {
            Ok(embedding) => embedding,
            Err(error) => {
                let detail = Some(scrub(&error));
                if category == "stale_content" {
                    // Privacy first: the obsolete text leaves the vector store; the fact is now
                    // missing its vector, which a later run repairs.
                    let removed = vectors.delete_points(self.tenant, &ids).await.is_ok();
                    let action = if removed {
                        "failed_stale_removed"
                    } else {
                        "failed"
                    };
                    return self.item(
                        "facts",
                        category,
                        Some(fact.id),
                        ids,
                        Some(&fact.content),
                        action,
                        detail,
                    );
                }
                return self.item(
                    "facts",
                    category,
                    Some(fact.id),
                    ids,
                    Some(&fact.content),
                    "failed",
                    detail,
                );
            }
        };
        if let Err(error) = vectors
            .write_vector(
                self.tenant,
                vector_record_from_fact(fact, embedding),
                VectorWriteIntent::Replace,
            )
            .await
        {
            let detail = Some(scrub(&error));
            return self.item(
                "facts",
                category,
                Some(fact.id),
                ids,
                Some(&fact.content),
                "failed",
                detail,
            );
        }
        let verified = match self.points_of(vectors, fact.id).await {
            Ok(left) => left.len() == 1 && point_matches_fact(&left[0], fact),
            Err(_) => false,
        };
        let (action, detail) = if verified {
            ("repaired", None)
        } else {
            (
                "failed",
                Some("the repaired vector could not be verified".to_string()),
            )
        };
        self.item(
            "facts",
            category,
            Some(fact.id),
            ids,
            Some(&fact.content),
            action,
            detail,
        );
    }

    // ---------------------------------------------------------------------------------------
    // Pass 1b: episodes (SQL-driven)
    // ---------------------------------------------------------------------------------------

    async fn episodes_pass(&mut self) {
        let Some(store) = self.stores.episodes else {
            return;
        };
        let vectors = self.stores.episode_vectors;
        let mut after = None;
        for _ in 0..self.options.max_pages {
            let page = match store
                .list_episodes_after(self.tenant, after, self.options.page_size)
                .await
            {
                Ok(page) => page,
                Err(error) => return self.incomplete("episode rows", &error),
            };
            if page.is_empty() {
                return;
            }
            after = page.last().map(|episode| (episode.recorded_at, episode.id));
            for episode in page {
                self.scan.episode_rows += 1;
                let points = match vectors {
                    Some(vectors) => match self.points_of(vectors, episode.id).await {
                        Ok(points) => points,
                        Err(error) => {
                            self.incomplete("episode points", &error);
                            self.item(
                                "episodes",
                                "verification_error",
                                Some(episode.id),
                                Vec::new(),
                                None,
                                "none",
                                Some(scrub(&error)),
                            );
                            continue;
                        }
                    },
                    None => Vec::new(),
                };
                self.scan.episode_points += points.len() as u64;
                // An episode that is not embeddable is stored without a vector by design.
                if !episode.is_embeddable() && points.is_empty() {
                    self.count("episodes", "episode_short_exempt");
                    continue;
                }
                let consistent: Vec<&StoredPoint> = points
                    .iter()
                    .filter(|point| point_matches_episode(point, &episode))
                    .collect();
                let category = if points.is_empty() {
                    if episode.embedded {
                        "missing_vector"
                    } else {
                        "episode_pending"
                    }
                } else if consistent.is_empty() {
                    "stale_content"
                } else if points.len() > 1 {
                    "duplicate_points"
                } else if !episode.embedded {
                    // A matching point whose flag lags: only the flag is missing.
                    "episode_pending"
                } else {
                    self.count("episodes", "consistent");
                    continue;
                };
                match vectors {
                    Some(vectors) => {
                        self.episode_repair(vectors, store, &episode, &points, category)
                            .await
                    }
                    None => {
                        // No episode collection: nothing can be inspected or written.
                        self.incomplete(
                            "episode collection",
                            &MemcoreError::NotFound(
                                "the episode collection does not exist".to_string(),
                            ),
                        );
                        self.item(
                            "episodes",
                            category,
                            Some(episode.id),
                            Vec::new(),
                            Some(&episode.content),
                            "none",
                            None,
                        );
                    }
                }
            }
        }
        self.incomplete(
            "episode rows",
            &MemcoreError::Internal("episode pagination exceeded the page limit".to_string()),
        );
    }

    async fn episode_repair(
        &mut self,
        vectors: &dyn VectorStore,
        store: &dyn EpisodeStore,
        episode: &Episode,
        points: &[StoredPoint],
        category: &'static str,
    ) {
        let ids = Self::ids(points);
        let content = Some(episode.content.as_str());
        if !self.may_repair() {
            return self.item(
                "episodes",
                category,
                Some(episode.id),
                ids,
                content,
                "none",
                None,
            );
        }
        // Guard: the row is still this tenant's with the same text.
        match store.get_episodes(self.tenant, &[episode.id]).await {
            Ok(rows) if rows.iter().any(|row| row.content == episode.content) => {}
            Ok(_) => {
                return self.item(
                    "episodes",
                    category,
                    Some(episode.id),
                    ids,
                    content,
                    "skipped_changed",
                    None,
                );
            }
            Err(error) => {
                let detail = Some(scrub(&error));
                return self.item(
                    "episodes",
                    category,
                    Some(episode.id),
                    ids,
                    content,
                    "failed",
                    detail,
                );
            }
        }
        let consistent: Vec<&StoredPoint> = points
            .iter()
            .filter(|point| point_matches_episode(point, episode))
            .collect();
        let stable = episode.id.to_string();
        if !consistent.is_empty() {
            // Keep the stable-id point if it matches, else the smallest matching one; no embedding.
            let keep = consistent
                .iter()
                .find(|point| point.point_id == stable)
                .copied()
                .or_else(|| smallest(consistent.iter().copied()))
                .map(|point| point.point_id.clone())
                .expect("a consistent point exists");
            let delete: Vec<String> = ids.iter().filter(|id| **id != keep).cloned().collect();
            let (mut action, mut detail) = if delete.is_empty() {
                ("repaired", None)
            } else {
                self.delete_and_verify(vectors, episode.id, &delete, std::slice::from_ref(&keep))
                    .await
            };
            if action == "repaired"
                && !episode.embedded
                && let Err(error) = store
                    .mark_episodes_embedded(self.tenant, &[episode.id])
                    .await
            {
                (action, detail) = ("failed", Some(scrub(&error)));
            }
            return self.item(
                "episodes",
                category,
                Some(episode.id),
                ids,
                content,
                action,
                detail,
            );
        }
        let Some(provider) = self
            .stores
            .embedding
            .filter(|_| self.stores.episode_identity.allows_embedding())
        else {
            return self.item(
                "episodes",
                category,
                Some(episode.id),
                ids,
                content,
                "skipped_identity_unverified",
                None,
            );
        };
        let embedding = match checked_embedding_from(provider, &episode.content).await {
            Ok(embedding) => embedding,
            Err(error) => {
                let detail = Some(scrub(&error));
                if category == "stale_content" {
                    let removed = vectors.delete_points(self.tenant, &ids).await.is_ok();
                    let action = if removed {
                        "failed_stale_removed"
                    } else {
                        "failed"
                    };
                    return self.item(
                        "episodes",
                        category,
                        Some(episode.id),
                        ids,
                        content,
                        action,
                        detail,
                    );
                }
                return self.item(
                    "episodes",
                    category,
                    Some(episode.id),
                    ids,
                    content,
                    "failed",
                    detail,
                );
            }
        };
        if let Err(error) = vectors
            .write_vector(
                self.tenant,
                episode_record(episode, embedding),
                VectorWriteIntent::StablePointId,
            )
            .await
        {
            let detail = Some(scrub(&error));
            return self.item(
                "episodes",
                category,
                Some(episode.id),
                ids,
                content,
                "failed",
                detail,
            );
        }
        // Points other than the stable one carried obsolete text: remove them, then verify.
        let others: Vec<String> = ids.iter().filter(|id| **id != stable).cloned().collect();
        let (mut action, mut detail) = self
            .delete_and_verify(vectors, episode.id, &others, std::slice::from_ref(&stable))
            .await;
        if action == "repaired" {
            match self.points_of(vectors, episode.id).await {
                Ok(left) if left.len() == 1 && point_matches_episode(&left[0], episode) => {}
                _ => {
                    (action, detail) = (
                        "failed",
                        Some("the repaired vector could not be verified".to_string()),
                    )
                }
            }
        }
        if action == "repaired"
            && let Err(error) = store
                .mark_episodes_embedded(self.tenant, &[episode.id])
                .await
        {
            (action, detail) = ("failed", Some(scrub(&error)));
        }
        self.item(
            "episodes",
            category,
            Some(episode.id),
            ids,
            content,
            action,
            detail,
        );
    }

    // ---------------------------------------------------------------------------------------
    // Pass 2: points of the tenant without a row of the tenant (vector-driven)
    // ---------------------------------------------------------------------------------------

    async fn orphan_pass(&mut self, collection: &'static str) {
        let vectors = match collection {
            "facts" => self.stores.fact_vectors,
            _ => self.stores.episode_vectors,
        };
        let Some(vectors) = vectors else {
            return;
        };
        let scope = PointScope::Tenant(self.tenant.clone());
        let expected = match vectors.count_points(&scope).await {
            Ok(n) => n,
            Err(error) => return self.incomplete("tenant point count", &error),
        };
        let mut seen = 0u64;
        let mut cursor: Option<String> = None;
        for _ in 0..self.options.max_pages {
            let page = match vectors
                .scroll_points(&scope, cursor.clone(), self.options.page_size)
                .await
            {
                Ok(page) => page,
                Err(error) => return self.incomplete("tenant point scroll", &error),
            };
            seen += page.points.len() as u64;
            for point in page.points {
                self.classify_tenant_point(vectors, collection, point).await;
            }
            match page.next_cursor {
                None => {
                    // Only a dry pass can compare: a repair changes points while it runs.
                    if !self.repair {
                        if seen != expected {
                            self.incomplete(
                                "tenant point scroll",
                                &MemcoreError::Internal(format!(
                                    "scrolled {seen} of {expected} points; enumeration incomplete"
                                )),
                            );
                        }
                        let per_row = match collection {
                            "facts" => self.scan.fact_points,
                            _ => self.scan.episode_points,
                        };
                        let owned = self.owned_points.get(collection).copied().unwrap_or(0);
                        if owned != per_row {
                            self.incomplete(
                                "per-row point enumeration",
                                &MemcoreError::Internal(format!(
                                    "{per_row} points found per row but {owned} in the tenant scroll; enumeration incomplete"
                                )),
                            );
                        }
                    }
                    return;
                }
                Some(next) if Some(&next) == cursor.as_ref() => {
                    return self.incomplete(
                        "tenant point scroll",
                        &MemcoreError::Internal("vector scroll did not advance".to_string()),
                    );
                }
                Some(next) => cursor = Some(next),
            }
        }
        self.incomplete(
            "tenant point scroll",
            &MemcoreError::Internal("vector scroll exceeded the page limit".to_string()),
        );
    }

    async fn classify_tenant_point(
        &mut self,
        vectors: &dyn VectorStore,
        collection: &'static str,
        point: StoredPoint,
    ) {
        let Some(entity) = point
            .entity_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok())
        else {
            // A tenant payload without a usable entity id: it cannot be tied to a row.
            return self.item(
                "global",
                "unattributable_point",
                None,
                vec![point.point_id],
                None,
                "report_only",
                None,
            );
        };
        let owned = match collection {
            "facts" => self
                .stores
                .facts
                .get_fact_including_deleted(self.tenant, entity)
                .await
                .map(|state| state.is_some()),
            _ => match self.stores.episodes {
                Some(store) => store
                    .get_episodes(self.tenant, &[entity])
                    .await
                    .map(|rows| !rows.is_empty()),
                None => Ok(false),
            },
        };
        match owned {
            Ok(true) => {
                // Classified by the SQL-driven pass.
                *self.owned_points.entry(collection).or_default() += 1;
                return;
            }
            Ok(false) => {}
            Err(error) => {
                self.incomplete("orphan check", &error);
                return self.item(
                    collection,
                    "verification_error",
                    None,
                    vec![point.point_id],
                    None,
                    "none",
                    Some(scrub(&error)),
                );
            }
        }
        let elsewhere = self.exists_in_any_tenant(collection, entity).await;
        match elsewhere {
            Ok(false) => {}
            Ok(true) => {
                // The id is another tenant's row: the point may be that tenant's. Never touched,
                // reported globally by point id only (no foreign id or tenant).
                return self.item(
                    "global",
                    "tenant_attribution_conflict",
                    None,
                    vec![point.point_id],
                    None,
                    "report_only",
                    None,
                );
            }
            Err(error) => {
                self.incomplete("attribution check", &error);
                return self.item(
                    collection,
                    "verification_error",
                    None,
                    vec![point.point_id],
                    None,
                    "none",
                    Some(scrub(&error)),
                );
            }
        }
        // An orphan of a hard-deleted row of this tenant (no tenant has the id).
        let ids = vec![point.point_id.clone()];
        if !self.may_repair() {
            return self.item(
                collection,
                "hard_deleted_orphan",
                Some(entity),
                ids,
                None,
                "none",
                None,
            );
        }
        // Guard: re-check right before the delete.
        match self.exists_in_any_tenant(collection, entity).await {
            Ok(false) => {}
            Ok(true) => {
                return self.item(
                    collection,
                    "hard_deleted_orphan",
                    Some(entity),
                    ids,
                    None,
                    "skipped_changed",
                    None,
                );
            }
            Err(error) => {
                let detail = Some(scrub(&error));
                return self.item(
                    collection,
                    "hard_deleted_orphan",
                    Some(entity),
                    ids,
                    None,
                    "failed",
                    detail,
                );
            }
        }
        let (action, detail) = match vectors.delete_points(self.tenant, &ids).await {
            Err(error) => ("failed", Some(scrub(&error))),
            Ok(()) => match self.points_of(vectors, entity).await {
                Ok(left) if !left.iter().any(|p| p.point_id == point.point_id) => {
                    ("repaired", None)
                }
                Ok(_) => (
                    "failed",
                    Some("the point remains after the delete".to_string()),
                ),
                Err(error) => ("failed", Some(scrub(&error))),
            },
        };
        self.item(
            collection,
            "hard_deleted_orphan",
            Some(entity),
            ids,
            None,
            action,
            detail,
        );
    }

    async fn exists_in_any_tenant(&self, collection: &str, entity: Uuid) -> MemcoreResult<bool> {
        match collection {
            "facts" => self.stores.facts.fact_id_exists_in_any_tenant(entity).await,
            _ => match self.stores.episodes {
                Some(store) => store.episode_id_exists_in_any_tenant(entity).await,
                None => Ok(false),
            },
        }
    }

    // ---------------------------------------------------------------------------------------
    // Pass 3: points without tenant payload (global, metadata only, never mutated)
    // ---------------------------------------------------------------------------------------

    async fn unattributed_pass(&mut self) {
        for vectors in [self.stores.fact_vectors, self.stores.episode_vectors]
            .into_iter()
            .flatten()
        {
            let expected = match vectors.count_points(&PointScope::Unattributed).await {
                Ok(n) => n,
                Err(error) => {
                    self.incomplete("unattributed point count", &error);
                    continue;
                }
            };
            match all_points(
                vectors,
                &PointScope::Unattributed,
                self.options.page_size,
                self.options.max_pages,
            )
            .await
            {
                Ok(points) => {
                    if points.len() as u64 != expected {
                        self.incomplete(
                            "unattributed point scroll",
                            &MemcoreError::Internal(
                                "unattributed enumeration incomplete".to_string(),
                            ),
                        );
                    }
                    for point in points {
                        self.item(
                            "global",
                            "unattributable_point",
                            None,
                            vec![point.point_id],
                            None,
                            "report_only",
                            None,
                        );
                    }
                }
                Err(error) => self.incomplete("unattributed point scroll", &error),
            }
        }
    }

    async fn run(&mut self) {
        self.facts_pass().await;
        self.episodes_pass().await;
        self.orphan_pass("facts").await;
        self.orphan_pass("episodes").await;
        self.unattributed_pass().await;
    }

    fn duplicate_content(&self) -> u64 {
        self.duplicate_hashes.values().filter(|n| **n > 1).count() as u64
    }

    fn tenant_divergent(&self) -> u64 {
        divergent(&self.facts) + divergent(&self.episodes)
    }

    fn global_attention(&self) -> u64 {
        self.global.values().sum()
    }
}

/// Runs a dry run or a confirmed repair for exactly `tenant`.
pub async fn verify_vectors(
    stores: &VerifyStores<'_>,
    tenant: &TenantContext,
    options: &VerifyOptions,
) -> VerifyReport {
    let identity = IdentityReport {
        facts: stores.fact_identity,
        episodes: stores.episode_identity,
    };
    let repair = match &options.mode {
        VerifyMode::DryRun => false,
        VerifyMode::Repair { confirmation } => {
            if *confirmation != confirmation_phrase(tenant) {
                let report = VerifyReport {
                    operation: "verify-vectors",
                    mode: "repair",
                    org_id: tenant.org_id.clone(),
                    user_id: tenant.user_id.clone(),
                    identity,
                    scan: ScanReport::default(),
                    facts: counts(FACT_CATEGORIES),
                    episodes: counts(EPISODE_CATEGORIES),
                    global: counts(GLOBAL_CATEGORIES),
                    duplicate_content: 0,
                    items: Vec::new(),
                    items_truncated: false,
                    repairs: RepairCounts::default(),
                    after: None,
                    remaining_inconsistent: 0,
                    outcome: "refused",
                    exit_code: 2,
                };
                return report;
            }
            true
        }
    };

    let mut run = Run::new(stores, tenant, options, repair);
    run.run().await;

    let after = if repair {
        let mut check = Run::new(stores, tenant, options, false);
        check.run().await;
        let remaining = check.tenant_divergent();
        Some(AfterReport {
            scan: check.scan.clone(),
            facts: check.facts.clone(),
            episodes: check.episodes.clone(),
            global: check.global.clone(),
            remaining_inconsistent: remaining,
        })
    } else {
        None
    };

    let (remaining, outcome, exit_code) = match &after {
        None => {
            let remaining = run.tenant_divergent();
            if !run.scan.complete {
                (remaining, "blocked", 4)
            } else if remaining > 0 {
                (remaining, "repair_needed", 1)
            } else if run.global_attention() > 0 {
                (remaining, "attention_required", 0)
            } else {
                (remaining, "clean", 0)
            }
        }
        Some(after) => {
            let global = after.global.values().sum::<u64>();
            if !run.scan.complete || !after.scan.complete {
                (after.remaining_inconsistent, "blocked", 4)
            } else if after.remaining_inconsistent > 0
                || run.repairs.failed > 0
                || run.repairs.skipped_unsafe > 0
            {
                (after.remaining_inconsistent, "repair_incomplete", 3)
            } else if global > 0 {
                (after.remaining_inconsistent, "attention_required", 0)
            } else {
                (after.remaining_inconsistent, "repair_completed", 0)
            }
        }
    };

    let report = VerifyReport {
        operation: "verify-vectors",
        mode: if repair { "repair" } else { "dry_run" },
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        identity,
        scan: run.scan.clone(),
        facts: run.facts.clone(),
        episodes: run.episodes.clone(),
        global: run.global.clone(),
        duplicate_content: run.duplicate_content(),
        items: run.items.clone(),
        items_truncated: run.items_truncated,
        repairs: run.repairs.clone(),
        after,
        remaining_inconsistent: remaining,
        outcome,
        exit_code,
    };
    record_metrics(&report, &run.results);
    report
}

/// R6: the report's counts as bounded counters (fixed labels only; recorded when the process has
/// a metrics recorder installed).
fn record_metrics(
    report: &VerifyReport,
    results: &BTreeMap<(&'static str, &'static str, &'static str), u64>,
) {
    for (collection, map) in [
        ("facts", &report.facts),
        ("episodes", &report.episodes),
        ("global", &report.global),
    ] {
        for (category, n) in map {
            if *n > 0 && !NOT_DIVERGENT.contains(category) {
                metrics::counter!(VERIFY_ITEMS_METRIC, "collection" => collection, "category" => *category, "result" => "detected")
                    .increment(*n);
            }
        }
    }
    for ((collection, category, result), n) in results {
        metrics::counter!(VERIFY_ITEMS_METRIC, "collection" => *collection, "category" => *category, "result" => *result)
            .increment(*n);
    }
    let complete = report.scan.complete
        && report
            .after
            .as_ref()
            .is_none_or(|after| after.scan.complete);
    metrics::counter!(
        VERIFY_SCANS_METRIC,
        "mode" => report.mode,
        "result" => if complete { "complete" } else { "incomplete" }
    )
    .increment(1);
}
