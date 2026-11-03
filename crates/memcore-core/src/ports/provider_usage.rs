use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::admin::ProviderUsageDailyBucket;
use crate::pagination::PageCursor;

pub const DEFAULT_PROVIDER_USAGE_LIMIT: usize = 50;
pub const MAX_PROVIDER_USAGE_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageCapability {
    Llm,
    Embedding,
    Summarization,
}

impl std::fmt::Display for ProviderUsageCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llm => write!(f, "llm"),
            Self::Embedding => write!(f, "embedding"),
            Self::Summarization => write!(f, "summarization"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCallStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageEventRecord {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: Option<String>,
    pub provider_name: String,
    pub model_name: Option<String>,
    pub capability: ProviderUsageCapability,
    pub operation_name: String,
    pub status: ProviderCallStatus,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub retry_count: u64,
    pub fallback_used: bool,
    pub circuit_blocked: bool,
    pub timed_out: bool,
    pub estimated_cost_usd: Option<f64>,
    pub metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageQuery {
    pub org_id: String,
    pub user_id: Option<String>,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub capability: Option<ProviderUsageCapability>,
    pub operation_name: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

impl ProviderUsageQuery {
    pub fn new(org_id: impl Into<String>, limit: usize) -> Self {
        Self {
            org_id: org_id.into(),
            user_id: None,
            provider_name: None,
            model_name: None,
            capability: None,
            operation_name: None,
            created_after: None,
            created_before: None,
            limit,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageDailyQuery {
    pub org_id: String,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub capability: Option<ProviderUsageCapability>,
    pub created_after: DateTime<Utc>,
    pub created_before: DateTime<Utc>,
}

impl ProviderUsageDailyQuery {
    pub fn new(
        org_id: impl Into<String>,
        created_after: DateTime<Utc>,
        created_before: DateTime<Utc>,
    ) -> Self {
        Self {
            org_id: org_id.into(),
            provider_name: None,
            model_name: None,
            capability: None,
            created_after,
            created_before,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderUsagePersistedSummary {
    pub total_requests: u64,
    pub total_successes: u64,
    pub total_errors: u64,
    pub total_retries: u64,
    pub total_fallbacks: u64,
    pub total_circuit_blocks: u64,
    pub total_timeouts: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub total_estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageQueryResult {
    pub events: Vec<ProviderUsageEventRecord>,
    pub next_cursor: Option<String>,
    pub summary: ProviderUsagePersistedSummary,
}

#[async_trait]
pub trait ProviderUsageStore: Send + Sync {
    async fn record_usage_event(&self, event: ProviderUsageEventRecord) -> MemcoreResult<()>;

    async fn query_usage(
        &self,
        query: ProviderUsageQuery,
    ) -> MemcoreResult<ProviderUsageQueryResult>;

    async fn query_usage_daily(
        &self,
        query: ProviderUsageDailyQuery,
    ) -> MemcoreResult<Vec<ProviderUsageDailyBucket>>;

    /// Count or delete persisted usage events with `created_at` strictly before `cutoff` for `org_id`.
    async fn delete_usage_events_older_than(
        &self,
        org_id: &str,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize>;
}

/// Tenant attribution for provider usage events (org required; user optional).
#[derive(Debug, Clone, Default)]
pub struct ProviderUsageAttribution {
    pub org_id: String,
    pub user_id: Option<String>,
}

/// Process-local slot set by the memory engine before provider calls.
#[derive(Debug, Default)]
pub struct ProviderUsageAttributionSlot {
    value: Mutex<Option<ProviderUsageAttribution>>,
}

impl ProviderUsageAttributionSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, org_id: String, user_id: Option<String>) {
        if let Ok(mut guard) = self.value.lock() {
            *guard = Some(ProviderUsageAttribution { org_id, user_id });
        }
    }

    pub fn snapshot(&self) -> Option<ProviderUsageAttribution> {
        self.value.lock().ok().and_then(|guard| guard.clone())
    }
}

pub fn validate_provider_usage_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Ok(DEFAULT_PROVIDER_USAGE_LIMIT);
    }

    if limit > MAX_PROVIDER_USAGE_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_PROVIDER_USAGE_LIMIT}"
        )));
    }

    Ok(limit)
}
