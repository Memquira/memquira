use chrono::{DateTime, Utc};
use memcore_common::MemcoreError;
use memcore_config::Settings;
use memcore_core::{
    ContextCacheMetricsSnapshot, CreateMemoryUsageSnapshotInput, CreateMemoryUsageSnapshotOutput,
    DEFAULT_LIST_ORG_USERS_LIMIT, DEFAULT_MEMORY_USAGE_SNAPSHOT_LIMIT,
    DEFAULT_SEARCH_ORG_MEMORY_EVENTS_LIMIT, ListOrgUsersInput, ListOrgUsersOutput,
    MAX_LIST_ORG_USERS_LIMIT, MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT, MemoryEvent,
    MemoryUsageLatestSnapshot, MemoryUsageSnapshot, OrgMemoryUsageSummary, OrgPlanConfig,
    OrgPlanLimits, OrgPlanTier, OrgQuotaLimits, OrgQuotaUsage, OrgSummaryInput, OrgSummaryOutput,
    OrgUsageDashboardInput, OrgUsageDashboardOutput, OrgUserSummary, ProviderUsageDailyBucket,
    ProviderUsageDailyOutput, ProviderUsageDashboardSummary, QueryEmbeddingCacheStats,
    QueryMemoryUsageSnapshotsInput, QueryMemoryUsageSnapshotsOutput, QuotaCheckResult,
    QuotaLimitKind, QuotaViolation, SearchOrgMemoryEventsOutput, resolve_org_usage_window,
    validate_org_plan_metadata,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::dto::memory_events::MemoryEventOperationResponse;

pub fn default_list_org_users_limit() -> usize {
    DEFAULT_LIST_ORG_USERS_LIMIT
}

pub fn default_search_org_memory_events_limit() -> usize {
    DEFAULT_SEARCH_ORG_MEMORY_EVENTS_LIMIT
}

pub fn default_memory_usage_snapshot_limit() -> usize {
    DEFAULT_MEMORY_USAGE_SNAPSHOT_LIMIT
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SearchOrgMemoryEventsQuery {
    pub user_id: Option<String>,
    pub fact_id: Option<String>,
    pub operation: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub q: Option<String>,
    #[serde(default = "default_search_org_memory_events_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ListOrgUsersQuery {
    #[serde(default = "default_list_org_users_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OrgQuotasQuery {
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OrgUsageDateRangeQuery {
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ProviderUsageDailyQueryParams {
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub capability: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateMemoryUsageSnapshotRequest {
    pub captured_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct QueryMemoryUsageSnapshotsParams {
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    #[serde(default = "default_memory_usage_snapshot_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchOrgMemoryEventsResponse {
    pub status: &'static str,
    pub events: Vec<AdminOrgMemoryEventItemResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminOrgMemoryEventItemResponse {
    pub id: Uuid,
    pub user_id: String,
    pub fact_id: Option<Uuid>,
    pub operation: MemoryEventOperationResponse,
    pub previous_content: Option<String>,
    pub new_content: Option<String>,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextCacheMetricsResponse {
    pub status: &'static str,
    /// Metrics are aggregate counters for this API process only.
    pub scope: &'static str,
    pub metrics: ContextCacheMetricsBodyResponse,
}

/// The embedding provider's process-local diagnostics (a local model's queue depth,
/// queue wait and inference time); `diagnostics` is null for providers without any.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EmbeddingDiagnosticsResponse {
    pub status: &'static str,
    pub scope: &'static str,
    #[schema(value_type = Option<Object>)]
    pub diagnostics: Option<Value>,
}

/// The query-embedding cache's process-local counters.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueryEmbeddingCacheMetricsResponse {
    pub status: &'static str,
    /// Metrics are aggregate counters for this API process only.
    pub scope: &'static str,
    /// `false` when `MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY` is 0 (the default); counters are then 0.
    pub enabled: bool,
    pub metrics: QueryEmbeddingCacheMetricsBodyResponse,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct QueryEmbeddingCacheMetricsBodyResponse {
    /// Query-embedding requests that reached the cache.
    pub requests: u64,
    /// Served from a stored entry (no provider operation).
    pub hits: u64,
    /// Started a single flight (one provider operation each).
    pub leaders: u64,
    /// Waited for a concurrent flight (no provider operation).
    pub followers: u64,
    /// Provider without a known embedding identity (one uncached provider operation each).
    pub bypassed: u64,
    /// Actual provider embedding operations started through the cache: leaders + bypassed.
    pub provider_operations: u64,
    pub failed_flights: u64,
    pub abandoned_flights: u64,
    pub follower_failures: u64,
    pub invalid_not_cached: u64,
    pub entries: usize,
}

pub fn query_embedding_cache_metrics_response(
    stats: Option<QueryEmbeddingCacheStats>,
) -> QueryEmbeddingCacheMetricsResponse {
    let metrics = stats.map_or_else(QueryEmbeddingCacheMetricsBodyResponse::default, |stats| {
        QueryEmbeddingCacheMetricsBodyResponse {
            requests: stats.requests,
            hits: stats.hits,
            leaders: stats.leaders,
            followers: stats.followers,
            bypassed: stats.bypassed,
            provider_operations: stats.provider_operations(),
            failed_flights: stats.failed_flights,
            abandoned_flights: stats.abandoned_flights,
            follower_failures: stats.follower_failures,
            invalid_not_cached: stats.invalid_not_cached,
            entries: stats.entries,
        }
    });
    QueryEmbeddingCacheMetricsResponse {
        status: "success",
        scope: "process_local",
        enabled: stats.is_some(),
        metrics,
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextCacheMetricsBodyResponse {
    pub hits: u64,
    pub misses: u64,
    pub sets: u64,
    pub invalidations: u64,
    pub invalidated_entries: u64,
    pub stale_served: u64,
    pub refresh_started: u64,
    pub refresh_succeeded: u64,
    pub refresh_failed: u64,
    /// Background refreshes skipped at the per-process refresh budget.
    pub refresh_skipped: u64,
    pub stampede_waits: u64,
    pub stampede_timeouts: u64,
    pub compute_errors: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgQuotaStatusResponse {
    pub status: &'static str,
    pub quotas: OrgQuotaStatusBodyResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgQuotaStatusBodyResponse {
    pub source: String,
    pub allowed: bool,
    pub limits: OrgQuotaLimitsResponse,
    pub usage: OrgQuotaUsageResponse,
    pub violations: Vec<QuotaViolationResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgUsageDashboardResponse {
    pub status: &'static str,
    pub dashboard: OrgUsageDashboardBodyResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgUsageDashboardBodyResponse {
    pub org_id: String,
    pub generated_at: DateTime<Utc>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub plan: Option<OrgPlanResponse>,
    pub quota: OrgQuotaStatusBodyResponse,
    pub memory: OrgMemoryUsageSummaryResponse,
    pub provider: ProviderUsageSummaryResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgMemoryUsageSummaryResponse {
    pub total_users: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub deleted_memories: Option<u64>,
    pub latest_snapshot: Option<MemoryUsageLatestSnapshotResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryUsageLatestSnapshotResponse {
    pub captured_at: DateTime<Utc>,
    pub total_users: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub deleted_memories: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreateMemoryUsageSnapshotResponse {
    pub status: &'static str,
    pub snapshot: MemoryUsageSnapshotResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueryMemoryUsageSnapshotsResponse {
    pub status: &'static str,
    pub snapshots: Vec<MemoryUsageSnapshotResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryUsageSnapshotResponse {
    pub id: Uuid,
    pub org_id: String,
    pub total_users: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub deleted_memories: Option<u64>,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgQuotaLimitsResponse {
    pub enabled: bool,
    pub max_users_per_org: Option<u64>,
    pub max_memories_per_user: Option<u64>,
    pub max_memories_per_org: Option<u64>,
    pub daily_provider_request_limit: Option<u64>,
    pub daily_provider_token_limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgQuotaUsageResponse {
    pub org_id: String,
    pub total_users: u64,
    pub total_memories: u64,
    pub user_memory_count: Option<u64>,
    pub daily_provider_requests: u64,
    pub daily_provider_tokens: u64,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuotaViolationResponse {
    pub kind: String,
    pub limit: u64,
    pub current: u64,
    pub requested: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgPlanResponse {
    pub org_id: String,
    pub tier: String,
    pub limits: OrgPlanLimitsResponse,
    pub is_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgPlanLimitsResponse {
    pub max_users_per_org: Option<u64>,
    pub max_memories_per_user: Option<u64>,
    pub max_memories_per_org: Option<u64>,
    pub daily_provider_request_limit: Option<u64>,
    pub daily_provider_token_limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GetOrgPlanResponse {
    pub status: &'static str,
    pub plan: Option<OrgPlanResponse>,
    pub resolved_source: String,
    pub resolved_limits: OrgQuotaLimitsResponse,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpsertOrgPlanRequest {
    pub tier: String,
    pub limits: UpsertOrgPlanLimitsRequest,
    #[serde(default = "default_true")]
    pub is_active: bool,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpsertOrgPlanLimitsRequest {
    pub max_users_per_org: Option<i64>,
    pub max_memories_per_user: Option<i64>,
    pub max_memories_per_org: Option<i64>,
    pub daily_provider_request_limit: Option<i64>,
    pub daily_provider_token_limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpsertOrgPlanResponse {
    pub status: &'static str,
    pub plan: OrgPlanResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeleteOrgPlanResponse {
    pub status: &'static str,
    pub deleted: bool,
}

impl From<ContextCacheMetricsSnapshot> for ContextCacheMetricsBodyResponse {
    fn from(snapshot: ContextCacheMetricsSnapshot) -> Self {
        Self {
            hits: snapshot.hits,
            misses: snapshot.misses,
            sets: snapshot.sets,
            invalidations: snapshot.invalidations,
            invalidated_entries: snapshot.invalidated_entries,
            stale_served: snapshot.stale_served,
            refresh_started: snapshot.refresh_started,
            refresh_succeeded: snapshot.refresh_succeeded,
            refresh_failed: snapshot.refresh_failed,
            refresh_skipped: snapshot.refresh_skipped,
            stampede_waits: snapshot.stampede_waits,
            stampede_timeouts: snapshot.stampede_timeouts,
            compute_errors: snapshot.compute_errors,
        }
    }
}

pub fn context_cache_metrics_response(
    snapshot: ContextCacheMetricsSnapshot,
) -> ContextCacheMetricsResponse {
    ContextCacheMetricsResponse {
        status: "success",
        scope: "process_local",
        metrics: snapshot.into(),
    }
}

pub fn org_quota_limits_from_settings(settings: &Settings) -> OrgQuotaLimits {
    OrgQuotaLimits::from_raw(
        settings.quotas_enabled,
        settings.max_users_per_org,
        settings.max_memories_per_user,
        settings.max_memories_per_org,
        settings.daily_provider_request_limit,
        settings.daily_provider_token_limit,
    )
}

pub fn org_quota_status_response(result: QuotaCheckResult) -> OrgQuotaStatusResponse {
    OrgQuotaStatusResponse {
        status: "success",
        quotas: OrgQuotaStatusBodyResponse {
            source: result.source.as_str().to_string(),
            allowed: result.allowed,
            limits: result.limits.into(),
            usage: result.usage.into(),
            violations: result
                .violations
                .into_iter()
                .map(QuotaViolationResponse::from)
                .collect(),
        },
    }
}

pub fn get_org_plan_response(
    plan: Option<OrgPlanConfig>,
    resolved: memcore_core::ResolvedOrgQuotaLimits,
) -> GetOrgPlanResponse {
    GetOrgPlanResponse {
        status: "success",
        plan: plan.map(OrgPlanResponse::from),
        resolved_source: resolved.source.as_str().to_string(),
        resolved_limits: resolved.limits.into(),
    }
}

pub fn upsert_org_plan_response(plan: OrgPlanConfig) -> UpsertOrgPlanResponse {
    UpsertOrgPlanResponse {
        status: "success",
        plan: plan.into(),
    }
}

impl UpsertOrgPlanRequest {
    pub fn into_plan(
        self,
        org_id: String,
        existing: Option<&OrgPlanConfig>,
    ) -> Result<OrgPlanConfig, MemcoreError> {
        let now = Utc::now();
        let tier = self.tier.parse::<OrgPlanTier>()?;
        validate_org_plan_metadata(self.metadata.as_ref())?;

        let plan = OrgPlanConfig {
            org_id,
            tier,
            limits: self.limits.into_limits()?,
            is_active: self.is_active,
            metadata: self.metadata,
            created_at: existing.map(|plan| plan.created_at).unwrap_or(now),
            updated_at: now,
        };
        plan.validate()?;
        Ok(plan)
    }
}

impl UpsertOrgPlanLimitsRequest {
    fn into_limits(self) -> Result<OrgPlanLimits, MemcoreError> {
        Ok(OrgPlanLimits {
            max_users_per_org: normalize_plan_limit(self.max_users_per_org, "max_users_per_org")?,
            max_memories_per_user: normalize_plan_limit(
                self.max_memories_per_user,
                "max_memories_per_user",
            )?,
            max_memories_per_org: normalize_plan_limit(
                self.max_memories_per_org,
                "max_memories_per_org",
            )?,
            daily_provider_request_limit: normalize_plan_limit(
                self.daily_provider_request_limit,
                "daily_provider_request_limit",
            )?,
            daily_provider_token_limit: normalize_plan_limit(
                self.daily_provider_token_limit,
                "daily_provider_token_limit",
            )?,
        })
    }
}

impl From<OrgPlanConfig> for OrgPlanResponse {
    fn from(plan: OrgPlanConfig) -> Self {
        Self {
            org_id: plan.org_id,
            tier: plan.tier.as_str().to_string(),
            limits: plan.limits.into(),
            is_active: plan.is_active,
            metadata: plan.metadata,
            created_at: plan.created_at,
            updated_at: plan.updated_at,
        }
    }
}

impl From<OrgPlanLimits> for OrgPlanLimitsResponse {
    fn from(limits: OrgPlanLimits) -> Self {
        Self {
            max_users_per_org: limits.max_users_per_org,
            max_memories_per_user: limits.max_memories_per_user,
            max_memories_per_org: limits.max_memories_per_org,
            daily_provider_request_limit: limits.daily_provider_request_limit,
            daily_provider_token_limit: limits.daily_provider_token_limit,
        }
    }
}

fn default_true() -> bool {
    true
}

fn normalize_plan_limit(value: Option<i64>, field: &str) -> Result<Option<u64>, MemcoreError> {
    match value {
        Some(value) if value < 0 => Err(MemcoreError::ValidationError(format!(
            "{field} cannot be negative"
        ))),
        Some(0) | None => Ok(None),
        Some(value) => Ok(Some(value as u64)),
    }
}

impl From<OrgQuotaLimits> for OrgQuotaLimitsResponse {
    fn from(limits: OrgQuotaLimits) -> Self {
        Self {
            enabled: limits.enabled,
            max_users_per_org: limits.max_users_per_org,
            max_memories_per_user: limits.max_memories_per_user,
            max_memories_per_org: limits.max_memories_per_org,
            daily_provider_request_limit: limits.daily_provider_request_limit,
            daily_provider_token_limit: limits.daily_provider_token_limit,
        }
    }
}

impl From<OrgQuotaUsage> for OrgQuotaUsageResponse {
    fn from(usage: OrgQuotaUsage) -> Self {
        Self {
            org_id: usage.org_id,
            total_users: usage.total_users,
            total_memories: usage.total_memories,
            user_memory_count: usage.user_memory_count,
            daily_provider_requests: usage.daily_provider_requests,
            daily_provider_tokens: usage.daily_provider_tokens,
            window_start: usage.window_start,
            window_end: usage.window_end,
        }
    }
}

impl From<QuotaViolation> for QuotaViolationResponse {
    fn from(violation: QuotaViolation) -> Self {
        Self {
            kind: quota_kind_label(violation.kind).to_string(),
            limit: violation.limit,
            current: violation.current,
            requested: violation.requested,
            message: violation.message,
        }
    }
}

fn quota_kind_label(kind: QuotaLimitKind) -> &'static str {
    kind.as_str()
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageResponse {
    pub status: &'static str,
    /// `persistent` reads stored events; `memory` returns process-local aggregates only.
    pub source: String,
    pub summary: ProviderUsageSummaryResponse,
    pub events: Vec<ProviderUsageEventItemResponse>,
    pub next_cursor: Option<String>,
}

/// Safe provider guardrail status (no keys, prompts, or secrets).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderGuardrailsResponse {
    pub status: &'static str,
    pub enabled: bool,
    pub real_provider_calls_enabled: bool,
    pub test_mode: String,
    pub max_calls_per_run: usize,
    pub used_calls: usize,
    pub remaining_calls: usize,
    pub max_input_chars: usize,
    pub max_output_tokens: usize,
    pub max_retries_per_call: usize,
    pub timeout_seconds: u64,
    pub allow_real_providers_during_load_tests: bool,
    pub background_jobs_allow_real_providers: bool,
    pub multi_provider_validation_enabled: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageSummaryResponse {
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

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageEventItemResponse {
    pub id: Uuid,
    pub provider_name: String,
    pub model_name: Option<String>,
    pub capability: String,
    pub operation_name: String,
    pub status: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub retry_count: u64,
    pub fallback_used: bool,
    pub circuit_blocked: bool,
    pub timed_out: bool,
    pub estimated_cost_usd: Option<f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageDailyResponse {
    pub status: &'static str,
    pub usage: ProviderUsageDailyBodyResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageDailyBodyResponse {
    pub org_id: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub buckets: Vec<ProviderUsageDailyBucketResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageDailyBucketResponse {
    pub date: chrono::NaiveDate,
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

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ProviderUsageQueryParams {
    pub user_id: Option<String>,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub capability: Option<String>,
    pub operation_name: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    #[serde(default = "default_provider_usage_query_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
    pub source: Option<String>,
}

pub fn default_provider_usage_query_limit() -> usize {
    memcore_core::DEFAULT_PROVIDER_USAGE_LIMIT
}

pub fn parse_provider_usage_capability(
    value: &str,
) -> Result<memcore_core::ProviderUsageCapability, MemcoreError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "llm" => Ok(memcore_core::ProviderUsageCapability::Llm),
        "embedding" => Ok(memcore_core::ProviderUsageCapability::Embedding),
        "summarization" => Ok(memcore_core::ProviderUsageCapability::Summarization),
        _ => Err(MemcoreError::ValidationError(format!(
            "invalid capability: {value}"
        ))),
    }
}

fn capability_label(capability: memcore_core::ProviderUsageCapability) -> String {
    match capability {
        memcore_core::ProviderUsageCapability::Llm => "Llm".to_string(),
        memcore_core::ProviderUsageCapability::Embedding => "Embedding".to_string(),
        memcore_core::ProviderUsageCapability::Summarization => "Summarization".to_string(),
    }
}

fn status_label(status: memcore_core::ProviderCallStatus) -> String {
    match status {
        memcore_core::ProviderCallStatus::Success => "Success".to_string(),
        memcore_core::ProviderCallStatus::Error => "Error".to_string(),
    }
}

impl From<memcore_core::ProviderUsagePersistedSummary> for ProviderUsageSummaryResponse {
    fn from(summary: memcore_core::ProviderUsagePersistedSummary) -> Self {
        Self {
            total_requests: summary.total_requests,
            total_successes: summary.total_successes,
            total_errors: summary.total_errors,
            total_retries: summary.total_retries,
            total_fallbacks: summary.total_fallbacks,
            total_circuit_blocks: summary.total_circuit_blocks,
            total_timeouts: summary.total_timeouts,
            total_input_tokens: summary.total_input_tokens,
            total_output_tokens: summary.total_output_tokens,
            total_tokens: summary.total_tokens,
            total_estimated_cost_usd: summary.total_estimated_cost_usd,
        }
    }
}

impl From<ProviderUsageDashboardSummary> for ProviderUsageSummaryResponse {
    fn from(summary: ProviderUsageDashboardSummary) -> Self {
        Self {
            total_requests: summary.total_requests,
            total_successes: summary.total_successes,
            total_errors: summary.total_errors,
            total_retries: summary.total_retries,
            total_fallbacks: summary.total_fallbacks,
            total_circuit_blocks: summary.total_circuit_blocks,
            total_timeouts: summary.total_timeouts,
            total_input_tokens: summary.total_input_tokens,
            total_output_tokens: summary.total_output_tokens,
            total_tokens: summary.total_tokens,
            total_estimated_cost_usd: summary.total_estimated_cost_usd,
        }
    }
}

pub fn provider_usage_persisted_response(
    source: &str,
    result: memcore_core::ProviderUsageQueryResult,
) -> ProviderUsageResponse {
    ProviderUsageResponse {
        status: "success",
        source: source.to_string(),
        summary: result.summary.into(),
        events: result
            .events
            .into_iter()
            .map(|event| ProviderUsageEventItemResponse {
                id: event.id,
                provider_name: event.provider_name,
                model_name: event.model_name,
                capability: capability_label(event.capability),
                operation_name: event.operation_name,
                status: status_label(event.status),
                input_tokens: event.input_tokens,
                output_tokens: event.output_tokens,
                total_tokens: event.total_tokens,
                retry_count: event.retry_count,
                fallback_used: event.fallback_used,
                circuit_blocked: event.circuit_blocked,
                timed_out: event.timed_out,
                estimated_cost_usd: event.estimated_cost_usd,
                created_at: event.created_at,
            })
            .collect(),
        next_cursor: result.next_cursor,
    }
}

pub fn provider_usage_memory_response(
    snapshot: memcore_providers::ProviderUsageSnapshot,
) -> ProviderUsageResponse {
    let mut total_input_tokens = 0_u64;
    let mut total_output_tokens = 0_u64;
    let mut total_tokens = 0_u64;
    for record in &snapshot.records {
        total_input_tokens = total_input_tokens.saturating_add(record.input_tokens.unwrap_or(0));
        total_output_tokens = total_output_tokens.saturating_add(record.output_tokens.unwrap_or(0));
        total_tokens = total_tokens.saturating_add(record.total_tokens.unwrap_or(0));
    }

    ProviderUsageResponse {
        status: "success",
        source: "memory".to_string(),
        summary: ProviderUsageSummaryResponse {
            total_requests: snapshot.total_requests,
            total_successes: snapshot.total_successes,
            total_errors: snapshot.total_errors,
            total_retries: snapshot.total_retries,
            total_fallbacks: snapshot.total_fallbacks,
            total_circuit_blocks: snapshot.total_circuit_blocks,
            total_timeouts: snapshot.total_timeouts,
            total_input_tokens,
            total_output_tokens,
            total_tokens,
            total_estimated_cost_usd: snapshot.total_estimated_cost_usd,
        },
        events: Vec::new(),
        next_cursor: None,
    }
}

/// Legacy aggregate response shape (process-local counters by provider key).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageLegacyBodyResponse {
    pub total_requests: u64,
    pub total_successes: u64,
    pub total_errors: u64,
    pub total_retries: u64,
    pub total_fallbacks: u64,
    pub total_circuit_blocks: u64,
    pub total_timeouts: u64,
    pub total_estimated_cost_usd: Option<f64>,
    pub records: Vec<ProviderUsageRecordResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderUsageRecordResponse {
    pub provider_name: String,
    pub model_name: Option<String>,
    pub capability: String,
    pub operation_name: String,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub retry_count: u64,
    pub fallback_count: u64,
    pub circuit_blocked_count: u64,
    pub timeout_count: u64,
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgSummaryResponse {
    pub status: &'static str,
    pub summary: OrgSummaryBodyResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgSummaryBodyResponse {
    pub org_id: String,
    pub total_users: usize,
    pub total_facts: usize,
    pub total_events: Option<usize>,
    /// Episodes, counted separately from facts; present only when episodes are enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_episodes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListOrgUsersResponse {
    pub status: &'static str,
    pub users: Vec<OrgUserSummaryResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrgUserSummaryResponse {
    pub user_id: String,
    pub memory_count: usize,
    pub last_memory_at: Option<DateTime<Utc>>,
}

impl From<&MemoryEvent> for AdminOrgMemoryEventItemResponse {
    fn from(event: &MemoryEvent) -> Self {
        Self {
            id: event.id,
            user_id: event.user_id.clone(),
            fact_id: event.fact_id,
            operation: event.operation.into(),
            previous_content: event.previous_content.clone(),
            new_content: event.new_content.clone(),
            provider_name: event.provider_name.clone(),
            model_name: event.model_name.clone(),
            metadata: event.metadata.clone(),
            created_at: event.created_at,
        }
    }
}

impl From<OrgSummaryOutput> for OrgSummaryResponse {
    fn from(output: OrgSummaryOutput) -> Self {
        Self {
            status: "success",
            summary: OrgSummaryBodyResponse {
                org_id: output.org_id,
                total_users: output.total_users,
                total_facts: output.total_facts,
                total_events: output.total_events,
                total_episodes: output.total_episodes,
            },
        }
    }
}

impl From<OrgMemoryUsageSummary> for OrgMemoryUsageSummaryResponse {
    fn from(summary: OrgMemoryUsageSummary) -> Self {
        Self {
            total_users: summary.total_users,
            total_memories: summary.total_memories,
            active_memories: summary.active_memories,
            deleted_memories: summary.deleted_memories,
            latest_snapshot: summary
                .latest_snapshot
                .map(MemoryUsageLatestSnapshotResponse::from),
        }
    }
}

impl From<MemoryUsageLatestSnapshot> for MemoryUsageLatestSnapshotResponse {
    fn from(snapshot: MemoryUsageLatestSnapshot) -> Self {
        Self {
            captured_at: snapshot.captured_at,
            total_users: snapshot.total_users,
            total_memories: snapshot.total_memories,
            active_memories: snapshot.active_memories,
            deleted_memories: snapshot.deleted_memories,
        }
    }
}

impl From<MemoryUsageSnapshot> for MemoryUsageSnapshotResponse {
    fn from(snapshot: MemoryUsageSnapshot) -> Self {
        Self {
            id: snapshot.id,
            org_id: snapshot.org_id,
            total_users: snapshot.total_users,
            total_memories: snapshot.total_memories,
            active_memories: snapshot.active_memories,
            deleted_memories: snapshot.deleted_memories,
            captured_at: snapshot.captured_at,
        }
    }
}

impl From<CreateMemoryUsageSnapshotOutput> for CreateMemoryUsageSnapshotResponse {
    fn from(output: CreateMemoryUsageSnapshotOutput) -> Self {
        Self {
            status: "success",
            snapshot: output.snapshot.into(),
        }
    }
}

impl From<QueryMemoryUsageSnapshotsOutput> for QueryMemoryUsageSnapshotsResponse {
    fn from(output: QueryMemoryUsageSnapshotsOutput) -> Self {
        Self {
            status: "success",
            snapshots: output
                .snapshots
                .into_iter()
                .map(MemoryUsageSnapshotResponse::from)
                .collect(),
            next_cursor: output.next_cursor,
        }
    }
}

impl From<OrgUsageDashboardOutput> for OrgUsageDashboardResponse {
    fn from(output: OrgUsageDashboardOutput) -> Self {
        let quota = org_quota_status_response(output.quota).quotas;
        Self {
            status: "success",
            dashboard: OrgUsageDashboardBodyResponse {
                org_id: output.org_id,
                generated_at: output.generated_at,
                window_start: output.window_start,
                window_end: output.window_end,
                plan: output.plan.map(OrgPlanResponse::from),
                quota,
                memory: output.memory.into(),
                provider: output.provider.into(),
            },
        }
    }
}

impl From<ProviderUsageDailyBucket> for ProviderUsageDailyBucketResponse {
    fn from(bucket: ProviderUsageDailyBucket) -> Self {
        Self {
            date: bucket.date,
            total_requests: bucket.total_requests,
            total_successes: bucket.total_successes,
            total_errors: bucket.total_errors,
            total_retries: bucket.total_retries,
            total_fallbacks: bucket.total_fallbacks,
            total_circuit_blocks: bucket.total_circuit_blocks,
            total_timeouts: bucket.total_timeouts,
            total_input_tokens: bucket.total_input_tokens,
            total_output_tokens: bucket.total_output_tokens,
            total_tokens: bucket.total_tokens,
            total_estimated_cost_usd: bucket.total_estimated_cost_usd,
        }
    }
}

impl From<ProviderUsageDailyOutput> for ProviderUsageDailyResponse {
    fn from(output: ProviderUsageDailyOutput) -> Self {
        Self {
            status: "success",
            usage: ProviderUsageDailyBodyResponse {
                org_id: output.org_id,
                window_start: output.window_start,
                window_end: output.window_end,
                buckets: output
                    .buckets
                    .into_iter()
                    .map(ProviderUsageDailyBucketResponse::from)
                    .collect(),
            },
        }
    }
}

impl From<OrgUserSummary> for OrgUserSummaryResponse {
    fn from(summary: OrgUserSummary) -> Self {
        Self {
            user_id: summary.user_id,
            memory_count: summary.memory_count,
            last_memory_at: summary.last_memory_at,
        }
    }
}

impl From<ListOrgUsersOutput> for ListOrgUsersResponse {
    fn from(output: ListOrgUsersOutput) -> Self {
        Self {
            status: "success",
            users: output
                .users
                .into_iter()
                .map(OrgUserSummaryResponse::from)
                .collect(),
            next_cursor: output.next_cursor,
        }
    }
}

impl From<SearchOrgMemoryEventsOutput> for SearchOrgMemoryEventsResponse {
    fn from(output: SearchOrgMemoryEventsOutput) -> Self {
        Self {
            status: "success",
            events: output
                .events
                .iter()
                .map(AdminOrgMemoryEventItemResponse::from)
                .collect(),
            next_cursor: output.next_cursor,
        }
    }
}

impl ListOrgUsersQuery {
    pub fn into_input(self, org_id: String) -> ListOrgUsersInput {
        ListOrgUsersInput {
            org_id,
            limit: self.limit,
            cursor: self.cursor,
        }
    }
}

pub fn org_summary_input(org_id: String) -> OrgSummaryInput {
    OrgSummaryInput { org_id }
}

pub fn org_usage_dashboard_input(
    org_id: String,
    query: OrgUsageDateRangeQuery,
) -> Result<OrgUsageDashboardInput, MemcoreError> {
    let (created_after, created_before) = parse_org_usage_window(
        query.created_after.as_ref(),
        query.created_before.as_ref(),
        query.days,
    )?;
    Ok(OrgUsageDashboardInput {
        org_id,
        created_after,
        created_before,
    })
}

impl CreateMemoryUsageSnapshotRequest {
    pub fn into_input(
        self,
        org_id: String,
    ) -> Result<CreateMemoryUsageSnapshotInput, MemcoreError> {
        let captured_at = self
            .captured_at
            .as_deref()
            .map(parse_rfc3339_utc)
            .transpose()
            .map_err(|_| {
                MemcoreError::ValidationError("invalid captured_at timestamp".to_string())
            })?;

        Ok(CreateMemoryUsageSnapshotInput {
            org_id,
            captured_at,
        })
    }
}

pub fn query_memory_usage_snapshots_input(
    org_id: String,
    query: QueryMemoryUsageSnapshotsParams,
) -> Result<QueryMemoryUsageSnapshotsInput, MemcoreError> {
    let (created_after, created_before) = crate::dto::memory_events::parse_event_date_filters(
        query.created_after.as_ref(),
        query.created_before.as_ref(),
    )?;
    let limit = memcore_core::validate_memory_usage_snapshot_limit(query.limit)?;
    let cursor = memcore_core::parse_optional_cursor(query.cursor)?;

    Ok(QueryMemoryUsageSnapshotsInput {
        org_id,
        created_after,
        created_before,
        limit,
        cursor,
    })
}

pub fn parse_org_usage_window(
    created_after: Option<&String>,
    created_before: Option<&String>,
    days: Option<u32>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), MemcoreError> {
    let (created_after, created_before) =
        crate::dto::memory_events::parse_event_date_filters(created_after, created_before)?;
    resolve_org_usage_window(created_after, created_before, days, Utc::now())
}

fn parse_rfc3339_utc(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).map(|parsed| parsed.with_timezone(&Utc))
}

pub fn validate_list_org_users_limit(limit: usize) -> Result<(), memcore_common::MemcoreError> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if limit > MAX_LIST_ORG_USERS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_LIST_ORG_USERS_LIMIT}"
        )));
    }

    Ok(())
}

pub fn validate_search_org_memory_events_limit(
    limit: usize,
) -> Result<(), memcore_common::MemcoreError> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if limit > MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_org_users_limit_defaults_to_fifty() {
        let json = r#"{}"#;
        let query: ListOrgUsersQuery =
            serde_json::from_str(json).expect("deserialize list org users query");
        assert_eq!(query.limit, DEFAULT_LIST_ORG_USERS_LIMIT);
    }

    #[test]
    fn search_org_memory_events_limit_defaults_to_fifty() {
        let json = r#"{}"#;
        let query: SearchOrgMemoryEventsQuery =
            serde_json::from_str(json).expect("deserialize search org memory events query");
        assert_eq!(query.limit, DEFAULT_SEARCH_ORG_MEMORY_EVENTS_LIMIT);
    }
}
