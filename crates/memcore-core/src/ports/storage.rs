use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::pagination::PageCursor;
use crate::search::FulltextQuery;
use crate::{Fact, MemoryType, TenantContext};

/// What a vector write claims about points that may already exist for its fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorWriteIntent {
    /// The fact was created by this same operation with a newly allocated fact id, and this
    /// record's point id was allocated once for it: no other point of this fact can exist.
    FreshFact,
    /// The point id equals the fact id (episodes): an upsert overwrites the only point the fact
    /// can have.
    StablePointId,
    /// The fact may already have vectors (update, re-embed, import, anything unproven): every
    /// existing vector of the fact is replaced.
    Replace,
}

/// A ranked full-text query over one tenant's active facts.
#[derive(Debug, Clone)]
pub struct FactFulltextQuery {
    pub tenant: TenantContext,
    pub memory_types: Option<Vec<MemoryType>>,
    pub query: FulltextQuery,
    pub limit: usize,
}

/// A fact with its full-text relevance; higher is better. Scores are only comparable within one
/// query on one backend (SQLite BM25, Postgres `ts_rank_cd`, or term overlap in the mock).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredFact {
    pub fact: Fact,
    pub score: f32,
}

/// Result of a fact retention prune operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPruneResult {
    /// Number of facts matched (dry-run) or soft-deleted (apply).
    pub count: usize,
    /// Fact IDs soft-deleted on apply; empty on dry-run.
    pub fact_ids: Vec<Uuid>,
}

/// Per-user aggregate for organization admin listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgUserSummary {
    pub user_id: String,
    pub memory_count: usize,
    pub last_memory_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactSearchQuery {
    pub tenant: TenantContext,
    pub memory_types: Option<Vec<MemoryType>>,
    pub query_text: Option<String>,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
    pub include_deleted: bool,
}

impl FactSearchQuery {
    pub fn new(tenant: TenantContext, limit: usize) -> Self {
        Self {
            tenant,
            memory_types: None,
            query_text: None,
            limit,
            cursor: None,
            include_deleted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorRecord {
    pub id: Uuid,
    pub fact_id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub embedding: Vec<f32>,
    pub content: String,
    pub memory_type: MemoryType,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchQuery {
    pub tenant: TenantContext,
    pub embedding: Vec<f32>,
    pub limit: usize,
    pub memory_types: Option<Vec<MemoryType>>,
    pub metadata_filter: Option<Value>,
}

/// One stored vector point of a fact, as a store reports it (write verification).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPointState {
    pub point_id: String,
    pub content: String,
    pub memory_type: MemoryType,
}

/// `verify-vectors`: which points a verification scroll or count covers.
#[derive(Debug, Clone, PartialEq)]
pub enum PointScope {
    /// Points whose payload names exactly this organization and user.
    Tenant(TenantContext),
    /// The tenant's points whose payload entity id (`fact_id`; the episode id for episodes) is
    /// this id.
    TenantEntity(TenantContext, Uuid),
    /// Points with no usable tenant payload (`org_id` or `user_id` missing or empty). Maintenance
    /// discovery only: the caller learns point ids, never content.
    Unattributed,
}

/// One point as a verification scroll reports it. Payload fields are optional because a damaged
/// or foreign point may lack them; nothing is inferred. `content` is `None` for
/// [`PointScope::Unattributed`] (metadata only).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPoint {
    /// The store's point id as text: a lowercase UUID or a decimal number.
    pub point_id: String,
    /// The payload `fact_id` as stored (it may not parse).
    pub entity_id: Option<String>,
    pub org_id: Option<String>,
    pub user_id: Option<String>,
    pub content: Option<String>,
    /// `None` when the payload type is missing or unknown.
    pub memory_type: Option<MemoryType>,
}

/// One page of a verification scroll; `next_cursor` is `None` after the last page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PointPage {
    pub points: Vec<StoredPoint>,
    pub next_cursor: Option<String>,
}

/// What is known about a collection's embedding identity, read without any
/// write and (in dry runs) without any embedding call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorIdentityStatus {
    /// A recorded identity equals the configured provider's (and, when probed, its fingerprint).
    Verified,
    /// No identity record (a legacy, D-L3 store): re-embedding into it cannot be proven safe.
    Unrecorded,
    /// The record, the vector size or the fingerprint disagrees with the configured provider.
    Mismatch,
    /// Not determined (no provider configured, the collection is absent, or the check failed).
    Unavailable,
}

impl VectorIdentityStatus {
    /// Only a verified identity allows writing newly embedded vectors.
    pub fn allows_embedding(self) -> bool {
        self == Self::Verified
    }
}

