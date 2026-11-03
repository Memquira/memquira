//! The durable write-ahead journal of in-flight ingestion requests.
//!
//! An add request records each fact operation here BEFORE it applies it (the fact id of an ADD is
//! allocated first; an UPDATE or a supersession keeps the target's before-image). The request
//! commits (deletes its journal rows) only after every operation it applied is complete, just
//! before it answers success. Anything still in the journal therefore belongs to a request that has
//! not succeeded: it failed, was cancelled (client disconnect, request deadline) or its process
//! stopped. Such an operation is rolled back, by the request itself, by the task its cancellation
//! spawns, or by the recovery sweep once it is stale (after a crash or restart).
//!
//! Invariant the engine relies on: an active fact written by ingestion is either committed or
//! named by a step of an operation in this journal. A request that would rely on another
//! request's uncommitted fact (an exact or embedding duplicate, an update or supersession target)
//! waits for that operation to resolve first.
//!
//! Rows hold tenant ids and, for updates and supersessions, the target's before-image (memory
//! content): they live only while the request is in flight or being rolled back, and forget-user
//! removes them.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Fact, TenantContext};

/// One fact operation of an in-flight request, recorded before it is applied. Stored as JSON
/// (tagged by `kind`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum IngestJournalStep {
    /// A fact this request is about to add, with its pre-allocated id. Undo: soft-delete the row
    /// and remove its vector (and entity links).
    Add { fact_id: Uuid },
    /// An in-place update of `before.id`. Undo: when the row's text changed, restore `before`
    /// (row and vector).
    Update { before: Fact },
    /// The target `before.id` is marked superseded and, when `replacement_fact_id` is set, linked
    /// to a fact this request adds with that id. Undo: restore the target's row, then remove the
    /// replacement as an ADD.
    Supersede {
        before: Fact,
        replacement_fact_id: Option<Uuid>,
    },
    /// A soft delete of `fact_id`. A deleted row cannot be restored, so it is rolled forward: its
    /// vector is removed when the row is deleted.
    Delete { fact_id: Uuid },
}

impl IngestJournalStep {
    /// The fixed `kind` label of the step (storage column and metric label).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Add { .. } => "add",
            Self::Update { .. } => "update",
            Self::Supersede { .. } => "supersede",
            Self::Delete { .. } => "delete",
        }
    }

    /// Every fact id the step writes, for the in-flight lookup.
    pub fn fact_ids(&self) -> Vec<Uuid> {
        match self {
            Self::Add { fact_id } | Self::Delete { fact_id } => vec![*fact_id],
            Self::Update { before } => vec![before.id],
            Self::Supersede {
                before,
                replacement_fact_id,
            } => std::iter::once(before.id)
                .chain(*replacement_fact_id)
                .collect(),
        }
    }
}

/// Every step `kind` value.
pub const INGEST_JOURNAL_STEP_KINDS: &[&str] = &["add", "update", "supersede", "delete"];

/// The state of a journalled operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOperationStatus {
    /// The request is (or was, until it stopped) applying its operations.
    InProgress,
    /// A rollback claimed it; a rollback that did not finish is retried once stale.
    RollingBack,
}

impl IngestOperationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::RollingBack => "rolling_back",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "in_progress" => Some(Self::InProgress),
            "rolling_back" => Some(Self::RollingBack),
            _ => None,
        }
    }
}

/// A journalled operation with its steps in the order they were recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestOperation {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub status: IngestOperationStatus,
    /// Rollback claims so far.
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    /// Last journal write of the request, or of its rollback.
    pub updated_at: DateTime<Utc>,
    pub steps: Vec<IngestJournalStep>,
}

impl IngestOperation {
    pub fn tenant(&self) -> MemcoreResult<TenantContext> {
        TenantContext::new(self.org_id.clone(), self.user_id.clone())
    }
}

/// closure: the durable record of a request that succeeded, keyed by a fingerprint of
/// its stable identity (every message's client id and content, the session and the metadata), so
/// a retry of that exact request after a lost response returns the recorded result instead of
/// running extraction again. `result` is engine-defined JSON holding counts and fact ids only (no
/// fact text). Kept for a bounded time (the recovery sweep purges old receipts) and erased by
/// forget-user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReceipt {
    pub request_key: String,
    pub operation_id: Option<Uuid>,
    pub committed_at: DateTime<Utc>,
    pub result: String,
}

