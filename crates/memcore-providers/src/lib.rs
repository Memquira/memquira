pub mod circuit_breaker;
pub mod factory;
pub mod guardrails;
pub mod inputs;
pub mod local_embedding;
pub mod mocks;
pub mod openai;
pub mod policy;
pub mod prompts;
mod redaction;
pub mod reference_gateway;
pub mod rerank;
pub mod resilient;
pub mod routing;
pub mod structured;
pub mod traits;
pub mod usage;

pub use circuit_breaker::{
    CircuitBreakerConfig, CircuitBreakerSnapshot, CircuitState, ProviderCircuitBreaker,
    validate_circuit_breaker_config,
};
pub use factory::{
    parse_provider_fallback_order, validate_embedding_provider_name, validate_llm_provider_name,
    validate_provider_fallback_order, validate_summarizer_provider_name,
};
pub use guardrails::{
    MULTI_PROVIDER_VALIDATION_CONFIRMATION, ProviderCallBudget, ProviderCallGuardInput,
    ProviderCallSource, ProviderCallSourceSlot, ProviderGuardDecision, ProviderGuardrailConfig,
    ProviderGuardrailEnforcer, ProviderGuardrailStatus, ProviderTestMode, is_real_provider_name,
    parse_provider_test_mode,
};
pub use inputs::{
    FactClassificationInput, FactExtractionInput, MemoryMessage, MessageRole, SummarizationInput,
};
pub use mocks::{
    ConsolidationDecider, MockEmbeddingProvider, MockLlmProvider, deterministic_embedding,
};
pub use openai::{
    CLASSIFICATION_INSTRUCTIONS_OPERATIONS_V1, ChatJsonMode, ClassificationPrompt,
    EmbeddingOptions, OpenAiClient, OpenAiEmbeddingProvider, OpenAiLlmProvider, OpenAiLlmTransport,
    OutputTokenLimits, default_embedding_dimensions_for_model, parse_fact_extraction_response,
    parse_memory_type_label,
};
pub use policy::{
    ProviderExecutionOutcome, ProviderExecutionPolicy, ProviderRetryDecision, backoff_duration,
    execute_provider_call, is_provider_health_failure, is_retryable_provider_error,
    provider_timeout_error, validate_provider_execution_config,
};
pub use prompts::{
    StructuredExtractionPolicy, fact_extraction_instructions, fact_extraction_json_schema,
};
pub use resilient::{
    PolicyEmbeddingProvider, PolicyLlmProvider, ResilientEmbeddingProvider, ResilientLlmProvider,
    build_resilient_embedding_from_candidates,
    build_resilient_embedding_from_candidates_with_guardrails, build_resilient_llm_from_candidates,
    build_resilient_llm_from_candidates_with_guardrails, wrap_embedding_provider,
    wrap_llm_provider,
};
pub use routing::{
    ProviderCallContext, ProviderCandidate, ProviderCapability, ProviderFallbackRouter, ProviderId,
    ProviderRoutingMetrics, ProviderRoutingMetricsSnapshot, circuit_key,
};
pub use traits::{EmbeddingProvider, LlmProvider};
pub use usage::{
    InMemoryProviderUsageRecorder, NoopProviderUsageRecorder, PersistentProviderUsageRecorder,
    ProviderCallStatus, ProviderCostCalculator, ProviderPricing, ProviderTokenUsage,
    ProviderUsageAttribution, ProviderUsageAttributionSlot, ProviderUsageCapability,
    ProviderUsageEvent, ProviderUsageRecord, ProviderUsageRecorder, ProviderUsageSnapshot,
    estimate_event_cost, lookup_pricing, new_token_usage_slot, provider_usage_recorder,
};
