use std::sync::Arc;

use memcore_core::{
    BuildContextInput, ContextCacheConfig, InMemoryContextCache, MemoryEngine, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};

fn engine_with_stampede_cache() -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_context_cache(
        Arc::new(InMemoryContextCache::new(100)),
        ContextCacheConfig {
            enabled: true,
            ttl_seconds: 300,
            max_entries: 100,
            ..Default::default()
        },
    )
}

fn context_input(query: &str) -> BuildContextInput {
    BuildContextInput {
        tenant: TenantContext::new("org_a", "user_a").expect("tenant"),
        query: query.to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn concurrent_identical_context_requests_share_single_computation() {
    let engine = Arc::new(engine_with_stampede_cache());
    let input = context_input("stampede flow query");

    let engine_a = engine.clone();
    let input_a = input.clone();
    let (first, second) = tokio::join!(
        tokio::spawn(async move { engine_a.build_context(input_a).await }),
        tokio::spawn(async move { engine.build_context(input).await }),
    );

    let first_output = first.expect("join first").expect("first context");
    let second_output = second.expect("join second").expect("second context");

    assert_eq!(first_output.context, second_output.context);
    assert!(first_output.cache.stampede_protection_enabled);
    assert!(second_output.cache.stampede_protection_enabled);
    assert!(
        first_output.cache.hit || second_output.cache.hit,
        "at least one concurrent request should observe a cache hit"
    );
}

#[tokio::test]
async fn cache_disabled_preserves_existing_behavior_without_stampede_metadata() {
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );

    let output = engine
        .build_context(context_input("disabled"))
        .await
        .expect("context");

    assert!(!output.cache.enabled);
    assert!(!output.cache.stampede_protection_enabled);
}

#[tokio::test]
async fn enabled_cache_reports_stampede_metadata_on_miss_and_hit() {
    let engine = engine_with_stampede_cache();
    let input = context_input("metadata flow");

    let miss = engine.build_context(input.clone()).await.expect("miss");
    assert!(!miss.cache.hit);
    assert!(miss.cache.stampede_protection_enabled);
    assert!(!miss.cache.waited_for_inflight);

    let hit = engine.build_context(input).await.expect("hit");
    assert!(hit.cache.hit);
    assert!(hit.cache.stampede_protection_enabled);
    assert!(!hit.cache.waited_for_inflight);
}
