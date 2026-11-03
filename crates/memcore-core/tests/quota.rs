use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_core::{
    CheckMemoryWriteQuotaInput, Fact, FactStore, GetOrgQuotaStatusInput, MemoryEngine,
    MemorySource, MemoryType, OrgPlanConfig, OrgPlanLimits, OrgPlanStore, OrgPlanTier,
    OrgQuotaLimits, ProviderCallStatus, ProviderUsageCapability, ProviderUsageEventRecord,
    ProviderUsageStore, QuotaLimitKind, QuotaLimitSource, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockOrgPlanStore, MockProviderUsageStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine(
    fact_store: Arc<MockFactStore>,
    usage_store: Option<Arc<dyn ProviderUsageStore>>,
    quota_limits: OrgQuotaLimits,
) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_provider_usage_store(usage_store)
    .with_org_plan_store(Arc::new(MockOrgPlanStore::new()))
    .with_global_quota_limits(quota_limits)
}

fn engine_with_plan_store(
    fact_store: Arc<MockFactStore>,
    plan_store: Arc<MockOrgPlanStore>,
    quota_limits: OrgQuotaLimits,
) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_org_plan_store(plan_store)
    .with_global_quota_limits(quota_limits)
}

fn limits(
    enabled: bool,
    max_users: u64,
    max_user_memories: u64,
    max_org_memories: u64,
    daily_requests: u64,
    daily_tokens: u64,
) -> OrgQuotaLimits {
    OrgQuotaLimits::from_raw(
        enabled,
        max_users,
        max_user_memories,
        max_org_memories,
        daily_requests,
        daily_tokens,
    )
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

async fn record_usage(
    store: &MockProviderUsageStore,
    org_id: &str,
    total_tokens: u64,
    created_at: chrono::DateTime<Utc>,
) {
    store
        .record_usage_event(ProviderUsageEventRecord {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            user_id: Some("user_a".to_string()),
            provider_name: "mock".to_string(),
            model_name: Some("mock-llm".to_string()),
            capability: ProviderUsageCapability::Llm,
            operation_name: "llm_extract_facts".to_string(),
            status: ProviderCallStatus::Success,
            input_tokens: Some(total_tokens),
            output_tokens: None,
            total_tokens: Some(total_tokens),
            retry_count: 0,
            fallback_used: false,
            circuit_blocked: false,
            timed_out: false,
            estimated_cost_usd: None,
            metadata: None,
            created_at,
        })
        .await
        .expect("record usage");
}

#[tokio::test]
async fn disabled_quotas_always_allow() {
    let store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_disabled_quota", "user_a");
    insert_fact(&store, &tenant, "one").await;
    let result = engine(store, None, limits(false, 1, 1, 1, 1, 1))
        .check_memory_write_quota(CheckMemoryWriteQuotaInput {
            org_id: tenant.org_id,
            user_id: tenant.user_id,
            requested_new_memories: 10,
        })
        .await
        .expect("quota");

    assert!(result.allowed);
    assert!(result.violations.is_empty());
}

#[tokio::test]
async fn max_memories_per_org_violation() {
    let store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_limit", "user_a");
    insert_fact(&store, &tenant, "one").await;

    let result = engine(store, None, limits(true, 0, 0, 1, 0, 0))
        .check_memory_write_quota(CheckMemoryWriteQuotaInput {
            org_id: tenant.org_id,
            user_id: tenant.user_id,
            requested_new_memories: 1,
        })
        .await
        .expect("quota");

    assert!(!result.allowed);
    assert_eq!(result.violations[0].kind, QuotaLimitKind::MemoriesPerOrg);
}

#[tokio::test]
async fn max_memories_per_user_violation() {
    let store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_user_limit", "user_a");
    insert_fact(&store, &tenant, "one").await;

    let result = engine(store, None, limits(true, 0, 1, 0, 0, 0))
        .check_memory_write_quota(CheckMemoryWriteQuotaInput {
            org_id: tenant.org_id,
            user_id: tenant.user_id,
            requested_new_memories: 1,
        })
        .await
        .expect("quota");

    assert!(!result.allowed);
    assert_eq!(result.violations[0].kind, QuotaLimitKind::MemoriesPerUser);
}

#[tokio::test]
async fn max_users_per_org_violation() {
    let store = Arc::new(MockFactStore::new());
    let existing = tenant("org_users_limit", "user_existing");
    let new_user = tenant("org_users_limit", "user_new");
    insert_fact(&store, &existing, "one").await;

    let result = engine(store, None, limits(true, 1, 0, 0, 0, 0))
        .check_memory_write_quota(CheckMemoryWriteQuotaInput {
            org_id: new_user.org_id,
            user_id: new_user.user_id,
            requested_new_memories: 1,
        })
        .await
        .expect("quota");

    assert!(!result.allowed);
    assert_eq!(result.violations[0].kind, QuotaLimitKind::UsersPerOrg);
}

#[tokio::test]
async fn daily_provider_request_and_token_violations_use_persisted_usage() {
    let fact_store = Arc::new(MockFactStore::new());
    let usage_store = Arc::new(MockProviderUsageStore::new());
    record_usage(&usage_store, "org_provider_quota", 20, Utc::now()).await;
    record_usage(&usage_store, "org_provider_quota", 20, Utc::now()).await;
    record_usage(
        &usage_store,
        "org_provider_quota",
        999,
        Utc::now() - Duration::days(2),
    )
    .await;

    let result = engine(fact_store, Some(usage_store), limits(true, 0, 0, 0, 1, 30))
        .get_org_quota_status(GetOrgQuotaStatusInput {
            org_id: "org_provider_quota".to_string(),
            user_id: None,
        })
        .await
        .expect("quota status");

    assert_eq!(result.usage.daily_provider_requests, 2);
    assert_eq!(result.usage.daily_provider_tokens, 40);
    assert_eq!(result.violations.len(), 2);
    assert!(
        result
            .violations
            .iter()
            .any(|v| v.kind == QuotaLimitKind::DailyProviderRequests)
    );
    assert!(
        result
            .violations
            .iter()
            .any(|v| v.kind == QuotaLimitKind::DailyProviderTokens)
    );
}

#[tokio::test]
async fn zero_limits_mean_unlimited_and_result_includes_usage_and_limits() {
    let store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_unlimited", "user_a");
    insert_fact(&store, &tenant, "one").await;

    let result = engine(store, None, limits(true, 0, 0, 0, 0, 0))
        .get_org_quota_status(GetOrgQuotaStatusInput {
            org_id: "org_unlimited".to_string(),
            user_id: Some("user_a".to_string()),
        })
        .await
        .expect("quota status");

    assert!(result.allowed);
    assert_eq!(result.usage.total_users, 1);
    assert_eq!(result.usage.total_memories, 1);
    assert_eq!(result.usage.user_memory_count, Some(1));
    assert!(result.limits.max_memories_per_org.is_none());
}

#[tokio::test]
async fn usage_window_uses_current_utc_day() {
    let result = engine(
        Arc::new(MockFactStore::new()),
        None,
        limits(false, 0, 0, 0, 0, 0),
    )
    .get_org_quota_status(GetOrgQuotaStatusInput {
        org_id: "org_window".to_string(),
        user_id: None,
    })
    .await
    .expect("quota status");

    assert_eq!(result.usage.window_start.time().to_string(), "00:00:00");
    assert_eq!(
        result.usage.window_end - result.usage.window_start,
        Duration::days(1)
    );
}

#[tokio::test]
async fn org_isolation_is_preserved() {
    let store = Arc::new(MockFactStore::new());
    insert_fact(&store, &tenant("org_a", "user_a"), "one").await;
    insert_fact(&store, &tenant("org_b", "user_b"), "two").await;

    let result = engine(store, None, limits(true, 0, 0, 0, 0, 0))
        .get_org_quota_status(GetOrgQuotaStatusInput {
            org_id: "org_a".to_string(),
            user_id: Some("user_a".to_string()),
        })
        .await
        .expect("quota status");

    assert_eq!(result.usage.total_users, 1);
    assert_eq!(result.usage.total_memories, 1);
}

#[tokio::test]
async fn active_org_plan_overrides_global_limits() {
    let fact_store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_plan_override", "user_a");
    insert_fact(&fact_store, &tenant, "one").await;

    let plan_store = Arc::new(MockOrgPlanStore::new());
    plan_store
        .upsert_org_plan(plan_config("org_plan_override", true, Some(1)))
        .await
        .expect("upsert plan");

    let result = engine_with_plan_store(fact_store, plan_store, limits(true, 0, 0, 999, 0, 0))
        .check_memory_write_quota(CheckMemoryWriteQuotaInput {
            org_id: tenant.org_id,
            user_id: tenant.user_id,
            requested_new_memories: 1,
        })
        .await
        .expect("quota");

    assert_eq!(result.source, QuotaLimitSource::OrgPlan);
    assert!(!result.allowed);
    assert_eq!(result.violations[0].kind, QuotaLimitKind::MemoriesPerOrg);
}

#[tokio::test]
async fn inactive_org_plan_uses_global_limits() {
    let fact_store = Arc::new(MockFactStore::new());
    let plan_store = Arc::new(MockOrgPlanStore::new());
    plan_store
        .upsert_org_plan(plan_config("org_inactive_plan", false, Some(1)))
        .await
        .expect("upsert plan");

    let result = engine_with_plan_store(fact_store, plan_store, limits(true, 0, 0, 0, 0, 0))
        .get_org_quota_status(GetOrgQuotaStatusInput {
            org_id: "org_inactive_plan".to_string(),
            user_id: None,
        })
        .await
        .expect("quota");

    assert_eq!(result.source, QuotaLimitSource::GlobalConfig);
    assert!(result.limits.max_memories_per_org.is_none());
}

fn plan_config(org_id: &str, is_active: bool, max_memories_per_org: Option<u64>) -> OrgPlanConfig {
    let now = Utc::now();
    OrgPlanConfig {
        org_id: org_id.to_string(),
        tier: OrgPlanTier::Pro,
        limits: OrgPlanLimits {
            max_users_per_org: None,
            max_memories_per_user: None,
            max_memories_per_org,
            daily_provider_request_limit: None,
            daily_provider_token_limit: None,
        },
        is_active,
        metadata: None,
        created_at: now,
        updated_at: now,
    }
}
