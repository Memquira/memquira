use std::sync::Arc;

use chrono::{TimeZone, Utc};
use memcore_core::{
    CreateMemoryUsageSnapshotInput, Fact, FactStore, GetOrgQuotaStatusInput, MemoryEngine,
    MemorySource, MemoryType, OrgPlanConfig, OrgPlanLimits, OrgPlanStore, OrgPlanTier,
    OrgQuotaLimits, OrgUsageDashboardInput, ProviderCallStatus, ProviderUsageCapability,
    ProviderUsageDailyInput, ProviderUsageEventRecord, ProviderUsageStore,
    QueryMemoryUsageSnapshotsInput, QuotaLimitSource, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{
    MockFactStore, MockMemoryUsageSnapshotStore, MockOrgPlanStore, MockProviderUsageStore,
    MockVectorStore,
};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn limits(enabled: bool, max_org_memories: u64) -> OrgQuotaLimits {
    OrgQuotaLimits::from_raw(enabled, 0, 0, max_org_memories, 0, 0)
}

fn engine(
    fact_store: Arc<MockFactStore>,
    usage_store: Option<Arc<dyn ProviderUsageStore>>,
    plan_store: Arc<MockOrgPlanStore>,
    quota_limits: OrgQuotaLimits,
) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_provider_usage_store(usage_store)
    .with_org_plan_store(plan_store)
    .with_memory_usage_snapshot_store(Some(Arc::new(MockMemoryUsageSnapshotStore::new())))
    .with_global_quota_limits(quota_limits)
}

async fn insert_fact(store: &MockFactStore, tenant: &TenantContext, content: &str) -> Fact {
    let now = Utc::now();
    let fact = Fact::new(
        Uuid::new_v4(),
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        MemoryType::Profile,
        content,
        None,
        MemorySource::ApiImport,
        0.9,
        0.8,
        None,
        None,
        now,
        now,
        json!({}),
    )
    .expect("fact");
    store
        .insert_fact(tenant, fact.clone())
        .await
        .expect("insert");
    fact
}

