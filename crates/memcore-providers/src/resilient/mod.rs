mod compat;

use std::sync::Arc;

use async_trait::async_trait;
use memcore_common::MemcoreResult;
use memcore_core::{FactExtractionOutput, FactOperationDecision};

use crate::circuit_breaker::ProviderCircuitBreaker;
use crate::guardrails::{ProviderCallSourceSlot, ProviderGuardrailEnforcer};
use crate::inputs::{FactClassificationInput, FactExtractionInput, SummarizationInput};
use crate::policy::ProviderExecutionPolicy;
use crate::routing::{
    ProviderCandidate, ProviderCapability, ProviderFallbackRouter, ProviderRoutingMetrics,
};
use crate::traits::{EmbeddingProvider, LlmProvider};
use crate::usage::{
    ProviderUsageAttributionSlot, ProviderUsageRecorder, TokenUsageSlot,
    estimate_embedding_batch_tokens, estimate_embedding_tokens, estimate_llm_classification_tokens,
    estimate_llm_extraction_tokens, estimate_llm_summarization_tokens, store_token_usage,
};

/// LLM provider with timeout/retry, circuit breaker, optional fallback routing, and usage recording.
pub struct ResilientLlmProvider {
    providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    router: ProviderFallbackRouter,
    fallback_enabled: bool,
}

