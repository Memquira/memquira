use std::sync::Arc;

use memcore_common::MemcoreError;
use memcore_core::{
    AddMemoryInput, MemoryEngine, MemoryMessage, MessageRole, SearchMemoryInput, TenantContext,
};
use memcore_providers::{
    CircuitBreakerConfig, InMemoryProviderUsageRecorder, MockEmbeddingProvider, MockLlmProvider,
    ProviderCandidate, ProviderCapability, ProviderCircuitBreaker, ProviderExecutionPolicy,
    ProviderId, ProviderRoutingMetrics, ProviderUsageRecorder,
    build_resilient_embedding_from_candidates, build_resilient_llm_from_candidates,
    provider_usage_recorder,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

fn tenant() -> TenantContext {
    TenantContext::new("org_fb", "user_fb").expect("tenant")
}

fn fast_policy() -> ProviderExecutionPolicy {
    ProviderExecutionPolicy {
        max_retries: 0,
        timeout: std::time::Duration::from_millis(200),
        initial_backoff: std::time::Duration::from_millis(1),
        max_backoff: std::time::Duration::from_millis(1),
        jitter_enabled: false,
        backoff_multiplier: 2.0,
    }
}

fn test_circuit_breaker() -> Arc<ProviderCircuitBreaker> {
    Arc::new(ProviderCircuitBreaker::new(
        CircuitBreakerConfig::for_tests(),
    ))
}

fn llm_candidate(
    name: &str,
    provider: Arc<MockLlmProvider>,
) -> ProviderCandidate<Arc<dyn memcore_core::LlmProvider>> {
    ProviderCandidate::new(
        ProviderId::new(name, ProviderCapability::Llm),
        provider as Arc<dyn memcore_core::LlmProvider>,
        Some("mock-llm".to_string()),
        None,
    )
}

fn embedding_candidate(
    name: &str,
    provider: Arc<MockEmbeddingProvider>,
) -> ProviderCandidate<Arc<dyn memcore_core::EmbeddingProvider>> {
    ProviderCandidate::new(
        ProviderId::new(name, ProviderCapability::Embedding),
        provider as Arc<dyn memcore_core::EmbeddingProvider>,
        Some("mock-embedding".to_string()),
        None,
    )
}

#[tokio::test]
async fn llm_extraction_falls_back_to_healthy_mock_provider() {
    let failing = Arc::new(
        MockLlmProvider::new().with_fail_error(MemcoreError::ProviderError(
            "OpenAI API error (503): unavailable".to_string(),
        )),
    );
    let healthy = Arc::new(MockLlmProvider::new());
    let usage = InMemoryProviderUsageRecorder::new();
    let llm = build_resilient_llm_from_candidates(
        vec![
            llm_candidate("primary", failing),
            llm_candidate("secondary", healthy),
        ],
        vec![],
        test_circuit_breaker(),
        fast_policy(),
        true,
        Some(ProviderRoutingMetrics::new()),
        Some(usage.clone()),
        None,
        false,
    );

    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm,
        Arc::new(MockEmbeddingProvider::new(4)),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "User enjoys hiking.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("fallback add should succeed");

    assert_eq!(output.added, 1);
    let snapshot = usage.snapshot();
    assert!(snapshot.total_requests >= 2);
    assert!(snapshot.total_successes >= 1);
    assert!(snapshot.total_fallbacks >= 1);
}

#[tokio::test]
async fn embedding_search_falls_back_to_healthy_mock_provider() {
    let failing = Arc::new(MockEmbeddingProvider::new(4).with_fail_error(
        MemcoreError::ProviderError("OpenAI API error (500): internal".to_string()),
    ));
    let healthy = Arc::new(MockEmbeddingProvider::new(4));
    let usage = provider_usage_recorder(true);
    let embedding = build_resilient_embedding_from_candidates(
        vec![
            embedding_candidate("primary", failing),
            embedding_candidate("secondary", healthy),
        ],
        test_circuit_breaker(),
        fast_policy(),
        true,
        Some(ProviderRoutingMetrics::new()),
        Some(usage.clone()),
        None,
        false,
    )
    .expect("embedding provider");

    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        embedding,
    );

    let _ = engine
        .search_memory(SearchMemoryInput {
            tenant: tenant(),
            query: "hiking".to_string(),
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search should succeed via fallback embedding");

    assert!(usage.snapshot().total_requests >= 1);
}
