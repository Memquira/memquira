use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::pagination::{PageCursor, build_page};
use memcore_core::{
    BackgroundJobRunQuery, BackgroundJobRunQueryResult, BackgroundJobRunStore,
    StoredBackgroundJobRun, validate_background_job_run_limit,
};
use serde_json::Value;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::pagination::{fetch_limit, push_sqlite_desc_cursor};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn datetime_to_str(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn datetime_from_str(value: &str) -> MemcoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| storage_error("parse sqlite background job timestamp", error))
}

fn metadata_to_str(value: &Option<Value>) -> MemcoreResult<Option<String>> {
    match value {
        Some(metadata) => Ok(Some(serde_json::to_string(metadata).map_err(|error| {
            storage_error("serialize background job run metadata", error)
        })?)),
        None => Ok(None),
    }
}

fn metadata_from_str(value: Option<String>) -> MemcoreResult<Option<Value>> {
    match value {
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => Ok(Some(serde_json::from_str(&raw).map_err(|error| {
            storage_error("deserialize background job run metadata", error)
        })?)),
        None => Ok(None),
    }
}

fn row_to_run(row: &SqliteRow) -> MemcoreResult<StoredBackgroundJobRun> {
    Ok(StoredBackgroundJobRun {
        id: Uuid::parse_str(
            row.try_get::<String, _>("id")
                .map_err(|error| storage_error("row id", error))?
                .as_str(),
        )
        .map_err(|error| storage_error("row id uuid", error))?,
        kind: row
            .try_get::<String, _>("kind")
            .map_err(|error| storage_error("row kind", error))?
            .parse()?,
        status: row
            .try_get::<String, _>("status")
            .map_err(|error| storage_error("row status", error))?
            .parse()?,
        started_at: datetime_from_str(
            row.try_get::<String, _>("started_at")
                .map_err(|error| storage_error("row started_at", error))?
                .as_str(),
        )?,
        finished_at: row
            .try_get::<Option<String>, _>("finished_at")
            .ok()
            .flatten()
            .map(|value| datetime_from_str(&value))
            .transpose()?,
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
        retried: row
            .try_get::<bool, _>("retried")
            .or_else(|_| row.try_get::<i64, _>("retried").map(|value| value != 0))
            .unwrap_or(false),
        error_code: row.try_get("error_code").ok(),
        error_message: row.try_get("error_message").ok(),
        metadata: metadata_from_str(row.try_get("metadata").ok())?,
    })
}

fn push_filters(builder: &mut QueryBuilder<Sqlite>, query: &BackgroundJobRunQuery) {
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
        builder.push_bind(datetime_to_str(created_after));
    }
    if let Some(created_before) = query.created_before {
        builder.push(" AND started_at < ");
        builder.push_bind(datetime_to_str(created_before));
    }
}

#[derive(Debug, Clone)]
pub struct SqliteBackgroundJobRunStore {
    pool: SqlitePool,
}

impl SqliteBackgroundJobRunStore {
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
impl BackgroundJobRunStore for SqliteBackgroundJobRunStore {
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(run.id.to_string())
        .bind(run.kind.as_str())
        .bind(run.status.as_str())
        .bind(datetime_to_str(run.started_at))
        .bind(run.finished_at.map(datetime_to_str))
        .bind(run.duration_ms.map(|value| value as i64))
        .bind(run.attempt_count as i64)
        .bind(run.max_attempts as i64)
        .bind(run.retried)
        .bind(run.error_code.as_deref())
        .bind(run.error_message.as_deref())
        .bind(metadata_to_str(&run.metadata)?)
        .bind(datetime_to_str(run.started_at))
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("insert sqlite background job run", error))?;

