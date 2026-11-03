use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::pagination::{PageCursor, build_page, is_after_cursor_in_desc_order};
use memcore_core::{
    BackgroundJobRunQuery, BackgroundJobRunQueryResult, BackgroundJobRunStore,
    StoredBackgroundJobRun, validate_background_job_run_limit,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn matches_query(run: &StoredBackgroundJobRun, query: &BackgroundJobRunQuery) -> bool {
    if let Some(kind) = query.kind
        && run.kind != kind
    {
        return false;
    }
    if let Some(status) = query.status
        && run.status != status
    {
        return false;
    }
    if let Some(created_after) = query.created_after
        && run.started_at < created_after
    {
        return false;
    }
    if let Some(created_before) = query.created_before
        && run.started_at >= created_before
    {
        return false;
    }
    true
}

#[derive(Debug, Default)]
pub struct MockBackgroundJobRunStore {
    runs: RwLock<Vec<StoredBackgroundJobRun>>,
}

impl MockBackgroundJobRunStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BackgroundJobRunStore for MockBackgroundJobRunStore {
    async fn insert_run(
        &self,
        run: StoredBackgroundJobRun,
    ) -> MemcoreResult<StoredBackgroundJobRun> {
        self.runs
            .write()
            .map_err(|_| storage_error("mock background job run lock poisoned", "lock"))?
            .push(run.clone());
        Ok(run)
    }

    async fn query_runs(
        &self,
        query: BackgroundJobRunQuery,
    ) -> MemcoreResult<BackgroundJobRunQueryResult> {
        let limit = validate_background_job_run_limit(query.limit)?;
        let mut runs = self
            .runs
            .read()
            .map_err(|_| storage_error("mock background job run lock poisoned", "lock"))?
            .iter()
            .filter(|run| matches_query(run, &query))
            .cloned()
            .collect::<Vec<_>>();

        runs.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.id.cmp(&left.id))
        });

        if let Some(cursor) = &query.cursor {
            runs.retain(|run| {
                is_after_cursor_in_desc_order(run.started_at, &run.id.to_string(), cursor)
            });
        }

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
        let mut runs = self
            .runs
            .write()
            .map_err(|_| storage_error("mock background job run lock poisoned", "lock"))?;
        let matched = runs.iter().filter(|run| run.started_at < cutoff).count();
        if dry_run {
            return Ok(matched);
        }
        runs.retain(|run| run.started_at >= cutoff);
        Ok(matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use memcore_core::{BackgroundJobKind, BackgroundJobStatus};
    use serde_json::json;
    use uuid::Uuid;

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
            error_code: None,
            error_message: None,
            metadata: Some(json!({ "org_count": 1, "affected_count": 2 })),
        }
    }

    #[tokio::test]
    async fn insert_query_filters_limit_cursor_and_delete_work() {
        let store = MockBackgroundJobRunStore::new();
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
            .expect("query kind");
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
            .expect("query status");
        assert_eq!(by_status.runs, vec![middle.clone()]);

        let date_range = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: None,
                created_after: Some(base - Duration::days(1)),
                created_before: Some(base + Duration::seconds(1)),
                limit: 10,
                cursor: None,
            })
            .await
            .expect("query date range");
        assert_eq!(date_range.runs.len(), 2);

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
        assert_eq!(first_page.runs, vec![latest]);
        assert!(first_page.next_cursor.is_some());

        let second_cursor = memcore_core::parse_optional_cursor(first_page.next_cursor)
            .expect("cursor")
            .expect("cursor value");
        let second_page = store
            .query_runs(BackgroundJobRunQuery {
                kind: None,
                status: None,
                created_after: None,
                created_before: None,
                limit: 1,
                cursor: Some(second_cursor),
            })
            .await
            .expect("second page");
        assert_eq!(second_page.runs, vec![middle]);

        let dry_run_count = store
            .delete_runs_older_than(base, true)
            .await
            .expect("dry run delete");
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
                .runs
                .len(),
            1
        );
    }
}
