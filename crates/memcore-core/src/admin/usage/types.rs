use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{OrgPlanConfig, PageCursor, ProviderUsageCapability, QuotaCheckResult};

pub const DEFAULT_ORG_USAGE_DASHBOARD_DAYS: u32 = 30;
pub const MAX_ORG_USAGE_DASHBOARD_DAYS: u32 = 90;
pub const DEFAULT_MEMORY_USAGE_SNAPSHOT_LIMIT: usize = 50;
pub const MAX_MEMORY_USAGE_SNAPSHOT_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgUsageDashboardInput {
    pub org_id: String,
    pub created_after: DateTime<Utc>,
    pub created_before: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgUsageDashboardOutput {
    pub org_id: String,
    pub generated_at: DateTime<Utc>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub plan: Option<OrgPlanConfig>,
    pub quota: QuotaCheckResult,
    pub memory: OrgMemoryUsageSummary,
    pub provider: ProviderUsageDashboardSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMemoryUsageSummary {
    pub total_users: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub deleted_memories: Option<u64>,
    pub latest_snapshot: Option<MemoryUsageLatestSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryUsageLatestSnapshot {
    pub captured_at: DateTime<Utc>,
    pub total_users: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub deleted_memories: Option<u64>,
}

impl From<&MemoryUsageSnapshot> for MemoryUsageLatestSnapshot {
    fn from(snapshot: &MemoryUsageSnapshot) -> Self {
        Self {
            captured_at: snapshot.captured_at,
            total_users: snapshot.total_users,
            total_memories: snapshot.total_memories,
            active_memories: snapshot.active_memories,
            deleted_memories: snapshot.deleted_memories,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageDashboardSummary {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageDailyBucket {
    pub date: NaiveDate,
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

impl ProviderUsageDailyBucket {
    pub fn empty(date: NaiveDate) -> Self {
        Self {
            date,
            total_requests: 0,
            total_successes: 0,
            total_errors: 0,
            total_retries: 0,
            total_fallbacks: 0,
            total_circuit_blocks: 0,
            total_timeouts: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_tokens: 0,
            total_estimated_cost_usd: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsageDailyInput {
    pub org_id: String,
    pub created_after: DateTime<Utc>,
    pub created_before: DateTime<Utc>,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub capability: Option<ProviderUsageCapability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageDailyOutput {
    pub org_id: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub buckets: Vec<ProviderUsageDailyBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryUsageSnapshot {
    pub id: Uuid,
    pub org_id: String,
    pub total_users: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub deleted_memories: Option<u64>,
    pub captured_at: DateTime<Utc>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateMemoryUsageSnapshotInput {
    pub org_id: String,
    pub captured_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateMemoryUsageSnapshotOutput {
    pub snapshot: MemoryUsageSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryMemoryUsageSnapshotsInput {
    pub org_id: String,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub limit: usize,
    pub cursor: Option<PageCursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMemoryUsageSnapshotsOutput {
    pub snapshots: Vec<MemoryUsageSnapshot>,
    pub next_cursor: Option<String>,
}