/// closure: the journal backlog (readiness detail and gauges).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IngestJournalBacklog {
    /// Operations in the journal (in flight or being rolled back).
    pub pending: usize,
    /// Of those, operations a rollback claimed.
    pub rolling_back: usize,
    /// The oldest last journal write.
    pub oldest_updated_at: Option<DateTime<Utc>>,
    /// The most rollback claims of one operation.
    pub max_attempts: u32,
}

/// The state of one operation, without its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOperationState {
    pub status: IngestOperationStatus,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait IngestJournalStore: Send + Sync {
    /// Creates the operation `operation_id` of `tenant` (status in progress) with its first step,
    /// atomically.
    async fn begin(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        step: &IngestJournalStep,
    ) -> MemcoreResult<()>;

    /// Appends `step` and refreshes the operation's `updated_at`, atomically, only while the
    /// operation is in progress. `Conflict` when a rollback already claimed it.
    async fn append(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        step: &IngestJournalStep,
    ) -> MemcoreResult<()>;

    /// Deletes the operation and its steps when it is still in progress (the request succeeded),
    /// and records `receipt` (when given) in the same transaction; an existing receipt with the
    /// same key is kept. `Conflict` when a rollback already claimed it: the request must not
    /// report success.
    async fn commit(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        receipt: Option<&IngestReceipt>,
    ) -> MemcoreResult<()>;

    /// Records the receipt of a request that journalled no operation (an existing one is kept).
    async fn record_receipt(
        &self,
        tenant: &TenantContext,
        receipt: &IngestReceipt,
    ) -> MemcoreResult<()>;

    /// The receipt of `request_key` for `tenant`, if one is recorded.
    async fn find_receipt(
        &self,
        tenant: &TenantContext,
        request_key: &str,
    ) -> MemcoreResult<Option<IngestReceipt>>;

    /// Maintenance (the recovery sweep): deletes receipts committed before `committed_before`, of
    /// any tenant. Returns how many.
    async fn purge_receipts(&self, committed_before: DateTime<Utc>) -> MemcoreResult<usize>;

    /// Claims an operation for rollback (status rolling back, attempts + 1, `updated_at` now) and
    /// returns it with its steps. Without `stale_before` only an operation in progress is claimed
    /// (the request's own rollback); with it, an operation of either status whose `updated_at` is
    /// older (the recovery sweep, also resuming a rollback that did not finish). `None` when
    /// nothing was claimed: unknown, committed, or claimed by someone else.
    async fn claim(
        &self,
        operation_id: Uuid,
        stale_before: Option<DateTime<Utc>>,
    ) -> MemcoreResult<Option<IngestOperation>>;

    /// Deletes a rolled-back operation and its steps.
    async fn finish(&self, operation_id: Uuid) -> MemcoreResult<()>;

    /// Records that a claimed rollback did not finish: `updated_at` becomes now, so the sweep
    /// retries it once it is stale again.
    async fn release(&self, operation_id: Uuid) -> MemcoreResult<()>;

    /// Maintenance only (the recovery sweep; never reachable from an API route): ids of operations
    /// of any tenant whose `updated_at` is older than `stale_before`, oldest first, at most `limit`.
    async fn list_stale(
        &self,
        stale_before: DateTime<Utc>,
        limit: usize,
    ) -> MemcoreResult<Vec<Uuid>>;

    /// An operation of `tenant` other than `exclude` with a step that writes `fact_id`.
    async fn find_in_flight(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
        exclude: Uuid,
    ) -> MemcoreResult<Option<Uuid>>;

    /// The state of an operation; `None` once it committed or finished rolling back.
    async fn operation_state(
        &self,
        operation_id: Uuid,
    ) -> MemcoreResult<Option<IngestOperationState>>;

    /// Operations in the journal (the backlog gauge).
    async fn count_operations(&self) -> MemcoreResult<usize>;

    /// The backlog's size, age and retry depth.
    async fn backlog(&self) -> MemcoreResult<IngestJournalBacklog>;

    /// Forget-user: deletes every operation of `tenant` with its steps, and the tenant's receipts.
    /// Returns the operations deleted.
    async fn delete_user_operations(&self, tenant: &TenantContext) -> MemcoreResult<usize>;
}