        Ok(run)
    }

    async fn query_runs(
        &self,
        query: BackgroundJobRunQuery,
    ) -> MemcoreResult<BackgroundJobRunQueryResult> {
        let limit = validate_background_job_run_limit(query.limit)?;
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, kind, status, started_at, finished_at, duration_ms, attempt_count, max_attempts, retried, error_code, error_message, metadata FROM background_job_runs",
        );
        push_filters(&mut builder, &query);

        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "started_at", "id", cursor);
        }

        builder.push(" ORDER BY started_at DESC, id DESC LIMIT ");
        builder.push_bind(fetch_limit(limit) as i64);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("query sqlite background job runs", error))?;
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
        let cutoff = datetime_to_str(cutoff);
        if dry_run {
            let row = sqlx::query(
                "SELECT COUNT(*) AS count FROM background_job_runs WHERE started_at < ?",
            )
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| {
                storage_error("count sqlite background job runs for retention", error)
            })?;
            let count: i64 = row
                .try_get("count")
                .map_err(|error| storage_error("row count", error))?;
            return Ok(count as usize);
        }

        let result = sqlx::query("DELETE FROM background_job_runs WHERE started_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|error| {
                storage_error("delete sqlite background job runs for retention", error)
            })?;

        Ok(result.rows_affected() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use memcore_core::{BackgroundJobKind, BackgroundJobStatus};
    use serde_json::json;

    async fn store() -> SqliteBackgroundJobRunStore {
        SqliteBackgroundJobRunStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite background job run store")
    }

    fn run(
        kind: BackgroundJobKind,
        status: BackgroundJobStatus,
        started_at: DateTime<Utc>,
    ) -> StoredBackgroundJobRun {
        StoredBackgroundJobRun {
            id: Uuid::new_v4(),
            kind,
            status,
            started_at,
            finished_at: Some(started_at + Duration::seconds(1)),
            duration_ms: Some(1000),
            attempt_count: 1,
            max_attempts: 1,
            retried: false,
            error_code: Some("SAFE_ERROR".to_string()),
            error_message: Some("safe error only".to_string()),
            metadata: Some(json!({ "org_count": 1, "affected_count": 2 })),
        }
    }

    #[tokio::test]
    async fn insert_query_filters_order_cursor_delete_and_error_fields_work() {
        let store = store().await;
        let base = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        let older = store
            .insert_run(run(
                BackgroundJobKind::MemoryUsageSnapshot,
                BackgroundJobStatus::Succeeded,
                base - Duration::days(2),
            ))
            .await
            .expect("older insert");
        let middle = store
            .insert_run(run(
                BackgroundJobKind::ProviderUsageRetention,
                BackgroundJobStatus::Failed,
                base - Duration::days(1),
            ))
            .await
            .expect("middle insert");
        let latest = store
            .insert_run(run(
                BackgroundJobKind::MemoryUsageSnapshot,
                BackgroundJobStatus::Skipped,
                base,
            ))
            .await
            .expect("latest insert");

        let all = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: None,
                created_after: None,
                created_before: None,
                limit: 10,
                cursor: None,
            })
            .await
            .expect("query all");
        assert_eq!(
            all.runs.iter().map(|run| run.id).collect::<Vec<_>>(),
            vec![latest.id, middle.id, older.id]
        );
        assert_eq!(all.runs[0].error_code.as_deref(), Some("SAFE_ERROR"));
        assert!(!format!("{:?}", all.runs[0]).contains("Bearer"));

        let by_kind = store
            .query_runs(BackgroundJobRunQuery {
                kind: Some(BackgroundJobKind::MemoryUsageSnapshot),
                status: None,
                created_after: None,
                created_before: None,
                limit: 10,
                cursor: None,
            })
            .await
            .expect("kind");
        assert_eq!(by_kind.runs.len(), 2);

        let by_status = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: Some(BackgroundJobStatus::Failed),
                created_after: None,
                created_before: None,
                limit: 10,
                cursor: None,
            })
            .await
            .expect("status");
        assert_eq!(by_status.runs, vec![middle.clone()]);

        let range = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: None,
                created_after: Some(base - Duration::days(1)),
                created_before: Some(base + Duration::seconds(1)),
                limit: 10,
                cursor: None,
            })
            .await
            .expect("range");
        assert_eq!(range.runs.len(), 2);

        let first_page = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: None,
                created_after: None,
                created_before: None,
                limit: 1,
                cursor: None,
            })
            .await
            .expect("first page");
        assert_eq!(first_page.runs, vec![latest.clone()]);
        assert!(first_page.next_cursor.is_some());

        let cursor = memcore_core::parse_optional_cursor(first_page.next_cursor)
            .expect("cursor")
            .expect("cursor value");
        let second_page = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: None,
                created_after: None,
                created_before: None,
                limit: 1,
                cursor: Some(cursor),
            })
            .await
            .expect("second page");
        assert_eq!(second_page.runs, vec![middle]);

        let dry_run_count = store
            .delete_runs_older_than(base, true)
            .await
            .expect("dry run");
        assert_eq!(dry_run_count, 2);
        assert_eq!(
            store
                .query_runs(BackgroundJobRunQuery {
                    kind: None,
                    status: None,
                    created_after: None,
                    created_before: None,
                    limit: 10,
                    cursor: None,
                })
                .await
                .expect("after dry run")
                .runs
                .len(),
            3
        );

        let deleted = store
            .delete_runs_older_than(base, false)
            .await
            .expect("delete");
        assert_eq!(deleted, 2);
        assert_eq!(
            store
                .query_runs(BackgroundJobRunQuery {
                    kind: None,
                    status: None,
                    created_after: None,
                    created_before: None,
                    limit: 10,
                    cursor: None,
                })
                .await
                .expect("after delete")
                .runs,
            vec![latest]
        );
    }
}
