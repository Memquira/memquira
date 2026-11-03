use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::{ApplyProviderUsageRetentionInput, CreateMemoryUsageSnapshotInput, MemoryEngine};
use memcore_common::MemcoreResult;

use super::{
    BackgroundJobDefinition, BackgroundJobKind, BackgroundJobRun, BackgroundJobStatus,
    is_retryable_job_error,
};

#[async_trait]
pub trait BackgroundJob: Send + Sync {
    fn kind(&self) -> BackgroundJobKind;
    fn interval(&self) -> Duration;
    fn enabled(&self) -> bool;

    async fn run_once(&self) -> MemcoreResult<BackgroundJobRun>;

    fn definition(&self) -> BackgroundJobDefinition {
        BackgroundJobDefinition {
            kind: self.kind(),
            enabled: self.enabled(),
            interval: self.interval(),
        }
    }
}

#[derive(Clone)]
pub struct MemoryUsageSnapshotJob {
    engine: Arc<MemoryEngine>,
    enabled: bool,
    interval: Duration,
    org_ids: Vec<String>,
}

impl MemoryUsageSnapshotJob {
    pub fn new(
        engine: Arc<MemoryEngine>,
        enabled: bool,
        interval: Duration,
        org_ids: Vec<String>,
    ) -> Self {
        Self {
            engine,
            enabled,
            interval,
            org_ids,
        }
    }
}

#[async_trait]
impl BackgroundJob for MemoryUsageSnapshotJob {
    fn kind(&self) -> BackgroundJobKind {
        BackgroundJobKind::MemoryUsageSnapshot
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    async fn run_once(&self) -> MemcoreResult<BackgroundJobRun> {
        let mut run = BackgroundJobRun::running(self.kind());
        run.org_count = self.org_ids.len() as u64;

        if self.org_ids.is_empty() {
            run.error_code = Some("NO_CONFIGURED_ORGS".to_string());
            run.error_message =
                Some("no organization ids configured for background jobs".to_string());
            return Ok(run.finish(BackgroundJobStatus::Skipped));
        }

        let mut failures = 0u64;
        let mut successes = 0u64;
        let mut first_retryable_error = None;
        for org_id in &self.org_ids {
            match self
                .engine
                .create_memory_usage_snapshot(CreateMemoryUsageSnapshotInput {
                    org_id: org_id.clone(),
                    captured_at: None,
                })
                .await
            {
                Ok(_) => {
                    successes += 1;
                    run.affected_count += 1;
                }
                Err(error) => {
                    failures += 1;
                    if first_retryable_error.is_none() && is_retryable_job_error(&error) {
                        first_retryable_error = Some(error.clone());
                    }
                    tracing::warn!(
                        job_kind = %self.kind(),
                        org_id = %org_id,
                        error_code = error.code(),
                        "background memory usage snapshot failed for org"
                    );
                }
            }
        }

        if failures > 0 {
            if successes == 0
                && let Some(error) = first_retryable_error
            {
                return Err(error);
            }
            run.error_code = Some("PARTIAL_FAILURE".to_string());
            run.error_message = Some(format!("{failures} organization(s) failed"));
            return Ok(run.finish(BackgroundJobStatus::Failed));
        }

        Ok(run.finish(BackgroundJobStatus::Succeeded))
    }
}

#[derive(Clone)]
pub struct ProviderUsageRetentionJob {
    engine: Arc<MemoryEngine>,
    enabled: bool,
    interval: Duration,
    org_ids: Vec<String>,
    retention_days: u32,
}

impl ProviderUsageRetentionJob {
    pub fn new(
        engine: Arc<MemoryEngine>,
        enabled: bool,
        interval: Duration,
        org_ids: Vec<String>,
        retention_days: u32,
    ) -> Self {
        Self {
            engine,
            enabled,
            interval,
            org_ids,
            retention_days,
        }
    }
}

#[async_trait]
impl BackgroundJob for ProviderUsageRetentionJob {
    fn kind(&self) -> BackgroundJobKind {
        BackgroundJobKind::ProviderUsageRetention
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    async fn run_once(&self) -> MemcoreResult<BackgroundJobRun> {
        let mut run = BackgroundJobRun::running(self.kind());
        run.org_count = self.org_ids.len() as u64;

        if self.org_ids.is_empty() {
            run.error_code = Some("NO_CONFIGURED_ORGS".to_string());
            run.error_message =
                Some("no organization ids configured for background jobs".to_string());
            return Ok(run.finish(BackgroundJobStatus::Skipped));
        }

        if self.retention_days == 0 {
            run.error_code = Some("RETENTION_DISABLED".to_string());
            run.error_message = Some("provider usage retention days is 0".to_string());
            return Ok(run.finish(BackgroundJobStatus::Skipped));
        }

        let mut failures = 0u64;
        let mut successes = 0u64;
        let mut first_retryable_error = None;
        for org_id in &self.org_ids {
            match self
                .engine
                .apply_provider_usage_retention(ApplyProviderUsageRetentionInput {
                    org_id: org_id.clone(),
                    retention_days: self.retention_days,
                    dry_run: false,
                })
                .await
            {
                Ok(output) => {
                    successes += 1;
                    run.affected_count += output.deleted_events as u64;
                }
                Err(error) => {
                    failures += 1;
                    if first_retryable_error.is_none() && is_retryable_job_error(&error) {
                        first_retryable_error = Some(error.clone());
                    }
                    tracing::warn!(
                        job_kind = %self.kind(),
                        org_id = %org_id,
                        error_code = error.code(),
                        "background provider usage retention failed for org"
                    );
                }
            }
        }

        if failures > 0 {
            if successes == 0
                && let Some(error) = first_retryable_error
            {
                return Err(error);
            }
            run.error_code = Some("PARTIAL_FAILURE".to_string());
            run.error_message = Some(format!("{failures} organization(s) failed"));
            return Ok(run.finish(BackgroundJobStatus::Failed));
        }

        Ok(run.finish(BackgroundJobStatus::Succeeded))
    }
}

#[derive(Clone)]
pub struct MemoryRetentionJob {
    enabled: bool,
    interval: Duration,
}

impl MemoryRetentionJob {
    pub fn new(enabled: bool, interval: Duration) -> Self {
        Self { enabled, interval }
    }
}

#[async_trait]
impl BackgroundJob for MemoryRetentionJob {
    fn kind(&self) -> BackgroundJobKind {
        BackgroundJobKind::MemoryRetention
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    async fn run_once(&self) -> MemcoreResult<BackgroundJobRun> {
        let mut run = BackgroundJobRun::running(self.kind());
        run.error_code = Some("USER_DISCOVERY_NOT_IMPLEMENTED".to_string());
        run.error_message =
            Some("user discovery for org-wide memory retention is not implemented yet".to_string());
        Ok(run.finish(BackgroundJobStatus::Skipped))
    }
}