impl ResilientLlmProvider {
    pub fn new(
        providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
        summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
        circuit_breaker: Arc<ProviderCircuitBreaker>,
        policy: ProviderExecutionPolicy,
        fallback_enabled: bool,
        metrics: Option<Arc<ProviderRoutingMetrics>>,
        usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
        attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
        cost_tracking_enabled: bool,
    ) -> Self {
        Self::with_guardrails(
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

    pub fn with_guardrails(
        providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
        summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
        circuit_breaker: Arc<ProviderCircuitBreaker>,
        policy: ProviderExecutionPolicy,
        fallback_enabled: bool,
        metrics: Option<Arc<ProviderRoutingMetrics>>,
        usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
        attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
        cost_tracking_enabled: bool,
        guardrails: Option<Arc<ProviderGuardrailEnforcer>>,
        call_source_slot: Option<Arc<ProviderCallSourceSlot>>,
    ) -> Self {
        Self {
            providers,
            summarizer_providers,
            router: ProviderFallbackRouter::with_guardrails(
                circuit_breaker,
                policy,
                metrics,
                usage_recorder,
                attribution_slot,
                cost_tracking_enabled,
                guardrails,
                call_source_slot,
            ),
            fallback_enabled,
        }
    }
}

pub fn build_resilient_llm_provider(
    providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
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
        None,
        None,
    )
}

pub fn build_resilient_llm_provider_with_guardrails(
    providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    summarizer_providers: Vec<ProviderCandidate<Arc<dyn LlmProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
    guardrails: Option<Arc<ProviderGuardrailEnforcer>>,
    call_source_slot: Option<Arc<ProviderCallSourceSlot>>,
) -> Arc<dyn LlmProvider> {
    Arc::new(ResilientLlmProvider::with_guardrails(
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
    ))
}

fn store_estimated_usage(slot: Option<TokenUsageSlot>, usage: crate::usage::ProviderTokenUsage) {
    // The per-call capture is what the router records (the shared slot is kept
    // for callers outside the router).
    crate::usage::capture_estimated_usage(usage);
    if let Some(slot) = slot {
        store_token_usage(&slot, usage);
    }
}

fn messages_char_count(messages: &[memcore_core::MemoryMessage]) -> usize {
    messages.iter().map(|message| message.content.len()).sum()
}

#[async_trait]
impl LlmProvider for ResilientLlmProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        let input_chars = messages_char_count(&input.messages);
        self.router
            .execute_with_fallback(
                ProviderCapability::Llm,
                "llm_extract_facts",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let facts = provider.extract_facts(input.clone()).await?;
                        store_estimated_usage(slot, estimate_llm_extraction_tokens(&input));
                        Ok(facts)
                    }
                },
            )
            .await
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        let input_chars = input.candidate_fact.content.len()
            + input
                .existing_facts
                .iter()
                .map(|fact| fact.content.len())
                .sum::<usize>();
        self.router
            .execute_with_fallback(
                ProviderCapability::Llm,
                "llm_classify_fact_operation",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let decision = provider.classify_fact_operation(input.clone()).await?;
                        store_estimated_usage(
                            slot,
                            estimate_llm_classification_tokens(
                                &input.candidate_fact.content,
                                input.existing_facts.len(),
                            ),
                        );
                        Ok(decision)
                    }
                },
            )
            .await
    }

    async fn decide_consolidation(
        &self,
        input: memcore_core::ports::ConsolidationInput,
    ) -> MemcoreResult<memcore_core::ports::ConsolidationDecision> {
        let input_chars = input.candidate_fact.content.len()
            + input
                .existing_facts
                .iter()
                .map(|fact| fact.content.len())
                .sum::<usize>();
        self.router
            .execute_with_fallback(
                ProviderCapability::Llm,
                "llm_decide_consolidation",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let decision = provider.decide_consolidation(input.clone()).await?;
                        store_estimated_usage(
                            slot,
                            estimate_llm_classification_tokens(
                                &input.candidate_fact.content,
                                input.existing_facts.len(),
                            ),
                        );
                        Ok(decision)
                    }
                },
            )
            .await
    }

    /// One logical operation through the router. The router's
    /// retry policy (`MEMCORE_PROVIDER_MAX_RETRIES`) is transport behaviour below the operation and
    /// is recorded in usage, never counted as another decomposition operation.
    async fn decompose_query(
        &self,
        input: memcore_core::ports::QueryDecompositionInput,
    ) -> MemcoreResult<String> {
        let input_chars = input.system.len() + input.user.len();
        let max_output_tokens = input.max_output_tokens;
        self.router
            .execute_with_fallback(
                ProviderCapability::Llm,
                "llm_decompose_query",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                Some(max_output_tokens as usize),
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let user = input.user.clone();
                        let text = provider.decompose_query(input).await?;
                        store_estimated_usage(slot, estimate_llm_classification_tokens(&user, 0));
                        Ok(text)
                    }
                },
            )
            .await
    }

    async fn annotate_entities(
        &self,
        input: memcore_core::ports::EntityAnnotationInput,
    ) -> MemcoreResult<memcore_core::ports::EntityAnnotationOutput> {
        let input_chars = input
            .messages
            .iter()
            .map(|message| message.content.len())
            .sum::<usize>()
            + input
                .facts
                .iter()
                .map(|fact| fact.content.len())
                .sum::<usize>();
        let operation = match input.attempt {
            memcore_core::ports::AnnotationAttempt::First => "llm_annotate_entities",
            memcore_core::ports::AnnotationAttempt::Retry => "llm_annotate_entities_retry",
        };
        self.router
            .execute_with_fallback(
                ProviderCapability::Llm,
                operation,
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let output = provider.annotate_entities(input.clone()).await?;
                        store_estimated_usage(
                            slot,
                            estimate_llm_classification_tokens(
                                &input
                                    .facts
                                    .iter()
                                    .map(|fact| fact.content.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                input.messages.len(),
                            ),
                        );
                        Ok(output)
                    }
                },
            )
            .await
    }

    async fn confirm_entity_match(
        &self,
        input: memcore_core::ports::EntityMatchInput,
    ) -> MemcoreResult<bool> {
        let input_chars = input.mention.len() + input.context.len() + input.candidate_name.len();
        self.router
            .execute_with_fallback(
                ProviderCapability::Llm,
                "llm_confirm_entity_match",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let same = provider.confirm_entity_match(input.clone()).await?;
                        store_estimated_usage(
                            slot,
                            estimate_llm_classification_tokens(&input.context, 1),
                        );
                        Ok(same)
                    }
                },
            )
            .await
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        let providers = if self.summarizer_providers.is_empty() {
            &self.providers
        } else {
            &self.summarizer_providers
        };
        let input_chars = input.facts.iter().map(|fact| fact.content.len()).sum();
        let requested_tokens = input.max_tokens;
        self.router
            .execute_with_fallback(
                ProviderCapability::Summarization,
                "llm_summarize_memory",
                self.fallback_enabled,
                providers,
                input_chars,
                requested_tokens,
                |provider, slot| {
                    let input = input.clone();
                    async move {
                        let summary = provider.summarize_memory(input.clone()).await?;
                        store_estimated_usage(slot, estimate_llm_summarization_tokens(&input));
                        Ok(summary)
                    }
                },
            )
            .await
    }
}

/// Embedding provider with timeout/retry, circuit breaker, optional fallback routing, and usage recording.
pub struct ResilientEmbeddingProvider {
    providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
    router: ProviderFallbackRouter,
    fallback_enabled: bool,
    dimensions: usize,
}

