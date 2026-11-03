use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AcquiredJobLock, BackgroundJobKind, BackgroundJobLockStore, JobLockRecord, lock_until_from_ttl,
};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Postgres, Row};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn row_to_lock(row: &PgRow) -> MemcoreResult<JobLockRecord> {
    Ok(JobLockRecord {
        kind: row
            .try_get::<String, _>("kind")
            .map_err(|error| storage_error("row kind", error))?
            .parse()?,
        owner_id: row
            .try_get("owner_id")
            .map_err(|error| storage_error("row owner_id", error))?,
        locked_until: row
            .try_get("locked_until")
            .map_err(|error| storage_error("row locked_until", error))?,
        acquired_at: row
            .try_get("acquired_at")
            .map_err(|error| storage_error("row acquired_at", error))?,
        heartbeat_at: row.try_get("heartbeat_at").ok(),
    })
}

async fn fetch_lock<'a, E>(
    executor: E,
    kind: BackgroundJobKind,
) -> MemcoreResult<Option<JobLockRecord>>
where
    E: sqlx::Executor<'a, Database = Postgres>,
{
    let row = sqlx::query(
        "SELECT kind, owner_id, locked_until, acquired_at, heartbeat_at FROM background_job_locks WHERE kind = $1",
    )
    .bind(kind.as_str())
    .fetch_optional(executor)
    .await
    .map_err(|error| storage_error("fetch postgres background job lock", error))?;

    row.map(|row| row_to_lock(&row)).transpose()
}

#[derive(Debug, Clone)]
pub struct PostgresBackgroundJobLockStore {
    pool: PgPool,
}

impl PostgresBackgroundJobLockStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|error| storage_error("connect postgres background job lock store", error))?;

        crate::migrations::postgres::run_postgres_migrations(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl BackgroundJobLockStore for PostgresBackgroundJobLockStore {
    async fn try_acquire_lock(
        &self,
        kind: BackgroundJobKind,
        owner_id: &str,
        ttl: Duration,
    ) -> MemcoreResult<Option<AcquiredJobLock>> {
        let now = Utc::now();
        let locked_until = lock_until_from_ttl(now, ttl);
        let row = sqlx::query(
            r#"
            INSERT INTO background_job_locks (
                kind, owner_id, locked_until, acquired_at, heartbeat_at, updated_at
            ) VALUES ($1, $2, $3, $4, NULL, $4)
            ON CONFLICT (kind) DO UPDATE
            SET owner_id = EXCLUDED.owner_id,
                locked_until = EXCLUDED.locked_until,
                acquired_at = EXCLUDED.acquired_at,
                heartbeat_at = NULL,
                updated_at = EXCLUDED.updated_at
            WHERE background_job_locks.locked_until <= $4
               OR background_job_locks.owner_id = $2
            RETURNING kind, owner_id, locked_until, acquired_at, heartbeat_at
            "#,
        )
        .bind(kind.as_str())
        .bind(owner_id)
        .bind(locked_until)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("acquire postgres background job lock", error))?;

        row.map(|row| row_to_lock(&row)).transpose().map(|record| {
            record.map(|record| AcquiredJobLock {
                kind: record.kind,
                owner_id: record.owner_id,
                locked_until: record.locked_until,
            })
        })
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
            SET locked_until = $1, heartbeat_at = $2, updated_at = $2
            WHERE kind = $3 AND owner_id = $4
            "#,
        )
        .bind(lock_until_from_ttl(now, ttl))
        .bind(now)
        .bind(kind.as_str())
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("renew postgres background job lock", error))?;

        Ok(result.rows_affected() > 0)
    }

    async fn release_lock(&self, kind: BackgroundJobKind, owner_id: &str) -> MemcoreResult<bool> {
        let result =
            sqlx::query("DELETE FROM background_job_locks WHERE kind = $1 AND owner_id = $2")
                .bind(kind.as_str())
                .bind(owner_id)
                .execute(&self.pool)
                .await
                .map_err(|error| storage_error("release postgres background job lock", error))?;

        Ok(result.rows_affected() > 0)
    }

    async fn get_lock(&self, kind: BackgroundJobKind) -> MemcoreResult<Option<JobLockRecord>> {
        fetch_lock(&self.pool, kind).await
    }
}
