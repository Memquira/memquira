use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use uuid::Uuid;

use crate::pagination::PageCursor;
use crate::{MemoryEvent, MemoryEventOperation, TenantContext};

/// Default limit for listing memory audit events.
pub const DEFAULT_MEMORY_EVENT_LIST_LIMIT: usize = 50;

/// Maximum allowed limit for listing memory audit events.
pub const MAX_MEMORY_EVENT_LIST_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEventQuery {
    pub tenant: TenantContext,
    pub fact_id: Option<Uuid>,
    pub operation: Option<MemoryEventOperation>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub query_text: Option<String>,
    /// Page size requested by the caller. Stores over-fetch: see [`MemoryEventStore::list_events`].
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

impl MemoryEventQuery {
    pub fn new(tenant: TenantContext, limit: usize) -> Self {
        Self {
            tenant,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit,
            cursor: None,
        }
    }
}

/// Validates that `created_after` is strictly earlier than `created_before` when both are set.
pub fn validate_event_date_range(
    created_after: Option<DateTime<Utc>>,
    created_before: Option<DateTime<Utc>>,
) -> memcore_common::MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if let (Some(after), Some(before)) = (created_after, created_before)
        && after >= before
    {
        return Err(MemcoreError::ValidationError(
            "created_after must be earlier than created_before".to_string(),
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgMemoryEventQuery {
    pub org_id: String,
    pub user_id: Option<String>,
    pub fact_id: Option<Uuid>,
    pub operation: Option<MemoryEventOperation>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub query_text: Option<String>,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

impl OrgMemoryEventQuery {
    pub fn new(org_id: String, limit: usize) -> Self {
        Self {
            org_id,
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit,
            cursor: None,
        }
    }
}

#[async_trait]
pub trait MemoryEventStore: Send + Sync {
    async fn record_event(
        &self,
        tenant: &TenantContext,
        event: MemoryEvent,
    ) -> MemcoreResult<MemoryEvent>;

    /// Lists a user's events newest first (`created_at DESC, id DESC`), after `query.cursor`.
    ///
    /// Returns up to `page_fetch_limit(query.limit)` = `limit + 1` rows: the extra row is a
    /// look-ahead that tells the caller another page exists. Callers trim it with `build_page`, which
    /// returns exactly `limit` items plus `next_cursor` (only when the look-ahead row was present).
    async fn list_events(&self, query: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>>;

    /// Lists memory audit events for an organization with optional filters.
    async fn list_events_by_org(
        &self,
        query: OrgMemoryEventQuery,
    ) -> MemcoreResult<Vec<MemoryEvent>>;

    /// Hard-deletes memory audit events with `created_at` older than `cutoff` for the tenant.
    /// When `dry_run` is true, counts matches without deleting.
    async fn delete_events_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize>;

    /// Counts memory audit events for an organization.
    async fn count_events_by_org(&self, org_id: &str) -> MemcoreResult<usize>;

    /// Hard-deletes every audit event of exactly this (org, user) except
    /// content-free forget-user tombstones (operation `forget_user` with no input, previous or new
    /// content). Events carry fact text (`previous_content` / `new_content`) and free-form metadata,
    /// so forget-user erases them. Returns how many were deleted.
    async fn erase_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize>;

    /// Events of exactly this (org, user) other than content-free forget-user
    /// tombstones; 0 after a successful erasure.
    async fn count_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize>;
}
