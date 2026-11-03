use std::sync::Arc;

use memcore_common::MemcoreError;
use memcore_core::{AddMemoryInput, MemoryEngine, MemoryMessage, MessageRole, TenantContext};
use memcore_providers::{
    CircuitBreakerConfig, InMemoryProviderUsageRecorder, MockLlmProvider, ProviderCandidate,
    ProviderCapability, ProviderCircuitBreaker, ProviderExecutionPolicy, ProviderId,
    ProviderUsageRecorder, build_resilient_llm_from_candidates, new_token_usage_slot,
    provider_usage_recorder,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

fn tenant() -> TenantContext {
    TenantContext::new("org_usage", "user_usage").expect("tenant")
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

#[tokio::test]
async fn successful_mock_llm_call_records_usage() {
    let usage = InMemoryProviderUsageRecorder::new();
    let llm = build_resilient_llm_from_candidates(
        vec![ProviderCandidate::new(
            ProviderId::new("mock", ProviderCapability::Llm),
            Arc::new(MockLlmProvider::new()),
            Some("mock-llm".to_string()),
            Some(new_token_usage_slot()),
        )],
        vec![],
        Arc::new(ProviderCircuitBreaker::new(
            CircuitBreakerConfig::for_tests(),
        )),
        fast_policy(),
        false,
        None,
        Some(usage.clone()),
        None,
        false,
    );

    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm,
        Arc::new(memcore_providers::MockEmbeddingProvider::new(4)),
    );

    let _ = engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "User likes Rust.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add");

    let snapshot = usage.snapshot();
    assert!(snapshot.total_successes >= 1);
    assert!(
        snapshot
            .records
            .iter()
            .any(|r| r.input_tokens.unwrap_or(0) > 0)
    );
}

#[tokio::test]
async fn failed_mock_llm_call_records_error_without_prompt_content() {
    let usage = provider_usage_recorder(true);
    let llm = build_resilient_llm_from_candidates(
        vec![ProviderCandidate::new(
            ProviderId::new("mock", ProviderCapability::Llm),
            Arc::new(
                MockLlmProvider::new().with_fail_error(MemcoreError::ProviderError(
                    "OpenAI API error (500): internal".to_string(),
                )),
            ),
            Some("mock-llm".to_string()),
            None,
        )],
        vec![],
        Arc::new(ProviderCircuitBreaker::new(
            CircuitBreakerConfig::for_tests(),
        )),
        fast_policy(),
        false,
        None,
        Some(usage.clone()),
        None,
        false,
    );

    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm,
        Arc::new(memcore_providers::MockEmbeddingProvider::new(4)),
    );

    let _ = engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "secret prompt content".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("should fail");

    let snapshot = usage.snapshot();
    assert!(snapshot.total_errors >= 1);
    let json = serde_json::to_string(&snapshot).expect("serialize");
    assert!(!json.contains("secret prompt content"));
    assert!(!json.contains("OPENAI_API_KEY"));
}
