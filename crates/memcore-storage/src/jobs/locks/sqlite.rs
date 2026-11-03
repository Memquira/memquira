use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AcquiredJobLock, BackgroundJobKind, BackgroundJobLockStore, JobLockRecord, lock_until_from_ttl,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn datetime_to_str(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn datetime_from_str(value: &str) -> MemcoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| storage_error("parse sqlite background job lock timestamp", error))
}

fn row_to_lock(row: &SqliteRow) -> MemcoreResult<JobLockRecord> {
    Ok(JobLockRecord {
        kind: row
            .try_get::<String, _>("kind")
            .map_err(|error| storage_error("row kind", error))?
            .parse()?,
        owner_id: row
            .try_get("owner_id")
            .map_err(|error| storage_error("row owner_id", error))?,
        locked_until: datetime_from_str(
            row.try_get::<String, _>("locked_until")
                .map_err(|error| storage_error("row locked_until", error))?
                .as_str(),
        )?,
        acquired_at: datetime_from_str(
            row.try_get::<String, _>("acquired_at")
                .map_err(|error| storage_error("row acquired_at", error))?
                .as_str(),
        )?,
        heartbeat_at: row
            .try_get::<Option<String>, _>("heartbeat_at")
            .ok()
            .flatten()
            .map(|value| datetime_from_str(&value))
            .transpose()?,
    })
}

async fn fetch_lock<'a, E>(
    executor: E,
    kind: BackgroundJobKind,
) -> MemcoreResult<Option<JobLockRecord>>
where
    E: sqlx::Executor<'a, Database = Sqlite>,
{
    let row = sqlx::query(
        "SELECT kind, owner_id, locked_until, acquired_at, heartbeat_at FROM background_job_locks WHERE kind = ?",
    )
    .bind(kind.as_str())
    .fetch_optional(executor)
    .await
    .map_err(|error| storage_error("fetch sqlite background job lock", error))?;

    row.map(|row| row_to_lock(&row)).transpose()
}

#[derive(Debug, Clone)]
pub struct SqliteBackgroundJobLockStore {
    pool: SqlitePool,
}

impl SqliteBackgroundJobLockStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl BackgroundJobLockStore for SqliteBackgroundJobLockStore {
    async fn try_acquire_lock(
        &self,
        kind: BackgroundJobKind,
        owner_id: &str,
        ttl: Duration,
    ) -> MemcoreResult<Option<AcquiredJobLock>> {
        let now = Utc::now();
        let locked_until = lock_until_from_ttl(now, ttl);
        let now_text = datetime_to_str(now);
        let locked_until_text = datetime_to_str(locked_until);
        let mut tx = self.pool.begin().await.map_err(|error| {
            storage_error("begin sqlite background job lock transaction", error)
        })?;

        let existing = fetch_lock(&mut *tx, kind).await?;
        if let Some(existing) = existing {
            if existing.locked_until > now && existing.owner_id != owner_id {
                tx.commit().await.map_err(|error| {
                    storage_error("commit sqlite background job lock transaction", error)
                })?;
                return Ok(None);
            }

            sqlx::query(
                r#"
                UPDATE background_job_locks
                SET owner_id = ?, locked_until = ?, acquired_at = ?, heartbeat_at = NULL, updated_at = ?
                WHERE kind = ?
                "#,
            )
            .bind(owner_id)
            .bind(&locked_until_text)
            .bind(&now_text)
            .bind(&now_text)
            .bind(kind.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| storage_error("update sqlite background job lock", error))?;
        } else {
            sqlx::query(
                r#"
                INSERT INTO background_job_locks (
                    kind, owner_id, locked_until, acquired_at, heartbeat_at, updated_at
                ) VALUES (?, ?, ?, ?, NULL, ?)
                "#,
            )
            .bind(kind.as_str())
            .bind(owner_id)
            .bind(&locked_until_text)
            .bind(&now_text)
            .bind(&now_text)
            .execute(&mut *tx)
            .await
            .map_err(|error| storage_error("insert sqlite background job lock", error))?;
        }

        tx.commit().await.map_err(|error| {
            storage_error("commit sqlite background job lock transaction", error)
        })?;
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
        let result = sqlx::query(
            r#"
            UPDATE background_job_locks
            SET locked_until = ?, heartbeat_at = ?, updated_at = ?
            WHERE kind = ? AND owner_id = ?
            "#,
        )
        .bind(datetime_to_str(lock_until_from_ttl(now, ttl)))
        .bind(datetime_to_str(now))
        .bind(datetime_to_str(now))
        .bind(kind.as_str())
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("renew sqlite background job lock", error))?;

        Ok(result.rows_affected() > 0)
    }

    async fn release_lock(&self, kind: BackgroundJobKind, owner_id: &str) -> MemcoreResult<bool> {
        let result =
            sqlx::query("DELETE FROM background_job_locks WHERE kind = ? AND owner_id = ?")
                .bind(kind.as_str())
                .bind(owner_id)
                .execute(&self.pool)
                .await
                .map_err(|error| storage_error("release sqlite background job lock", error))?;

        Ok(result.rows_affected() > 0)
    }

    async fn get_lock(&self, kind: BackgroundJobKind) -> MemcoreResult<Option<JobLockRecord>> {
        fetch_lock(&self.pool, kind).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    async fn store() -> SqliteBackgroundJobLockStore {
        SqliteBackgroundJobLockStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite lock store")
    }

    #[tokio::test]
    async fn sqlite_lock_lifecycle_works() {
        let store = store().await;
        let kind = BackgroundJobKind::MemoryUsageSnapshot;
        let acquired = store
            .try_acquire_lock(kind, "owner-a", Duration::from_secs(60))
            .await
            .expect("acquire")
            .expect("lock");
        assert_eq!(acquired.owner_id, "owner-a");

        assert!(
            store
                .try_acquire_lock(kind, "owner-b", Duration::from_secs(60))
                .await
                .expect("blocked")
                .is_none()
        );
        assert!(
            !store
                .release_lock(kind, "owner-b")
                .await
                .expect("other release")
        );
        assert!(
            store
                .renew_lock(kind, "owner-a", Duration::from_secs(120))
                .await
                .expect("renew")
        );

        let lock = store.get_lock(kind).await.expect("get").expect("record");
        assert_eq!(lock.owner_id, "owner-a");
        assert!(lock.heartbeat_at.is_some());

        sqlx::query("UPDATE background_job_locks SET locked_until = ? WHERE kind = ?")
            .bind(datetime_to_str(Utc::now() - ChronoDuration::seconds(1)))
            .bind(kind.as_str())
            .execute(&store.pool)
            .await
            .expect("expire lock");

        let acquired = store
            .try_acquire_lock(kind, "owner-b", Duration::from_secs(60))
            .await
            .expect("expired acquire")
            .expect("lock");
        assert_eq!(acquired.owner_id, "owner-b");

        assert!(store.release_lock(kind, "owner-b").await.expect("release"));
        assert!(store.get_lock(kind).await.expect("get").is_none());
    }
}
