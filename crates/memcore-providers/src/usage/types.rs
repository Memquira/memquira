use serde::{Deserialize, Serialize};

pub use memcore_core::{ProviderCallStatus, ProviderUsageCapability};

/// Token usage reported by a provider when available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderTokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl ProviderTokenUsage {
    pub fn from_counts(input_tokens: Option<u64>, output_tokens: Option<u64>) -> Self {
        let total_tokens = match (input_tokens, output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output)),
            (Some(input), None) => Some(input),
            (None, Some(output)) => Some(output),
            (None, None) => None,
        };
        Self {
            input_tokens,
            output_tokens,
            total_tokens,
        }
    }
}

/// Single provider usage event (no prompts, memory content, or secrets).
#[derive(Debug, Clone)]
pub struct ProviderUsageEvent {
    pub org_id: Option<String>,
    pub user_id: Option<String>,
    pub provider_name: String,
    pub model_name: Option<String>,
    pub capability: ProviderUsageCapability,
    pub operation_name: String,
    pub status: ProviderCallStatus,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub retry_count: u64,
    pub fallback_used: bool,
    pub circuit_blocked: bool,
    pub timed_out: bool,
    pub estimated_cost_usd: Option<f64>,
    /// Where `input_tokens` / `output_tokens` came from: provider-reported,
    /// partially reported, or memcore's character-count estimate.
    pub token_source: super::capture::TokenSource,
    /// What the provider responses said about usage (`reported`, `partial`, `missing`,
    /// `malformed`).
    pub usage_report: super::capture::UsageReportStatus,
}

/// Aggregated usage for one provider/model/capability/operation key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageRecord {
    pub provider_name: String,
    pub model_name: Option<String>,
    pub capability: ProviderUsageCapability,
    pub operation_name: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub retry_count: u64,
    pub fallback_count: u64,
    pub circuit_blocked_count: u64,
    pub timeout_count: u64,
    pub estimated_cost_usd: Option<f64>,
}

/// Process-local aggregate provider usage snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageSnapshot {
    pub records: Vec<ProviderUsageRecord>,
    pub total_requests: u64,
    pub total_successes: u64,
    pub total_errors: u64,
    pub total_retries: u64,
    pub total_fallbacks: u64,
    pub total_circuit_blocks: u64,
    pub total_timeouts: u64,
    pub total_estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UsageAggregateKey {
    pub provider_name: String,
    pub model_name: Option<String>,
    pub capability: ProviderUsageCapability,
    pub operation_name: String,
}

impl UsageAggregateKey {
    pub fn from_event(event: &ProviderUsageEvent) -> Self {
        Self {
            provider_name: event.provider_name.clone(),
            model_name: event.model_name.clone(),
            capability: event.capability,
            operation_name: event.operation_name.clone(),
        }
    }
}
