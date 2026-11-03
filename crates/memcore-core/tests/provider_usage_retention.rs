use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use memcore_core::{
    ApplyProviderUsageRetentionInput, MemoryEngine, ProviderCallStatus, ProviderUsageCapability,
    ProviderUsageEventRecord, ProviderUsageQuery, ProviderUsageStore,
};
use memcore_storage::{MockFactStore, MockProviderUsageStore, MockVectorStore};
use uuid::Uuid;

fn sample_event(org_id: &str, created_at: chrono::DateTime<Utc>) -> ProviderUsageEventRecord {
    ProviderUsageEventRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        user_id: Some("user_a".to_string()),
        provider_name: "mock".to_string(),
        model_name: Some("mock-llm".to_string()),
        capability: ProviderUsageCapability::Llm,
        operation_name: "llm_extract_facts".to_string(),
        status: ProviderCallStatus::Success,
        input_tokens: Some(10),
        output_tokens: Some(2),
        total_tokens: Some(12),
        retry_count: 0,
        fallback_used: false,
        circuit_blocked: false,
        timed_out: false,
        estimated_cost_usd: None,
        metadata: None,
        created_at,
    }
}

fn engine_with_store(store: Arc<dyn ProviderUsageStore>) -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(memcore_providers::MockLlmProvider::new()),
        Arc::new(memcore_providers::MockEmbeddingProvider::new(8)),
    )
    .with_provider_usage_store(Some(store))
}

#[tokio::test]
async fn retention_days_zero_returns_zero_counts() {
    let store = Arc::new(MockProviderUsageStore::new());
    let engine = engine_with_store(store.clone());
    let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    store
        .record_usage_event(sample_event("org_zero", old))
        .await
        .expect("record");

    let output = engine
        .apply_provider_usage_retention(ApplyProviderUsageRetentionInput {
            org_id: "org_zero".to_string(),
            retention_days: 0,
            dry_run: false,
        })
        .await
        .expect("apply");

    assert_eq!(output.matched_events, 0);
    assert_eq!(output.deleted_events, 0);
    let remaining = store
        .query_usage(ProviderUsageQuery::new("org_zero", 10))
        .await
        .expect("query");
    assert_eq!(remaining.events.len(), 1);
}

#[tokio::test]
async fn dry_run_returns_matched_without_deleting() {
    let store = Arc::new(MockProviderUsageStore::new());
    let engine = engine_with_store(store.clone());
    let now = Utc::now();
    let old = now - Duration::days(60);
    let recent = now - Duration::days(1);

    store
        .record_usage_event(sample_event("org_dry", old))
        .await
        .expect("record");
    store
        .record_usage_event(sample_event("org_dry", recent))
        .await
        .expect("record");

    let output = engine
        .apply_provider_usage_retention(ApplyProviderUsageRetentionInput {
            org_id: "org_dry".to_string(),
            retention_days: 30,
            dry_run: true,
        })
        .await
        .expect("apply");

    assert!(output.matched_events >= 1);
    assert_eq!(output.deleted_events, 0);
    let remaining = store
        .query_usage(ProviderUsageQuery::new("org_dry", 10))
        .await
        .expect("query");
    assert_eq!(remaining.events.len(), 2);
}

#[tokio::test]
async fn non_dry_run_deletes_matched_events() {
    let store = Arc::new(MockProviderUsageStore::new());
    let engine = engine_with_store(store.clone());
    let now = Utc::now();
    let old = now - Duration::days(60);
    let recent = now - Duration::days(1);

    store
        .record_usage_event(sample_event("org_apply", old))
        .await
        .expect("record");
    store
        .record_usage_event(sample_event("org_apply", recent))
        .await
        .expect("record");

    let output = engine
        .apply_provider_usage_retention(ApplyProviderUsageRetentionInput {
            org_id: "org_apply".to_string(),
            retention_days: 30,
            dry_run: false,
        })
        .await
        .expect("apply");

    assert!(output.matched_events >= 1);
    assert_eq!(output.deleted_events, output.matched_events);
    let remaining = store
        .query_usage(ProviderUsageQuery::new("org_apply", 10))
        .await
        .expect("query");
    assert_eq!(remaining.events.len(), 1);
}

#[tokio::test]
async fn org_isolation_is_preserved() {
    let store = Arc::new(MockProviderUsageStore::new());
    let engine = engine_with_store(store.clone());
    let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();

    store
        .record_usage_event(sample_event("org_a", old))
        .await
        .expect("record");
    store
        .record_usage_event(sample_event("org_b", old))
        .await
        .expect("record");

    let output = engine
        .apply_provider_usage_retention(ApplyProviderUsageRetentionInput {
            org_id: "org_a".to_string(),
            retention_days: 30,
            dry_run: false,
        })
        .await
        .expect("apply");

    assert_eq!(output.deleted_events, 1);
    let org_b = store
        .query_usage(ProviderUsageQuery::new("org_b", 10))
        .await
        .expect("org_b");
    assert_eq!(org_b.events.len(), 1);
}

#[tokio::test]
async fn cutoff_is_computed_from_retention_days() {
    let store = Arc::new(MockProviderUsageStore::new());
    let engine = engine_with_store(store.clone());
    let before_cutoff = Utc::now() - chrono::Duration::days(10);
    let after_cutoff = Utc::now() - chrono::Duration::days(1);

    store
        .record_usage_event(sample_event("org_cut", before_cutoff))
        .await
        .expect("record");
    store
        .record_usage_event(sample_event("org_cut", after_cutoff))
        .await
        .expect("record");

    let output = engine
        .apply_provider_usage_retention(ApplyProviderUsageRetentionInput {
            org_id: "org_cut".to_string(),
            retention_days: 5,
            dry_run: true,
        })
        .await
        .expect("apply");

    let expected_cutoff = Utc::now() - chrono::Duration::days(5);
    let delta = (output.cutoff - expected_cutoff)
        .num_seconds()
        .unsigned_abs();
    assert!(
        delta < 5,
        "cutoff should be approximately now - retention_days"
    );
    assert_eq!(output.matched_events, 1);
}