/// The point-id order of the Qdrant store's replacement (`replace_reusing_point`): numeric ids
/// (zero-padded) before UUIDs (lowercase). Used to keep "the smallest id" consistently.
pub fn point_id_order_key(point_id: &str) -> String {
    match point_id.parse::<u64>() {
        Ok(num) => format!("n:{num:020}"),
        Err(_) => format!("u:{}", point_id.to_ascii_lowercase()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub fact_id: Uuid,
    pub content: String,
    pub score: f32,
    pub memory_type: MemoryType,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgUserListQuery {
    pub org_id: String,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

/// A fact row of one tenant read regardless of soft deletion, with its deletion state
/// (`FactStore::get_fact_including_deleted`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFactState {
    pub fact: Fact,
    /// True when the row is soft-deleted (`deleted_at` set).
    pub deleted: bool,
}

/// The outcome of [`FactStore::insert_fact_deduplicated`].
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum DedupInsert {
    Inserted(Fact),
    /// An active fact (not deleted, superseded or invalid) of the same tenant already holds the key
    /// (e.g. a concurrent equivalent addition committed first); nothing was written.
    Duplicate {
        existing_fact_id: Uuid,
    },
}

#[async_trait]
pub trait FactStore: Send + Sync {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact>;

    /// Inserts `fact` recording `dedup_key` (`dedup::exact_dedup_key` under the
    /// calling ingestion mode's `ExactDedupRule`) unless an
    /// active fact (not deleted, superseded or invalid) of the same tenant already holds that key. The SQL stores decide this
    /// atomically (a partial unique index, migration 15), so concurrent equivalent additions from
    /// any number of API processes store one fact. Only ingestion ADDs write a key; imports and
    /// `update_fact` never leave one on a row. The default (stores without the column, test
    /// wrappers) is a plain [`Self::insert_fact`].
    async fn insert_fact_deduplicated(
        &self,
        tenant: &TenantContext,
        fact: Fact,
        dedup_key: &str,
    ) -> MemcoreResult<DedupInsert> {
        let _ = dedup_key;
        self.insert_fact(tenant, fact)
            .await
            .map(DedupInsert::Inserted)
    }

    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact>;

    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>>;

    /// The tenant's fact with this
    /// id whether or not it is soft-deleted, with its deletion state; `None` when no row of THIS
    /// tenant (organization and user) has the id, so another tenant's row is indistinguishable
    /// from an absent one. Only the delete and import paths use it: search, list, context,
    /// candidates and GET keep the live-only `get_fact`.
    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>>;

    /// One page of the tenant's fact rows, live and soft-deleted, with their
    /// state, in ascending id order and strictly after `after` (keyset pagination; at most `limit`).
    async fn list_fact_states(
        &self,
        tenant: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>>;

    /// Maintenance only (never reachable from an API route): whether ANY
    /// tenant has a fact row (live or deleted) with this id. Fact ids are global keys, so a vector
    /// that looks orphaned for one tenant may name another tenant's fact; the verifier must not
    /// delete it then. Answers existence only, never content or the owning tenant.
    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool>;

    async fn search_facts(&self, query: FactSearchQuery) -> MemcoreResult<Vec<Fact>>;

    /// Active facts matching any term of `query`, best first, at most `query.limit`. An empty
    /// query returns no facts.
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>>;

    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()>;

    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()>;

    /// Soft-deletes active facts with `updated_at` older than `cutoff` for the tenant.
    /// When `dry_run` is true, counts matches without deleting.
    async fn delete_facts_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<RetentionPruneResult>;

    /// Counts stored (non-deleted) facts for an organization: the capacity count quotas use
    /// . Invalidated and superseded facts are included; they still take storage.
    async fn count_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize>;

    /// Counts active facts for an organization: not deleted, and neither `invalid_at` nor
    /// `superseded_at` set. Never used for quotas.
    async fn count_active_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize>;

    /// Counts active (non-deleted) facts for one tenant user.
    async fn count_facts_by_user(&self, tenant: &TenantContext) -> MemcoreResult<usize>;

    /// Counts distinct users with at least one active fact in the organization.
    async fn count_users_by_org(&self, org_id: &str) -> MemcoreResult<usize>;

    /// Lists users with memory aggregates for an organization.
    async fn list_users_by_org(
        &self,
        query: OrgUserListQuery,
    ) -> MemcoreResult<Vec<OrgUserSummary>>;
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Writes `record` so that it is the only vector of `record.fact_id` for the tenant (stores
    /// that key points by fact replace; others insert by point id).
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()>;

    /// Writes `record` with an explicit claim about existing points of its fact. The
    /// default keeps the earlier behaviour of every store: `Replace` deletes the fact's vectors
    /// (a missing one is fine) and upserts; the other intents upsert. A store may only use the
    /// claim to skip work that cannot change the result.
    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        if intent == VectorWriteIntent::Replace {
            match self.delete_by_fact_id(tenant, record.fact_id).await {
                Ok(()) | Err(memcore_common::MemcoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.upsert_vector(tenant, record).await
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>>;

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()>;

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()>;

    /// Every point the tenant has for `fact_id` (an UPDATE verifies the shared
    /// store after writing). `None` = this store cannot report points, so no verification runs.
    async fn fact_vector_points(
        &self,
        _tenant: &TenantContext,
        _fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        Ok(None)
    }

    /// One page (at most `limit` points, ascending point id) of the points in
    /// `scope`, continuing after `cursor`; read-only. A store that cannot enumerate fails closed,
    /// so a verification is reported blocked, never clean.
    async fn scroll_points(
        &self,
        _scope: &PointScope,
        _cursor: Option<String>,
        _limit: usize,
    ) -> MemcoreResult<PointPage> {
        Err(memcore_common::MemcoreError::ValidationError(
            "this vector store cannot enumerate points for verification".to_string(),
        ))
    }

    /// The exact number of points in `scope` (the completeness check of a
    /// scroll); read-only.
    async fn count_points(&self, _scope: &PointScope) -> MemcoreResult<u64> {
        Err(memcore_common::MemcoreError::ValidationError(
            "this vector store cannot count points for verification".to_string(),
        ))
    }

    /// Deletes exactly these points, and only those of them whose payload
    /// names `tenant` (a point of any other tenant, or without tenant payload, is never touched).
    async fn delete_points(
        &self,
        _tenant: &TenantContext,
        _point_ids: &[String],
    ) -> MemcoreResult<()> {
        Err(memcore_common::MemcoreError::ValidationError(
            "this vector store cannot delete points by id".to_string(),
        ))
    }
}
