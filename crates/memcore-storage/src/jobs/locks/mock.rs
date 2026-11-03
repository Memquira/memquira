use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AcquiredJobLock, BackgroundJobKind, BackgroundJobLockStore, JobLockRecord, lock_until_from_ttl,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

#[derive(Debug, Default)]
pub struct MockBackgroundJobLockStore {
    locks: RwLock<HashMap<BackgroundJobKind, JobLockRecord>>,
}

impl MockBackgroundJobLockStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BackgroundJobLockStore for MockBackgroundJobLockStore {
    async fn try_acquire_lock(
        &self,
        kind: BackgroundJobKind,
        owner_id: &str,
        ttl: Duration,
    ) -> MemcoreResult<Option<AcquiredJobLock>> {
        let now = Utc::now();
        let locked_until = lock_until_from_ttl(now, ttl);
        let mut locks = self
            .locks
            .write()
            .map_err(|_| storage_error("mock background job lock poisoned", "lock"))?;

        if let Some(existing) = locks.get(&kind)
            && existing.locked_until > now
            && existing.owner_id != owner_id
        {
            return Ok(None);
        }

        let record = JobLockRecord {
            kind,
            owner_id: owner_id.to_string(),
            locked_until,
            acquired_at: now,
            heartbeat_at: None,
        };
        locks.insert(kind, record);

        Ok(Some(AcquiredJobLock {
            kind,
            owner_id: owner_id.to_string(),
            locked_until,
        }))
    }

    async fn renew_lock(
        &self,
        kind: BackgroundJobKind,
        owner_id: &str,
        ttl: Duration,
    ) -> MemcoreResult<bool> {
        let now = Utc::now();
        let mut locks = self
            .locks
            .write()
            .map_err(|_| storage_error("mock background job lock poisoned", "lock"))?;
        let Some(lock) = locks.get_mut(&kind) else {
            return Ok(false);
        };
        if lock.owner_id != owner_id {
            return Ok(false);
        }

        lock.locked_until = lock_until_from_ttl(now, ttl);
        lock.heartbeat_at = Some(now);
        Ok(true)
    }

    async fn release_lock(&self, kind: BackgroundJobKind, owner_id: &str) -> MemcoreResult<bool> {
        let mut locks = self
            .locks
            .write()
            .map_err(|_| storage_error("mock background job lock poisoned", "lock"))?;
        if locks.get(&kind).map(|lock| lock.owner_id.as_str()) != Some(owner_id) {
            return Ok(false);
        }
        locks.remove(&kind);
        Ok(true)
    }

    async fn get_lock(&self, kind: BackgroundJobKind) -> MemcoreResult<Option<JobLockRecord>> {
        Ok(self
            .locks
            .read()
            .map_err(|_| storage_error("mock background job lock poisoned", "lock"))?
            .get(&kind)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    #[tokio::test]
    async fn acquire_blocks_expires_renews_releases_and_reads() {
        let store = MockBackgroundJobLockStore::new();
        let kind = BackgroundJobKind::MemoryUsageSnapshot;

        let acquired = store
            .try_acquire_lock(kind, "owner-a", Duration::from_secs(60))
            .await
            .expect("acquire")
            .expect("lock");
        assert_eq!(acquired.owner_id, "owner-a");

        let blocked = store
            .try_acquire_lock(kind, "owner-b", Duration::from_secs(60))
            .await
            .expect("blocked");
        assert!(blocked.is_none());

        let lock = store.get_lock(kind).await.expect("get").expect("record");
        assert_eq!(lock.owner_id, "owner-a");

        assert!(
            store
                .renew_lock(kind, "owner-a", Duration::from_secs(120))
                .await
                .expect("renew")
        );
        assert!(
            !store
                .renew_lock(kind, "owner-b", Duration::from_secs(120))
                .await
                .expect("renew blocked")
        );

        {
            let mut locks = store.locks.write().expect("test lock");
            let lock = locks.get_mut(&kind).expect("lock");
            lock.locked_until = Utc::now() - ChronoDuration::seconds(1);
        }

        let acquired = store
            .try_acquire_lock(kind, "owner-b", Duration::from_secs(60))
            .await
            .expect("expired acquire")
            .expect("lock");
        assert_eq!(acquired.owner_id, "owner-b");

        assert!(
            !store
                .release_lock(kind, "owner-a")
                .await
                .expect("release rejected")
        );
        assert!(store.release_lock(kind, "owner-b").await.expect("release"));
        assert!(store.get_lock(kind).await.expect("get").is_none());
    }
}
