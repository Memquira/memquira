use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;

use crate::admin::usage::MemoryUsageSnapshot;
use crate::pagination::PageCursor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryUsageSnapshotQuery {
    pub org_id: String,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryUsageSnapshotQueryResult {
    pub snapshots: Vec<MemoryUsageSnapshot>,
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait MemoryUsageSnapshotStore: Send + Sync {
    async fn insert_snapshot(
        &self,
        snapshot: MemoryUsageSnapshot,
    ) -> MemcoreResult<MemoryUsageSnapshot>;

    async fn query_snapshots(
        &self,
        query: MemoryUsageSnapshotQuery,
    ) -> MemcoreResult<MemoryUsageSnapshotQueryResult>;
}
