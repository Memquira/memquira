use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::jobs::{BackgroundJobKind, BackgroundJobRun, BackgroundJobStatus};
use crate::pagination::PageCursor;

pub const DEFAULT_BACKGROUND_JOB_RUN_LIMIT: usize = 50;
pub const MAX_BACKGROUND_JOB_RUN_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredBackgroundJobRun {
    pub id: Uuid,
    pub kind: BackgroundJobKind,
    pub status: BackgroundJobStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub attempt_count: usize,
    pub max_attempts: usize,
    pub retried: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub metadata: Option<Value>,
}

impl From<BackgroundJobRun> for StoredBackgroundJobRun {
    fn from(run: BackgroundJobRun) -> Self {
        let metadata = serde_json::json!({
            "org_count": run.org_count,
            "affected_count": run.affected_count,
            "retry": {
                "attempt_count": run.attempt_count,
                "max_attempts": run.max_attempts,
                "retried": run.retried
            }
        });
        Self {
            id: run.id,
            kind: run.kind,
            status: run.status,
            started_at: run.started_at,
            finished_at: run.finished_at,
            duration_ms: run.duration_ms,
            attempt_count: run.attempt_count,
            max_attempts: run.max_attempts,
            retried: run.retried,
            error_code: run.error_code,
            error_message: run.error_message.map(sanitize_background_job_error_message),
            metadata: Some(metadata),
        }
    }
}

impl From<&StoredBackgroundJobRun> for BackgroundJobRun {
    fn from(run: &StoredBackgroundJobRun) -> Self {
        let org_count = run
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("org_count"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let affected_count = run
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("affected_count"))
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let retry = run
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("retry"));
        let attempt_count = if run.attempt_count == 0 {
            retry
                .and_then(|metadata| metadata.get("attempt_count"))
                .and_then(|value| value.as_u64())
                .map(|value| value as usize)
                .unwrap_or(1)
        } else {
            run.attempt_count
        };
        let max_attempts = if run.max_attempts == 0 {
            retry
                .and_then(|metadata| metadata.get("max_attempts"))
                .and_then(|value| value.as_u64())
                .map(|value| value as usize)
                .unwrap_or(1)
        } else {
            run.max_attempts
        };
        let retried = run.retried
            || retry
                .and_then(|metadata| metadata.get("retried"))
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            || attempt_count > 1;

        Self {
            id: run.id,
            kind: run.kind,
            status: run.status,
            started_at: run.started_at,
            finished_at: run.finished_at,
            duration_ms: run.duration_ms,
            attempt_count,
            max_attempts,
            retried,
            error_code: run.error_code.clone(),
            error_message: run.error_message.clone(),
            org_count,
            affected_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundJobRunQuery {
    pub kind: Option<BackgroundJobKind>,
    pub status: Option<BackgroundJobStatus>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundJobRunQueryResult {
    pub runs: Vec<StoredBackgroundJobRun>,
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait BackgroundJobRunStore: Send + Sync {
    async fn insert_run(
        &self,
        run: StoredBackgroundJobRun,
    ) -> MemcoreResult<StoredBackgroundJobRun>;

    async fn query_runs(
        &self,
        query: BackgroundJobRunQuery,
    ) -> MemcoreResult<BackgroundJobRunQueryResult>;

    async fn delete_runs_older_than(
        &self,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize>;
}

pub fn validate_background_job_run_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Ok(DEFAULT_BACKGROUND_JOB_RUN_LIMIT);
    }

    if limit > MAX_BACKGROUND_JOB_RUN_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit must be <= {MAX_BACKGROUND_JOB_RUN_LIMIT}"
        )));
    }

    Ok(limit)
}

pub fn sanitize_background_job_error_message(message: impl Into<String>) -> String {
    let mut sanitized = memcore_common::Redactor::redact_str(&message.into());

    if sanitized.len() > 512 {
        sanitized.truncate(512);
        sanitized.push_str("...");
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_defaults_and_caps() {
        assert_eq!(
            validate_background_job_run_limit(0).expect("default limit"),
            DEFAULT_BACKGROUND_JOB_RUN_LIMIT
        );
        assert!(validate_background_job_run_limit(MAX_BACKGROUND_JOB_RUN_LIMIT).is_ok());
        assert!(validate_background_job_run_limit(MAX_BACKGROUND_JOB_RUN_LIMIT + 1).is_err());
    }

    #[test]
    fn sanitizes_secret_like_error_messages() {
        let message = sanitize_background_job_error_message(
            "failed to connect postgres://user:pass@localhost/db",
        );
        assert!(!message.contains("pass"));
        assert!(message.contains("[REDACTED]"));
    }
}
