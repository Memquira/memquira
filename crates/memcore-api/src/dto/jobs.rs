use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use memcore_config::Settings;
use memcore_core::{
    BackgroundJobDefinition, BackgroundJobKind, BackgroundJobRun, BackgroundJobRunQuery,
    BackgroundJobRunQueryResult, BackgroundJobSnapshot, BackgroundJobStatus,
    DEFAULT_BACKGROUND_JOB_RUN_LIMIT, JobLockRecord, StoredBackgroundJobRun,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::dto::parse_optional_rfc3339_timestamp;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackgroundJobsResponse {
    pub status: &'static str,
    pub jobs: BackgroundJobSnapshotResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackgroundJobSnapshotResponse {
    pub jobs_enabled: bool,
    pub jobs: Vec<BackgroundJobDefinitionResponse>,
    pub recent_runs: Vec<BackgroundJobRunResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_persisted_runs: Option<Vec<BackgroundJobRunResponse>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackgroundJobDefinitionResponse {
    pub kind: String,
    pub enabled: bool,
    pub interval_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<BackgroundJobLockStatusResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackgroundJobLockStatusResponse {
    pub enabled: bool,
    pub owner_id: Option<String>,
    pub locked_until: Option<DateTime<Utc>>,
    pub is_locked: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackgroundJobRunResponse {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub attempt_count: usize,
    pub max_attempts: usize,
    pub retried: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub org_count: u64,
    pub affected_count: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunBackgroundJobResponse {
    pub status: &'static str,
    pub run: BackgroundJobRunResponse,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct QueryBackgroundJobRunsParams {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    #[serde(default = "default_background_job_run_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueryBackgroundJobRunsResponse {
    pub status: &'static str,
    pub runs: Vec<BackgroundJobRunResponse>,
    pub next_cursor: Option<String>,
}

fn default_background_job_run_limit() -> usize {
    DEFAULT_BACKGROUND_JOB_RUN_LIMIT
}

fn default_dry_run_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ApplyBackgroundJobRunRetentionRequest {
    #[serde(default = "default_dry_run_true")]
    pub dry_run: bool,
    #[serde(default)]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyBackgroundJobRunRetentionResponse {
    pub status: &'static str,
    pub summary: ApplyBackgroundJobRunRetentionSummaryResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyBackgroundJobRunRetentionSummaryResponse {
    pub dry_run: bool,
    pub matched_runs: usize,
    pub deleted_runs: usize,
    pub cutoff: DateTime<Utc>,
}

impl From<BackgroundJobDefinition> for BackgroundJobDefinitionResponse {
    fn from(definition: BackgroundJobDefinition) -> Self {
        Self {
            kind: definition.kind.as_str().to_string(),
            enabled: definition.enabled,
            interval_seconds: definition.interval.as_secs(),
            lock: None,
        }
    }
}

impl From<BackgroundJobRun> for BackgroundJobRunResponse {
    fn from(run: BackgroundJobRun) -> Self {
        Self {
            id: run.id,
            kind: run.kind.as_str().to_string(),
            status: run.status.as_str().to_string(),
            started_at: run.started_at,
            finished_at: run.finished_at,
            duration_ms: run.duration_ms,
            attempt_count: run.attempt_count,
            max_attempts: run.max_attempts,
            retried: run.retried,
            error_code: run.error_code,
            error_message: run
                .error_message
                .map(memcore_core::sanitize_background_job_error_message),
            org_count: run.org_count,
            affected_count: run.affected_count,
        }
    }
}

impl From<StoredBackgroundJobRun> for BackgroundJobRunResponse {
    fn from(run: StoredBackgroundJobRun) -> Self {
        BackgroundJobRunResponse::from(BackgroundJobRun::from(&run))
    }
}

impl From<BackgroundJobSnapshot> for BackgroundJobSnapshotResponse {
    fn from(snapshot: BackgroundJobSnapshot) -> Self {
        Self {
            jobs_enabled: snapshot.jobs_enabled,
            jobs: snapshot
                .jobs
                .into_iter()
                .map(BackgroundJobDefinitionResponse::from)
                .collect(),
            recent_runs: snapshot
                .recent_runs
                .into_iter()
                .map(BackgroundJobRunResponse::from)
                .collect(),
            latest_persisted_runs: None,
        }
    }
}

pub fn background_jobs_response(snapshot: BackgroundJobSnapshot) -> BackgroundJobsResponse {
    BackgroundJobsResponse {
        status: "success",
        jobs: BackgroundJobSnapshotResponse::from(snapshot),
    }
}

pub fn background_jobs_response_with_persisted_runs(
    snapshot: BackgroundJobSnapshot,
    latest_persisted_runs: Vec<StoredBackgroundJobRun>,
) -> BackgroundJobsResponse {
    let mut jobs = BackgroundJobSnapshotResponse::from(snapshot);
    jobs.latest_persisted_runs = Some(
        latest_persisted_runs
            .into_iter()
            .map(BackgroundJobRunResponse::from)
            .collect(),
    );
    BackgroundJobsResponse {
        status: "success",
        jobs,
    }
}

pub fn background_jobs_response_with_persisted_runs_and_locks(
    snapshot: BackgroundJobSnapshot,
    latest_persisted_runs: Option<Vec<StoredBackgroundJobRun>>,
    locks_enabled: bool,
    lock_statuses: Vec<(BackgroundJobKind, Option<JobLockRecord>)>,
) -> BackgroundJobsResponse {
    let mut jobs = BackgroundJobSnapshotResponse::from(snapshot);
    if let Some(runs) = latest_persisted_runs {
        jobs.latest_persisted_runs = Some(
            runs.into_iter()
                .map(BackgroundJobRunResponse::from)
                .collect(),
        );
    }

    if locks_enabled {
        for job in &mut jobs.jobs {
            let kind = parse_background_job_kind(&job.kind).ok();
            let lock = kind.and_then(|kind| {
                lock_statuses
                    .iter()
                    .find(|(lock_kind, _)| *lock_kind == kind)
                    .and_then(|(_, lock)| lock.clone())
            });
            job.lock = Some(BackgroundJobLockStatusResponse {
                enabled: true,
                owner_id: lock.as_ref().map(|lock| lock.owner_id.clone()),
                locked_until: lock.as_ref().map(|lock| lock.locked_until),
                is_locked: lock
                    .as_ref()
                    .is_some_and(|lock| lock.locked_until > Utc::now()),
            });
        }
    }

    BackgroundJobsResponse {
        status: "success",
        jobs,
    }
}

pub fn run_background_job_response(run: BackgroundJobRun) -> RunBackgroundJobResponse {
    RunBackgroundJobResponse {
        status: "success",
        run: BackgroundJobRunResponse::from(run),
    }
}

pub fn parse_background_job_kind(value: &str) -> MemcoreResult<BackgroundJobKind> {
    value.parse()
}

pub fn parse_background_job_status(value: &str) -> MemcoreResult<BackgroundJobStatus> {
    value.parse()
}

pub fn query_background_job_runs_input(
    params: QueryBackgroundJobRunsParams,
) -> MemcoreResult<BackgroundJobRunQuery> {
    let kind = params
        .kind
        .as_deref()
        .map(parse_background_job_kind)
        .transpose()?;
    let status = params
        .status
        .as_deref()
        .map(parse_background_job_status)
        .transpose()?;
    let created_after =
        parse_optional_rfc3339_timestamp(params.created_after.as_ref(), "created_after")?;
    let created_before =
        parse_optional_rfc3339_timestamp(params.created_before.as_ref(), "created_before")?;
    if let (Some(after), Some(before)) = (created_after, created_before)
        && after >= before
    {
        return Err(memcore_common::MemcoreError::ValidationError(
            "created_after must be earlier than created_before".to_string(),
        ));
    }

    Ok(BackgroundJobRunQuery {
        kind,
        status,
        created_after,
        created_before,
        limit: params.limit,
        cursor: memcore_core::parse_optional_cursor(params.cursor)?,
    })
}

pub fn query_background_job_runs_response(
    result: BackgroundJobRunQueryResult,
) -> QueryBackgroundJobRunsResponse {
    QueryBackgroundJobRunsResponse {
        status: "success",
        runs: result
            .runs
            .into_iter()
            .map(BackgroundJobRunResponse::from)
            .collect(),
        next_cursor: result.next_cursor,
    }
}

pub fn resolve_background_job_history_retention_days(
    override_days: Option<u32>,
    default_days: u32,
) -> u32 {
    override_days.unwrap_or(default_days)
}

impl ApplyBackgroundJobRunRetentionRequest {
    pub fn retention_days(&self, settings: &Settings) -> u32 {
        resolve_background_job_history_retention_days(
            self.retention_days,
            settings.background_job_history_retention_days,
        )
    }
}

pub fn background_job_run_retention_response(
    request: ApplyBackgroundJobRunRetentionRequest,
    cutoff: DateTime<Utc>,
    matched_runs: usize,
) -> ApplyBackgroundJobRunRetentionResponse {
    ApplyBackgroundJobRunRetentionResponse {
        status: "success",
        summary: ApplyBackgroundJobRunRetentionSummaryResponse {
            dry_run: request.dry_run,
            matched_runs,
            deleted_runs: if request.dry_run { 0 } else { matched_runs },
            cutoff,
        },
    }
}
