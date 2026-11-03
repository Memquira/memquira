use std::sync::Arc;

use memcore_core::{
    AddMemoryInput, ApplyRetentionInput, BuildContextInput, ContextCacheConfig, ContextFormat,
    ImportMode, ImportUserDataInput, InMemoryContextCache, MemoryEngine, MemoryMessage,
    MessageRole, RetentionPolicy, TenantContext, UserMemoryExport,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with_cache() -> MemoryEngine {
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

fn context_input(org_id: &str, user_id: &str, query: &str) -> BuildContextInput {
    BuildContextInput {
        tenant: tenant(org_id, user_id),
        query: query.to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn cache_disabled_preserves_existing_behavior() {
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );

    let output = engine
        .build_context(context_input("org_a", "user_a", "hello"))
        .await
        .expect("context");

    assert!(!output.cache.enabled);
    assert!(!output.cache.hit);
}

#[tokio::test]
async fn identical_requests_hit_cache_on_second_call() {
    let engine = engine_with_cache();
    let input = context_input("org_a", "user_a", "cache flow query");

    let first = engine.build_context(input.clone()).await.expect("first");
    assert!(!first.cache.hit);

    let second = engine.build_context(input).await.expect("second");
    assert!(second.cache.hit);
    assert_eq!(second.context, first.context);
}

#[tokio::test]
async fn different_query_is_cache_miss() {
    let engine = engine_with_cache();

    let first = engine
        .build_context(context_input("org_a", "user_a", "query alpha"))
        .await
        .expect("first");
    let second = engine
        .build_context(context_input("org_a", "user_a", "query beta"))
        .await
        .expect("second");

    assert!(!first.cache.hit);
    assert!(!second.cache.hit);
}

#[tokio::test]
async fn different_format_options_are_cache_miss() {
    let engine = engine_with_cache();
    let mut markdown = context_input("org_a", "user_a", "format cache");
    markdown.format_options.format = ContextFormat::Markdown;

    let _ = engine.build_context(markdown.clone()).await.expect("first");
    let second = engine.build_context(markdown).await.expect("second");
    assert!(second.cache.hit);

    let plain = engine
        .build_context(context_input("org_a", "user_a", "format cache"))
        .await
        .expect("plain");
    assert!(!plain.cache.hit);
}

#[tokio::test]
async fn another_user_or_org_cannot_hit_cached_entry() {
    let engine = engine_with_cache();
    let input = context_input("org_a", "user_a", "tenant isolation");

    let _ = engine.build_context(input).await.expect("seed");
    let other_user = engine
        .build_context(context_input("org_a", "user_b", "tenant isolation"))
        .await
        .expect("other user");
    let other_org = engine
        .build_context(context_input("org_b", "user_a", "tenant isolation"))
        .await
        .expect("other org");

    assert!(!other_user.cache.hit);
    assert!(!other_org.cache.hit);
}

#[tokio::test]
async fn memory_write_invalidates_user_context_cache() {
    let engine = Arc::new(engine_with_cache());
    let tenant = tenant("org_a", "user_a");
    let input = context_input("org_a", "user_a", "invalidate on write");

    let _ = engine.build_context(input.clone()).await.expect("seed");
    let hit = engine.build_context(input.clone()).await.expect("hit");
    assert!(hit.cache.hit);

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "User is building memcore cache tests.".to_string(),
                ..Default::default()
            }],
            metadata: serde_json::json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    let miss = engine.build_context(input).await.expect("miss after write");
    assert!(!miss.cache.hit);
}

#[tokio::test]
async fn dry_run_import_does_not_invalidate_cache() {
    let engine = Arc::new(engine_with_cache());
    let tenant = tenant("org_a", "user_a");
    let input = context_input("org_a", "user_a", "import dry run");

    let _ = engine.build_context(input.clone()).await.expect("seed");

    engine
        .import_user_data(ImportUserDataInput {
            tenant: tenant.clone(),
            export: UserMemoryExport {
                format_version: memcore_core::USER_EXPORT_FORMAT_VERSION.to_string(),
                exported_at: chrono::Utc::now(),
                org_id: tenant.org_id.clone(),
                user_id: tenant.user_id.clone(),
                facts: vec![],
                memory_events: vec![],
                episodes: None,
                entities: None,
                truncated: None,
                completeness: None,
            },
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: true,
        })
        .await
        .expect("dry run import");

    let hit = engine.build_context(input).await.expect("still cached");
    assert!(hit.cache.hit);
}

#[tokio::test]
async fn retention_dry_run_does_not_invalidate_cache() {
    let engine = Arc::new(engine_with_cache());
    let tenant = tenant("org_a", "user_a");
    let input = context_input("org_a", "user_a", "retention dry run");

    let _ = engine.build_context(input.clone()).await.expect("seed");

    engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(30),
                event_retention_days: None,
            },
            dry_run: true,
        })
        .await
        .expect("retention dry run");

    let hit = engine.build_context(input).await.expect("still cached");
    assert!(hit.cache.hit);
}
