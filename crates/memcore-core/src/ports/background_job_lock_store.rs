use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;

use crate::jobs::BackgroundJobKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLockKey {
    pub kind: BackgroundJobKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLockRecord {
    pub kind: BackgroundJobKind,
    pub owner_id: String,
    pub locked_until: DateTime<Utc>,
    pub acquired_at: DateTime<Utc>,
    pub heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquiredJobLock {
    pub kind: BackgroundJobKind,
    pub owner_id: String,
    pub locked_until: DateTime<Utc>,
}

#[async_trait]
pub trait BackgroundJobLockStore: Send + Sync {
    async fn try_acquire_lock(
        &self,
        kind: BackgroundJobKind,
        owner_id: &str,
        ttl: Duration,
    ) -> MemcoreResult<Option<AcquiredJobLock>>;

    async fn renew_lock(
        &self,
        kind: BackgroundJobKind,
        owner_id: &str,
        ttl: Duration,
    ) -> MemcoreResult<bool>;

    async fn release_lock(&self, kind: BackgroundJobKind, owner_id: &str) -> MemcoreResult<bool>;

    async fn get_lock(&self, kind: BackgroundJobKind) -> MemcoreResult<Option<JobLockRecord>>;
}

pub fn lock_until_from_ttl(now: DateTime<Utc>, ttl: Duration) -> DateTime<Utc> {
    let ttl = chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(0));
    now + ttl
}
