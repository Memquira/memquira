use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_core::{
    BuildContextInput, BuildContextOutput, ContextBudgetUsage, ContextCache, ContextCacheConfig,
    ContextCacheUsage, ContextCompressionUsage, InMemoryContextCache, MemoryEngine, TenantContext,
    build_context_cache_key, cached_entry_with_ttl,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};

fn swr_config() -> ContextCacheConfig {
    ContextCacheConfig {
        enabled: true,
        ttl_seconds: 300,
        max_entries: 100,
        stale_while_revalidate_enabled: true,
        stale_ttl_seconds: 120,
        ..Default::default()
    }
}

fn engine_with_cache(cache: Arc<InMemoryContextCache>) -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_context_cache(cache, swr_config())
}

fn context_input(query: &str) -> BuildContextInput {
    BuildContextInput {
        tenant: TenantContext::new("org_a", "user_a").expect("tenant"),
        query: query.to_string(),
        ..Default::default()
    }
}

fn sample_output(context: &str) -> BuildContextOutput {
    BuildContextOutput {
        context: context.to_string(),
        memories: Vec::new(),
        budget: ContextBudgetUsage {
            max_tokens: 2000,
            reserved_tokens: 300,
            available_tokens: 1700,
            used_tokens: 10,
            included_memories: 0,
            skipped_memories: 0,
        },
        compression: ContextCompressionUsage::disabled(),
        cache: ContextCacheUsage::disabled(),
    }
}

fn stale_entry(context: &str) -> memcore_core::CachedContextEntry {
    let mut entry = cached_entry_with_ttl(&sample_output(context), 300);
    let now = Utc::now();
    entry.expires_at = now - Duration::seconds(5);
    entry.stale_until = Some(now + Duration::seconds(60));
    entry
}

#[test]
fn swr_disabled_by_default_in_config() {
    let config = ContextCacheConfig::default();
    assert!(!config.stale_while_revalidate_enabled);
}

#[tokio::test]
async fn engine_serves_stale_context_when_swr_enabled() {
    let cache = Arc::new(InMemoryContextCache::new(100));
    let input = context_input("swr engine query");
    let key = build_context_cache_key(&input);
    cache
        .set(key, stale_entry("stale engine context"))
        .await
        .expect("seed");

    let engine = engine_with_cache(cache);
    let output = engine.build_context(input).await.expect("stale context");
    assert_eq!(output.context, "stale engine context");
    assert!(output.cache.served_stale);
    assert!(output.cache.refresh_started);
}

#[tokio::test]
async fn refresh_stale_context_updates_cache_entry() {
    let cache = Arc::new(InMemoryContextCache::new(100));
    let input = context_input("refresh flow");
    let key = build_context_cache_key(&input);
    cache
        .set(key.clone(), stale_entry("before refresh"))
        .await
        .expect("seed");

    let engine = engine_with_cache(cache.clone());
    engine.refresh_stale_context(input).await.expect("refresh");

    let fresh = cache.get(&key).await.expect("get").expect("fresh");
    assert_ne!(fresh.context, "before refresh");
    assert!(fresh.is_fresh(Utc::now()));
}
