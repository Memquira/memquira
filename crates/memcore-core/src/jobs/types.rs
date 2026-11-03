use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackgroundJobKind {
    MemoryUsageSnapshot,
    ProviderUsageRetention,
    MemoryRetention,
}

impl BackgroundJobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemoryUsageSnapshot => "MemoryUsageSnapshot",
            Self::ProviderUsageRetention => "ProviderUsageRetention",
            Self::MemoryRetention => "MemoryRetention",
        }
    }

    pub fn as_path(self) -> &'static str {
        match self {
            Self::MemoryUsageSnapshot => "memory-usage-snapshot",
            Self::ProviderUsageRetention => "provider-usage-retention",
            Self::MemoryRetention => "memory-retention",
        }
    }
}

impl fmt::Display for BackgroundJobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BackgroundJobKind {
    type Err = MemcoreError;

    fn from_str(value: &str) -> MemcoreResult<Self> {
        match value.trim() {
            "MemoryUsageSnapshot" | "memory-usage-snapshot" | "memory_usage_snapshot" => {
                Ok(Self::MemoryUsageSnapshot)
            }
            "ProviderUsageRetention" | "provider-usage-retention" | "provider_usage_retention" => {
                Ok(Self::ProviderUsageRetention)
            }
            "MemoryRetention" | "memory-retention" | "memory_retention" => {
                Ok(Self::MemoryRetention)
            }
            other => Err(MemcoreError::ValidationError(format!(
                "invalid job_kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackgroundJobStatus {
    Idle,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

impl BackgroundJobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
        }
    }
}

impl fmt::Display for BackgroundJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BackgroundJobStatus {
    type Err = MemcoreError;

    fn from_str(value: &str) -> MemcoreResult<Self> {
        match value.trim() {
            "Idle" | "idle" => Ok(Self::Idle),
            "Running" | "running" => Ok(Self::Running),
            "Succeeded" | "succeeded" | "success" => Ok(Self::Succeeded),
            "Failed" | "failed" | "error" => Ok(Self::Failed),
            "Skipped" | "skipped" => Ok(Self::Skipped),
            "Cancelled" | "cancelled" | "canceled" => Ok(Self::Cancelled),
            other => Err(MemcoreError::ValidationError(format!(
                "invalid job status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundJobDefinition {
    pub kind: BackgroundJobKind,
    pub enabled: bool,
    pub interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundJobRun {
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
    pub org_count: u64,
    pub affected_count: u64,
}

impl BackgroundJobRun {
    pub fn running(kind: BackgroundJobKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            status: BackgroundJobStatus::Running,
            started_at: Utc::now(),
            finished_at: None,
            duration_ms: None,
            attempt_count: 1,
            max_attempts: 1,
            retried: false,
            error_code: None,
            error_message: None,
            org_count: 0,
            affected_count: 0,
        }
    }

    pub fn finish(mut self, status: BackgroundJobStatus) -> Self {
        let finished_at = Utc::now();
        self.duration_ms = Some((finished_at - self.started_at).num_milliseconds().max(0) as u64);
        self.finished_at = Some(finished_at);
        self.status = status;
        self
    }

    pub fn skipped(kind: BackgroundJobKind, message: impl Into<String>) -> Self {
        let mut run = Self::running(kind).finish(BackgroundJobStatus::Skipped);
        run.error_code = Some("SKIPPED".to_string());
        run.error_message = Some(message.into());
        run
    }

    pub fn failed(
        kind: BackgroundJobKind,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut run = Self::running(kind).finish(BackgroundJobStatus::Failed);
        run.error_code = Some(code.into());
        run.error_message = Some(message.into());
        run
    }

    pub fn cancelled(kind: BackgroundJobKind, message: impl Into<String>) -> Self {
        let mut run = Self::running(kind).finish(BackgroundJobStatus::Cancelled);
        run.error_code = Some("SHUTDOWN_REQUESTED".to_string());
        run.error_message = Some(message.into());
        run
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundJobSnapshot {
    pub jobs_enabled: bool,
    pub jobs: Vec<BackgroundJobDefinition>,
    pub recent_runs: Vec<BackgroundJobRun>,
}
