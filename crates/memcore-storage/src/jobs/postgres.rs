use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::pagination::{PageCursor, build_page};
use memcore_core::{
    BackgroundJobRunQuery, BackgroundJobRunQueryResult, BackgroundJobRunStore,
    StoredBackgroundJobRun, validate_background_job_run_limit,
};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Postgres, QueryBuilder, Row};

use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn row_to_run(row: &PgRow) -> MemcoreResult<StoredBackgroundJobRun> {
    Ok(StoredBackgroundJobRun {
        id: row
            .try_get("id")
            .map_err(|error| storage_error("row id", error))?,
        kind: row
            .try_get::<String, _>("kind")
            .map_err(|error| storage_error("row kind", error))?
            .parse()?,
        status: row
            .try_get::<String, _>("status")
            .map_err(|error| storage_error("row status", error))?
            .parse()?,
        started_at: row
            .try_get("started_at")
            .map_err(|error| storage_error("row started_at", error))?,
        finished_at: row.try_get("finished_at").ok(),
        duration_ms: row
            .try_get::<Option<i64>, _>("duration_ms")
            .ok()
            .flatten()
            .map(|value| value as u64),
        attempt_count: row
            .try_get::<i64, _>("attempt_count")
            .ok()
            .filter(|value| *value > 0)
            .map(|value| value as usize)
            .unwrap_or(1),
        max_attempts: row
            .try_get::<i64, _>("max_attempts")
            .ok()
            .filter(|value| *value > 0)
            .map(|value| value as usize)
            .unwrap_or(1),
        retried: row.try_get::<bool, _>("retried").ok().unwrap_or(false),
        error_code: row.try_get("error_code").ok(),
        error_message: row.try_get("error_message").ok(),
        metadata: row.try_get::<Option<Value>, _>("metadata").ok().flatten(),
    })
}

fn push_filters(builder: &mut QueryBuilder<Postgres>, query: &BackgroundJobRunQuery) {
    builder.push(" WHERE 1 = 1");
    if let Some(kind) = query.kind {
        builder.push(" AND kind = ");
        builder.push_bind(kind.as_str());
    }
    if let Some(status) = query.status {
        builder.push(" AND status = ");
        builder.push_bind(status.as_str());
    }
    if let Some(created_after) = query.created_after {
        builder.push(" AND started_at >= ");
        builder.push_bind(created_after);
    }
    if let Some(created_before) = query.created_before {
        builder.push(" AND started_at < ");
        builder.push_bind(created_before);
    }
}

#[derive(Debug, Clone)]
pub struct PostgresBackgroundJobRunStore {
    pool: PgPool,
}

impl PostgresBackgroundJobRunStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|error| storage_error("connect postgres background job run store", error))?;

        crate::migrations::postgres::run_postgres_migrations(&pool).await?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
}

#[async_trait]
impl BackgroundJobRunStore for PostgresBackgroundJobRunStore {
    async fn insert_run(
        &self,
        run: StoredBackgroundJobRun,
    ) -> MemcoreResult<StoredBackgroundJobRun> {
        sqlx::query(
            r#"
            INSERT INTO background_job_runs (
                id, kind, status, started_at, finished_at, duration_ms,
                attempt_count, max_attempts, retried,
                error_code, error_message, metadata, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(run.id)
        .bind(run.kind.as_str())
        .bind(run.status.as_str())
        .bind(run.started_at)
        .bind(run.finished_at)
        .bind(run.duration_ms.map(|value| value as i64))
        .bind(run.attempt_count as i64)
        .bind(run.max_attempts as i64)
        .bind(run.retried)
        .bind(run.error_code.as_deref())
        .bind(run.error_message.as_deref())
        .bind(run.metadata.as_ref())
        .bind(run.started_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("insert postgres background job run", error))?;

        Ok(run)
    }

    async fn query_runs(
        &self,
        query: BackgroundJobRunQuery,
    ) -> MemcoreResult<BackgroundJobRunQueryResult> {
        let limit = validate_background_job_run_limit(query.limit)?;
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, kind, status, started_at, finished_at, duration_ms, attempt_count, max_attempts, retried, error_code, error_message, metadata FROM background_job_runs",
        );
        push_filters(&mut builder, &query);

        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "started_at", "id", cursor);
        }

        builder.push(" ORDER BY started_at DESC, id DESC LIMIT ");
        builder.push_bind(fetch_limit(limit) as i64);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("query postgres background job runs", error))?;
        let runs = rows
            .iter()
            .map(row_to_run)
            .collect::<MemcoreResult<Vec<_>>>()?;
        let page = build_page(runs, limit, |run| PageCursor {
            last_id: run.id.to_string(),
            last_sort_value: run.started_at,
        })?;

        Ok(BackgroundJobRunQueryResult {
            runs: page.items,
            next_cursor: page.next_cursor,
        })
    }

    async fn delete_runs_older_than(
        &self,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize> {
        if dry_run {
            let row = sqlx::query(
                "SELECT COUNT(*)::bigint AS count FROM background_job_runs WHERE started_at < $1",
            )
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                storage_error("count postgres background job runs for retention", error)
            })?;
            let count: i64 = row
                .try_get("count")
                .map_err(|error| storage_error("row count", error))?;
            return Ok(count as usize);
        }

        let result = sqlx::query("DELETE FROM background_job_runs WHERE started_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|error| {
                storage_error("delete postgres background job runs for retention", error)
            })?;

        Ok(result.rows_affected() as usize)
    }
}
