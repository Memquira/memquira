use std::sync::Arc;

use memcore_common::MemcoreResult;

use crate::circuit_breaker::{CircuitBreakerConfig, ProviderCircuitBreaker};
use crate::policy::ProviderExecutionPolicy;
use crate::routing::{ProviderCandidate, ProviderCapability, ProviderId, ProviderRoutingMetrics};
use crate::traits::{EmbeddingProvider, LlmProvider};
use crate::usage::{NoopProviderUsageRecorder, ProviderUsageRecorder, new_token_usage_slot};

use super::{
    build_resilient_embedding_provider, build_resilient_embedding_provider_with_guardrails,
    build_resilient_llm_provider, build_resilient_llm_provider_with_guardrails,
};

pub use super::{
    ResilientEmbeddingProvider as PolicyEmbeddingProvider,
    ResilientLlmProvider as PolicyLlmProvider,
};

fn disabled_circuit_breaker() -> Arc<ProviderCircuitBreaker> {
    Arc::new(ProviderCircuitBreaker::new(CircuitBreakerConfig {
        enabled: false,
        ..CircuitBreakerConfig::default()
    }))
}

fn noop_usage_recorder() -> Arc<dyn ProviderUsageRecorder> {
    Arc::new(NoopProviderUsageRecorder)
}

/// Backward-compatible single-provider wrapper (circuit breaker and fallback disabled).
pub fn wrap_llm_provider(
    inner: Arc<dyn LlmProvider>,
    policy: ProviderExecutionPolicy,
) -> Arc<dyn LlmProvider> {
    let usage_slot = new_token_usage_slot();
    let providers = vec![ProviderCandidate::new(
        ProviderId::new("primary", ProviderCapability::Llm),
        inner.clone(),
        Some("mock-llm".to_string()),
        Some(usage_slot.clone()),
    )];
    let summarizer_providers = vec![ProviderCandidate::new(
        ProviderId::new("primary", ProviderCapability::Summarization),
        inner,
        Some("mock-llm".to_string()),
        Some(usage_slot),
    )];
    build_resilient_llm_provider(
        providers,
        summarizer_providers,
        disabled_circuit_breaker(),
        policy,
        false,
        None,
        Some(noop_usage_recorder()),
        None,
        false,
    )
}

/// Backward-compatible single-provider wrapper (circuit breaker and fallback disabled).
pub fn wrap_embedding_provider(
    inner: Arc<dyn EmbeddingProvider>,
    policy: ProviderExecutionPolicy,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    let providers = vec![ProviderCandidate::new(
        ProviderId::new("primary", ProviderCapability::Embedding),
        inner,
        Some("mock-embedding".to_string()),
        Some(new_token_usage_slot()),
    )];
    build_resilient_embedding_provider(
        providers,
        disabled_circuit_breaker(),
        policy,
        false,
        None,
        Some(noop_usage_recorder()),
        None,
        false,
    )
}

pub fn build_resilient_llm_from_candidates(
    providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<crate::usage::ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
) -> Arc<dyn LlmProvider> {
    build_resilient_llm_from_candidates_with_guardrails(
        providers,
        summarizer_providers,
        circuit_breaker,
        policy,
        fallback_enabled,
        metrics,
        usage_recorder,
        attribution_slot,
        cost_tracking_enabled,
        None,
        None,
    )
}

pub fn build_resilient_llm_from_candidates_with_guardrails(
    providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<crate::usage::ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
    guardrails: Option<Arc<crate::guardrails::ProviderGuardrailEnforcer>>,
    call_source_slot: Option<Arc<crate::guardrails::ProviderCallSourceSlot>>,
) -> Arc<dyn LlmProvider> {
    build_resilient_llm_provider_with_guardrails(
        providers,
        summarizer_providers,
        circuit_breaker,
        policy,
        fallback_enabled,
        metrics,
        usage_recorder,
        attribution_slot,
        cost_tracking_enabled,
        guardrails,
        call_source_slot,
    )
}

pub fn build_resilient_embedding_from_candidates(
    providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<crate::usage::ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    build_resilient_embedding_from_candidates_with_guardrails(
        providers,
        circuit_breaker,
        policy,
        fallback_enabled,
        metrics,
        usage_recorder,
        attribution_slot,
        cost_tracking_enabled,
        None,
        None,
    )
}

pub fn build_resilient_embedding_from_candidates_with_guardrails(
    providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<crate::usage::ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
    guardrails: Option<Arc<crate::guardrails::ProviderGuardrailEnforcer>>,
    call_source_slot: Option<Arc<crate::guardrails::ProviderCallSourceSlot>>,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    build_resilient_embedding_provider_with_guardrails(
        providers,
        circuit_breaker,
        policy,
        fallback_enabled,
        metrics,
        usage_recorder,
        attribution_slot,
        cost_tracking_enabled,
        guardrails,
        call_source_slot,
    )
}