impl ResilientEmbeddingProvider {
    pub fn new(
        providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
        circuit_breaker: Arc<ProviderCircuitBreaker>,
        policy: ProviderExecutionPolicy,
        fallback_enabled: bool,
        metrics: Option<Arc<ProviderRoutingMetrics>>,
        usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
        attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
        cost_tracking_enabled: bool,
    ) -> MemcoreResult<Self> {
        Self::with_guardrails(
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

    pub fn with_guardrails(
        providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
        circuit_breaker: Arc<ProviderCircuitBreaker>,
        policy: ProviderExecutionPolicy,
        fallback_enabled: bool,
        metrics: Option<Arc<ProviderRoutingMetrics>>,
        usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
        attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
        cost_tracking_enabled: bool,
        guardrails: Option<Arc<ProviderGuardrailEnforcer>>,
        call_source_slot: Option<Arc<ProviderCallSourceSlot>>,
    ) -> MemcoreResult<Self> {
        let dimensions = providers
            .first()
            .map(|candidate| candidate.provider.dimensions())
            .ok_or_else(|| {
                memcore_common::MemcoreError::ValidationError(
                    "at least one embedding provider is required".to_string(),
                )
            })?;

        for candidate in providers.iter().skip(1) {
            if candidate.provider.dimensions() != dimensions {
                return Err(memcore_common::MemcoreError::ValidationError(
                    "all embedding providers in fallback order must share the same dimensions"
                        .to_string(),
                ));
            }
        }

        Ok(Self {
            providers,
            router: ProviderFallbackRouter::with_guardrails(
                circuit_breaker,
                policy,
                metrics,
                usage_recorder,
                attribution_slot,
                cost_tracking_enabled,
                guardrails,
                call_source_slot,
            ),
            fallback_enabled,
            dimensions,
        })
    }
}

pub fn build_resilient_embedding_provider(
    providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
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
        None,
        None,
    )
}

pub fn build_resilient_embedding_provider_with_guardrails(
    providers: Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>,
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    policy: ProviderExecutionPolicy,
    fallback_enabled: bool,
    metrics: Option<Arc<ProviderRoutingMetrics>>,
    usage_recorder: Option<Arc<dyn ProviderUsageRecorder>>,
    attribution_slot: Option<Arc<ProviderUsageAttributionSlot>>,
    cost_tracking_enabled: bool,
    guardrails: Option<Arc<ProviderGuardrailEnforcer>>,
    call_source_slot: Option<Arc<ProviderCallSourceSlot>>,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    Ok(Arc::new(ResilientEmbeddingProvider::with_guardrails(
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
    )?))
}

#[async_trait]
impl EmbeddingProvider for ResilientEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let text = text.to_string();
        let input_chars = text.len();
        self.router
            .execute_with_fallback(
                ProviderCapability::Embedding,
                "embedding_embed_text",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let text = text.clone();
                    async move {
                        let embedding = provider.embed_text(&text).await?;
                        store_estimated_usage(slot, estimate_embedding_tokens(&text));
                        Ok(embedding)
                    }
                },
            )
            .await
    }

    async fn embed_query(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let text = text.to_string();
        let input_chars = text.len();
        self.router
            .execute_with_fallback(
                ProviderCapability::Embedding,
                "embedding_embed_query",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let text = text.clone();
                    async move {
                        let embedding = provider.embed_query(&text).await?;
                        store_estimated_usage(slot, estimate_embedding_tokens(&text));
                        Ok(embedding)
                    }
                },
            )
            .await
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let input_chars = texts.iter().map(|text| text.len()).sum();
        self.router
            .execute_with_fallback(
                ProviderCapability::Embedding,
                "embedding_embed_batch",
                self.fallback_enabled,
                &self.providers,
                input_chars,
                None,
                |provider, slot| {
                    let texts = texts.clone();
                    async move {
                        let embeddings = provider.embed_batch(texts.clone()).await?;
                        store_estimated_usage(slot, estimate_embedding_batch_tokens(&texts));
                        Ok(embeddings)
                    }
                },
            )
            .await
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// The first routed provider's diagnostics (a local model reports its queue here).
    fn diagnostics(&self) -> Option<serde_json::Value> {
        self.providers
            .iter()
            .find_map(|candidate| candidate.provider.diagnostics())
    }

    /// Known only when every routed provider has the same known identity: with fallback, a vector
    /// may come from any of them, so mixed or unknown identities are unknown.
    fn embedding_identity(&self) -> Option<memcore_core::ports::EmbeddingIdentity> {
        let mut identities = self
            .providers
            .iter()
            .map(|candidate| candidate.provider.embedding_identity());
        let first = identities.next()??;
        identities
            .all(|identity| identity.as_ref() == Some(&first))
            .then_some(first)
    }
}

pub use compat::{
    PolicyEmbeddingProvider, PolicyLlmProvider, build_resilient_embedding_from_candidates,
    build_resilient_embedding_from_candidates_with_guardrails, build_resilient_llm_from_candidates,
    build_resilient_llm_from_candidates_with_guardrails, wrap_embedding_provider,
    wrap_llm_provider,
};
