#![cfg(feature = "redis-cache")]

use std::env;

use chrono::{Duration, Utc};
use memcore_core::{
    BuildContextInput, BuildContextOutput, ContextBudgetUsage, ContextCache,
    ContextCompressionUsage, TenantContext, build_context_cache_key, cached_entry_with_ttl,
};
use memcore_storage::RedisContextCache;

fn test_redis_url() -> Option<String> {
    env::var("MEMCORE_TEST_REDIS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn sample_input(org_id: &str, user_id: &str, query: &str) -> BuildContextInput {
    BuildContextInput {
        tenant: TenantContext::new(org_id, user_id).expect("tenant"),
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
        cache: memcore_core::ContextCacheUsage::disabled(),
    }
}

#[tokio::test]
async fn redis_context_cache_set_and_get_works() {
    let Some(redis_url) = test_redis_url() else {
        eprintln!("skipping redis integration test: MEMCORE_TEST_REDIS_URL not set");
        return;
    };

    let cache = RedisContextCache::connect(&redis_url, "memcore_test", 60)
        .await
        .expect("connect redis");
    let key = build_context_cache_key(&sample_input("org_a", "user_a", "redis get set"));
    let entry = cached_entry_with_ttl(&sample_output("cached via redis"), 60);

    cache.set(key.clone(), entry).await.expect("set");
    let loaded = cache.get(&key).await.expect("get").expect("cache hit");
    assert_eq!(loaded.context, "cached via redis");

    let _ = cache.invalidate_user("org_a", "user_a").await;
}

#[tokio::test]
async fn redis_context_cache_ttl_expiration_works() {
    let Some(redis_url) = test_redis_url() else {
        eprintln!("skipping redis integration test: MEMCORE_TEST_REDIS_URL not set");
        return;
    };

    let cache = RedisContextCache::connect(&redis_url, "memcore_test", 1)
        .await
        .expect("connect redis");
    let key = build_context_cache_key(&sample_input("org_a", "user_a", "redis ttl"));
    let mut entry = cached_entry_with_ttl(&sample_output("ttl entry"), 1);
    entry.expires_at = Utc::now() + Duration::seconds(1);

    cache.set(key.clone(), entry).await.expect("set");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(cache.get(&key).await.expect("get").is_none());

    let _ = cache.invalidate_user("org_a", "user_a").await;
}

#[tokio::test]
async fn redis_invalidate_user_removes_only_matching_user_keys() {
    let Some(redis_url) = test_redis_url() else {
        eprintln!("skipping redis integration test: MEMCORE_TEST_REDIS_URL not set");
        return;
    };

    let cache = RedisContextCache::connect(&redis_url, "memcore_test", 120)
        .await
        .expect("connect redis");

    let key_a = build_context_cache_key(&sample_input("org_a", "user_a", "invalidate a"));
    let key_b = build_context_cache_key(&sample_input("org_b", "user_a", "invalidate b"));

    cache
        .set(
            key_a.clone(),
            cached_entry_with_ttl(&sample_output("a"), 120),
        )
        .await
        .expect("set a");
    cache
        .set(
            key_b.clone(),
            cached_entry_with_ttl(&sample_output("b"), 120),
        )
        .await
        .expect("set b");

    let removed = cache
        .invalidate_user("org_a", "user_a")
        .await
        .expect("invalidate");
    assert_eq!(removed, 1);
    assert!(cache.get(&key_a).await.expect("get a").is_none());
    assert!(cache.get(&key_b).await.expect("get b").is_some());

    let _ = cache.invalidate_user("org_b", "user_a").await;
}

#[tokio::test]
async fn redis_invalidate_user_handles_expired_indexed_keys() {
    let Some(redis_url) = test_redis_url() else {
        eprintln!("skipping redis integration test: MEMCORE_TEST_REDIS_URL not set");
        return;
    };

    let cache = RedisContextCache::connect(&redis_url, "memcore_test", 1)
        .await
        .expect("connect redis");
    let key = build_context_cache_key(&sample_input("org_a", "user_a", "expired index"));
    cache
        .set(
            key,
            cached_entry_with_ttl(&sample_output("stale indexed"), 1),
        )
        .await
        .expect("set");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    cache
        .invalidate_user("org_a", "user_a")
        .await
        .expect("invalidate should succeed");
}
