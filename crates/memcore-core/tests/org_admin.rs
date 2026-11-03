use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_core::{
    Fact, FactStore, ListOrgUsersInput, MemoryEngine, MemoryEvent, MemoryEventOperation,
    MemoryEventStore, MemorySource, MemoryType, OrgSummaryInput, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with_events(fact_store: Arc<MockFactStore>) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(Arc::new(MockMemoryEventStore::new()))
}

async fn insert_fact(
    store: &MockFactStore,
    tenant: &TenantContext,
    content: &str,
    updated_days_ago: i64,
) -> Fact {
    let updated_at = Utc::now() - Duration::days(updated_days_ago);
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
        updated_at,
        updated_at,
        json!({}),
    )
    .expect("fact");
    store
        .insert_fact(tenant, fact.clone())
        .await
        .expect("insert");
    fact
}

async fn insert_event(event_store: &MockMemoryEventStore, tenant: &TenantContext) {
    let event = MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        None,
        MemoryEventOperation::Add,
        None,
        Some("secret content".to_string()),
        Some("mock".to_string()),
        Some("mock-llm".to_string()),
        json!({}),
    );
    event_store
        .record_event(tenant, event)
        .await
        .expect("record event");
}

#[tokio::test]
async fn org_summary_counts_users_and_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    let tenant_a = tenant("org_admin_a", "user_1");
    let tenant_b = tenant("org_admin_a", "user_2");

    insert_fact(&fact_store, &tenant_a, "memory one", 1).await;
    insert_fact(&fact_store, &tenant_a, "memory two", 2).await;
    insert_fact(&fact_store, &tenant_b, "memory three", 3).await;

    let engine = engine_with_events(fact_store);
    let summary = engine
        .get_org_summary(OrgSummaryInput {
            org_id: "org_admin_a".to_string(),
        })
        .await
        .expect("summary");

    assert_eq!(summary.org_id, "org_admin_a");
    assert_eq!(summary.total_users, 2);
    assert_eq!(summary.total_facts, 3);
    assert_eq!(summary.total_events, Some(0));
}

#[tokio::test]
async fn org_summary_excludes_other_org() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(&fact_store, &tenant("org_target", "user_a"), "a", 1).await;
    insert_fact(&fact_store, &tenant("org_other", "user_b"), "b", 1).await;

    let engine = engine_with_events(fact_store);
    let summary = engine
        .get_org_summary(OrgSummaryInput {
            org_id: "org_target".to_string(),
        })
        .await
        .expect("summary");

    assert_eq!(summary.total_users, 1);
    assert_eq!(summary.total_facts, 1);
}

#[tokio::test]
async fn org_summary_includes_event_count() {
    let fact_store = Arc::new(MockFactStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let tenant_a = tenant("org_events", "user_a");

    insert_event(&event_store, &tenant_a).await;
    insert_event(&event_store, &tenant("org_other", "user_b")).await;

    let engine = MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(event_store);

    let summary = engine
        .get_org_summary(OrgSummaryInput {
            org_id: "org_events".to_string(),
        })
        .await
        .expect("summary");

    assert_eq!(summary.total_events, Some(1));
}

#[tokio::test]
async fn org_users_list_returns_only_requested_org() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(&fact_store, &tenant("org_users_a", "user_1"), "one", 1).await;
    insert_fact(&fact_store, &tenant("org_users_a", "user_2"), "two", 2).await;
    insert_fact(&fact_store, &tenant("org_users_b", "user_x"), "other", 1).await;

    let engine = engine_with_events(fact_store);
    let output = engine
        .list_org_users(ListOrgUsersInput {
            org_id: "org_users_a".to_string(),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("list users");

    assert_eq!(output.users.len(), 2);
    assert!(
        output
            .users
            .iter()
            .all(|user| user.user_id == "user_1" || user.user_id == "user_2")
    );
    assert!(output.next_cursor.is_none());
}

#[tokio::test]
async fn org_users_list_respects_limit() {
    let fact_store = Arc::new(MockFactStore::new());
    for index in 0..5 {
        insert_fact(
            &fact_store,
            &tenant("org_limit", &format!("user_{index}")),
            "memory",
            1,
        )
        .await;
    }

    let engine = engine_with_events(fact_store);
    let output = engine
        .list_org_users(ListOrgUsersInput {
            org_id: "org_limit".to_string(),
            limit: 2,
            cursor: None,
        })
        .await
        .expect("list users");

    assert_eq!(output.users.len(), 2);
}

#[tokio::test]
async fn org_users_list_excludes_deleted_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_deleted", "user_a");
    let fact = insert_fact(&fact_store, &tenant, "active", 1).await;
    let deleted = insert_fact(&fact_store, &tenant, "deleted", 2).await;
    fact_store
        .soft_delete_fact(&tenant, deleted.id)
        .await
        .expect("soft delete");

    let engine = engine_with_events(fact_store);
    let output = engine
        .list_org_users(ListOrgUsersInput {
            org_id: "org_deleted".to_string(),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("list users");

    assert_eq!(output.users.len(), 1);
    assert_eq!(output.users[0].user_id, "user_a");
    assert_eq!(output.users[0].memory_count, 1);
    assert_eq!(output.users[0].last_memory_at, Some(fact.updated_at));
}

#[tokio::test]
async fn org_users_list_rejects_limit_above_max() {
    let engine = engine_with_events(Arc::new(MockFactStore::new()));
    let error = engine
        .list_org_users(ListOrgUsersInput {
            org_id: "org_x".to_string(),
            limit: memcore_core::MAX_LIST_ORG_USERS_LIMIT + 1,
            cursor: None,
        })
        .await
        .expect_err("limit should be rejected");

    assert!(matches!(
        error,
        memcore_common::MemcoreError::ValidationError(_)
    ));
}