fn plan_config(org_id: &str, is_active: bool) -> OrgPlanConfig {
    let now = Utc::now();
    OrgPlanConfig {
        org_id: org_id.to_string(),
        tier: OrgPlanTier::Pro,
        limits: OrgPlanLimits {
            max_users_per_org: Some(10),
            max_memories_per_user: Some(10),
            max_memories_per_org: Some(10),
            daily_provider_request_limit: Some(100),
            daily_provider_token_limit: Some(1000),
        },
        is_active,
        metadata: None,
        created_at: now,
        updated_at: now,
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_usage(
    store: &MockProviderUsageStore,
    org_id: &str,
    provider_name: &str,
    model_name: &str,
    capability: ProviderUsageCapability,
    status: ProviderCallStatus,
    created_at: chrono::DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
    cost: Option<f64>,
) {
    store
        .record_usage_event(ProviderUsageEventRecord {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            user_id: Some("user_usage".to_string()),
            provider_name: provider_name.to_string(),
            model_name: Some(model_name.to_string()),
            capability,
            operation_name: "llm_extract_facts".to_string(),
            status,
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            total_tokens: Some(input_tokens + output_tokens),
            retry_count: 1,
            fallback_used: status == ProviderCallStatus::Error,
            circuit_blocked: false,
            timed_out: status == ProviderCallStatus::Error,
            estimated_cost_usd: cost,
            metadata: Some(json!({"safe": true})),
            created_at,
        })
        .await
        .expect("record usage");
}

#[tokio::test]
async fn dashboard_combines_plan_quota_memory_and_provider_summary() {
    let fact_store = Arc::new(MockFactStore::new());
    let usage_store = Arc::new(MockProviderUsageStore::new());
    let plan_store = Arc::new(MockOrgPlanStore::new());
    let org_id = "org_dashboard";
    let start = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();

    insert_fact(&fact_store, &tenant(org_id, "user_a"), "secret memory").await;
    insert_fact(&fact_store, &tenant(org_id, "user_b"), "another secret").await;
    insert_fact(&fact_store, &tenant("org_other", "user_x"), "other memory").await;
    plan_store
        .upsert_org_plan(plan_config(org_id, true))
        .await
        .expect("plan");
    record_usage(
        &usage_store,
        org_id,
        "mock",
        "mock-llm",
        ProviderUsageCapability::Llm,
        ProviderCallStatus::Success,
        start,
        100,
        20,
        Some(0.001),
    )
    .await;
    record_usage(
        &usage_store,
        org_id,
        "mock",
        "mock-llm",
        ProviderUsageCapability::Llm,
        ProviderCallStatus::Error,
        start + chrono::Duration::days(1),
        50,
        5,
        Some(0.002),
    )
    .await;
    record_usage(
        &usage_store,
        "org_other",
        "mock",
        "mock-llm",
        ProviderUsageCapability::Llm,
        ProviderCallStatus::Success,
        start,
        999,
        999,
        Some(9.99),
    )
    .await;

    let output = engine(fact_store, Some(usage_store), plan_store, limits(true, 100))
        .get_org_usage_dashboard(OrgUsageDashboardInput {
            org_id: org_id.to_string(),
            created_after: start,
            created_before: end,
        })
        .await
        .expect("dashboard");

    assert_eq!(output.org_id, org_id);
    assert_eq!(output.plan.as_ref().expect("plan").tier, OrgPlanTier::Pro);
    assert_eq!(output.quota.source, QuotaLimitSource::OrgPlan);
    assert!(output.quota.allowed);
    assert_eq!(output.memory.total_users, 2);
    assert_eq!(output.memory.total_memories, 2);
    assert_eq!(output.memory.active_memories, 2);
    assert_eq!(output.memory.deleted_memories, None);
    assert_eq!(output.provider.total_requests, 2);
    assert_eq!(output.provider.total_successes, 1);
    assert_eq!(output.provider.total_errors, 1);
    assert_eq!(output.provider.total_tokens, 175);
    assert_eq!(output.provider.total_estimated_cost_usd, Some(0.003));

    let body = serde_json::to_string(&output).expect("json");
    assert!(!body.contains("secret memory"));
    assert!(!body.contains("another secret"));
}

#[tokio::test]
async fn create_and_query_memory_usage_snapshots_are_org_scoped() {
    let fact_store = Arc::new(MockFactStore::new());
    let plan_store = Arc::new(MockOrgPlanStore::new());
    let org_id = "org_snapshots";
    let captured_at = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();

    insert_fact(&fact_store, &tenant(org_id, "user_a"), "private memory").await;
    insert_fact(
        &fact_store,
        &tenant(org_id, "user_b"),
        "another private memory",
    )
    .await;
    insert_fact(
        &fact_store,
        &tenant("org_other", "user_x"),
        "other private memory",
    )
    .await;

    let engine = engine(fact_store, None, plan_store, limits(false, 0));
    let created = engine
        .create_memory_usage_snapshot(CreateMemoryUsageSnapshotInput {
            org_id: org_id.to_string(),
            captured_at: Some(captured_at),
        })
        .await
        .expect("create snapshot");

    assert_eq!(created.snapshot.org_id, org_id);
    assert_eq!(created.snapshot.total_users, 2);
    assert_eq!(created.snapshot.total_memories, 2);
    assert_eq!(created.snapshot.active_memories, 2);
    assert_eq!(created.snapshot.deleted_memories, None);
    assert_eq!(created.snapshot.metadata, None);

    engine
        .create_memory_usage_snapshot(CreateMemoryUsageSnapshotInput {
            org_id: "org_other".to_string(),
            captured_at: Some(captured_at + chrono::Duration::days(1)),
        })
        .await
        .expect("other snapshot");

    let output = engine
        .query_memory_usage_snapshots(QueryMemoryUsageSnapshotsInput {
            org_id: org_id.to_string(),
            created_after: Some(captured_at - chrono::Duration::seconds(1)),
            created_before: Some(captured_at + chrono::Duration::seconds(1)),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("query snapshots");

    assert_eq!(output.snapshots.len(), 1);
    assert_eq!(output.snapshots[0].id, created.snapshot.id);

    let body = serde_json::to_string(&output).expect("json");
    assert!(!body.contains("private memory"));
}

#[tokio::test]
async fn dashboard_includes_latest_memory_usage_snapshot() {
    let fact_store = Arc::new(MockFactStore::new());
    let plan_store = Arc::new(MockOrgPlanStore::new());
    let org_id = "org_latest_snapshot";
    let older = Utc.with_ymd_and_hms(2026, 6, 17, 10, 0, 0).unwrap();
    let newer = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();

    insert_fact(&fact_store, &tenant(org_id, "user_a"), "private memory").await;

    let engine = engine(fact_store, None, plan_store, limits(false, 0));
    engine
        .create_memory_usage_snapshot(CreateMemoryUsageSnapshotInput {
            org_id: org_id.to_string(),
            captured_at: Some(older),
        })
        .await
        .expect("older snapshot");
    let latest = engine
        .create_memory_usage_snapshot(CreateMemoryUsageSnapshotInput {
            org_id: org_id.to_string(),
            captured_at: Some(newer),
        })
        .await
        .expect("newer snapshot");

    let output = engine
        .get_org_usage_dashboard(OrgUsageDashboardInput {
            org_id: org_id.to_string(),
            created_after: older - chrono::Duration::days(1),
            created_before: newer + chrono::Duration::days(1),
        })
        .await
        .expect("dashboard");

    let latest_snapshot = output.memory.latest_snapshot.expect("latest snapshot");
    assert_eq!(latest_snapshot.captured_at, latest.snapshot.captured_at);
    assert_eq!(latest_snapshot.total_memories, 1);
    assert_eq!(latest_snapshot.active_memories, 1);
}

#[tokio::test]
async fn dashboard_uses_global_quota_source_when_no_plan_exists() {
    let output = engine(
        Arc::new(MockFactStore::new()),
        None,
        Arc::new(MockOrgPlanStore::new()),
        limits(true, 10),
    )
    .get_org_usage_dashboard(OrgUsageDashboardInput {
        org_id: "org_global_dashboard".to_string(),
        created_after: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        created_before: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
    })
    .await
    .expect("dashboard");

    assert!(output.plan.is_none());
    assert_eq!(output.quota.source, QuotaLimitSource::GlobalConfig);
    assert_eq!(output.provider.total_requests, 0);
}

#[tokio::test]
async fn dashboard_rejects_invalid_range() {
    let error = engine(
        Arc::new(MockFactStore::new()),
        None,
        Arc::new(MockOrgPlanStore::new()),
        limits(false, 0),
    )
    .get_org_usage_dashboard(OrgUsageDashboardInput {
        org_id: "org_bad_range".to_string(),
        created_after: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
        created_before: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
    })
    .await
    .expect_err("invalid range");

    assert!(matches!(
        error,
        memcore_common::MemcoreError::ValidationError(_)
    ));
}

#[tokio::test]
async fn daily_buckets_aggregate_by_utc_date_and_filters_preserve_org_isolation() {
    let usage_store = Arc::new(MockProviderUsageStore::new());
    let start = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 6, 4, 0, 0, 0).unwrap();

    record_usage(
        &usage_store,
        "org_daily",
        "mock",
        "model-a",
        ProviderUsageCapability::Llm,
        ProviderCallStatus::Success,
        start + chrono::Duration::hours(1),
        10,
        2,
        Some(0.001),
    )
    .await;
    record_usage(
        &usage_store,
        "org_daily",
        "mock",
        "model-a",
        ProviderUsageCapability::Llm,
        ProviderCallStatus::Error,
        start + chrono::Duration::hours(23),
        20,
        3,
        Some(0.002),
    )
    .await;
    record_usage(
        &usage_store,
        "org_daily",
        "other",
        "model-b",
        ProviderUsageCapability::Embedding,
        ProviderCallStatus::Success,
        start + chrono::Duration::days(1),
        100,
        0,
        None,
    )
    .await;
    record_usage(
        &usage_store,
        "org_other",
        "mock",
        "model-a",
        ProviderUsageCapability::Llm,
        ProviderCallStatus::Success,
        start,
        999,
        999,
        Some(9.99),
    )
    .await;

    let engine = engine(
        Arc::new(MockFactStore::new()),
        Some(usage_store),
        Arc::new(MockOrgPlanStore::new()),
        limits(false, 0),
    );

    let all = engine
        .get_provider_usage_daily(ProviderUsageDailyInput {
            org_id: "org_daily".to_string(),
            created_after: start,
            created_before: end,
            provider_name: None,
            model_name: None,
            capability: None,
        })
        .await
        .expect("daily");
    assert_eq!(all.buckets.len(), 2);
    assert_eq!(all.buckets[0].date, start.date_naive());
    assert_eq!(all.buckets[0].total_requests, 2);
    assert_eq!(all.buckets[0].total_successes, 1);
    assert_eq!(all.buckets[0].total_errors, 1);
    assert_eq!(all.buckets[0].total_input_tokens, 30);
    assert_eq!(all.buckets[0].total_output_tokens, 5);
    assert_eq!(all.buckets[0].total_tokens, 35);
    assert_eq!(all.buckets[0].total_estimated_cost_usd, Some(0.003));

    let provider_filtered = engine
        .get_provider_usage_daily(ProviderUsageDailyInput {
            org_id: "org_daily".to_string(),
            created_after: start,
            created_before: end,
            provider_name: Some("other".to_string()),
            model_name: None,
            capability: None,
        })
        .await
        .expect("provider filter");
    assert_eq!(provider_filtered.buckets.len(), 1);
    assert_eq!(provider_filtered.buckets[0].total_requests, 1);

    let model_filtered = engine
        .get_provider_usage_daily(ProviderUsageDailyInput {
            org_id: "org_daily".to_string(),
            created_after: start,
            created_before: end,
            provider_name: None,
            model_name: Some("model-a".to_string()),
            capability: None,
        })
        .await
        .expect("model filter");
    assert_eq!(model_filtered.buckets.len(), 1);
    assert_eq!(model_filtered.buckets[0].total_requests, 2);

    let capability_filtered = engine
        .get_provider_usage_daily(ProviderUsageDailyInput {
            org_id: "org_daily".to_string(),
            created_after: start,
            created_before: end,
            provider_name: None,
            model_name: None,
            capability: Some(ProviderUsageCapability::Embedding),
        })
        .await
        .expect("capability filter");
    assert_eq!(capability_filtered.buckets.len(), 1);
    assert_eq!(capability_filtered.buckets[0].total_requests, 1);

    let other_org = engine
        .get_provider_usage_daily(ProviderUsageDailyInput {
            org_id: "org_other".to_string(),
            created_after: start,
            created_before: end,
            provider_name: None,
            model_name: None,
            capability: None,
        })
        .await
        .expect("other org");
    assert_eq!(other_org.buckets.len(), 1);
    assert_eq!(other_org.buckets[0].total_tokens, 1998);
}

#[tokio::test]
async fn quota_status_still_available_for_dashboard_inputs() {
    let engine = engine(
        Arc::new(MockFactStore::new()),
        None,
        Arc::new(MockOrgPlanStore::new()),
        limits(false, 0),
    );
    let quota = engine
        .get_org_quota_status(GetOrgQuotaStatusInput {
            org_id: "org_quota_dashboard".to_string(),
            user_id: None,
        })
        .await
        .expect("quota");
    assert!(quota.allowed);
}

/// `total_memories` counts stored rows (what quotas count, invalidated and superseded facts
/// included); `active_memories` counts active rows only.
#[tokio::test]
async fn stored_and_active_memory_counts_are_distinct() {
    let fact_store = Arc::new(MockFactStore::new());
    let plan_store = Arc::new(MockOrgPlanStore::new());
    let org_id = "org_counts";
    let user = tenant(org_id, "user_a");
    insert_fact(&fact_store, &user, "current memory").await;
    let mut invalidated = insert_fact(&fact_store, &user, "historical memory").await;
    invalidated.invalid_at = Some(Utc::now());
    invalidated.superseded_at = Some(Utc::now());
    fact_store
        .update_fact(&user, invalidated)
        .await
        .expect("update");
    let mut superseded = insert_fact(&fact_store, &user, "superseded memory").await;
    superseded.superseded_at = Some(Utc::now());
    fact_store
        .update_fact(&user, superseded)
        .await
        .expect("update");

    // A quota of 3 stored memories is reached although only one memory is active.
    let engine = engine(fact_store, None, plan_store, limits(true, 3));
    let output = engine
        .get_org_usage_dashboard(OrgUsageDashboardInput {
            org_id: org_id.to_string(),
            created_after: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            created_before: Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap(),
        })
        .await
        .expect("dashboard");
    assert_eq!(output.memory.total_memories, 3);
    assert_eq!(output.memory.active_memories, 1);
    assert_eq!(output.quota.usage.total_memories, 3);

    let snapshot = engine
        .create_memory_usage_snapshot(CreateMemoryUsageSnapshotInput {
            org_id: org_id.to_string(),
            captured_at: None,
        })
        .await
        .expect("snapshot")
        .snapshot;
    assert_eq!((snapshot.total_memories, snapshot.active_memories), (3, 1));
}
