use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_common::MemcoreError;
use memcore_core::{
    FactStore, ListMemoriesInput, ListMemoryEventsInput, ListOrgUsersInput, MemoryEngine,
    MemoryEvent, MemoryEventOperation, MemoryEventStore, TenantContext, decode_cursor,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with_stores(
    fact_store: Arc<MockFactStore>,
    event_store: Option<Arc<MockMemoryEventStore>>,
) -> MemoryEngine {
    let mut engine = MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    );
    if let Some(store) = event_store {
        engine = engine.with_event_store(store);
    }
    engine
}

async fn insert_fact_at(
    store: &MockFactStore,
    tenant: &TenantContext,
    content: &str,
    updated_at: chrono::DateTime<Utc>,
) {
    let fact = memcore_core::Fact::new(
        Uuid::new_v4(),
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        memcore_core::MemoryType::Skill,
        content,
        None,
        memcore_core::MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        updated_at,
        updated_at,
        serde_json::json!({}),
    )
    .expect("fact");
    store.insert_fact(tenant, fact).await.expect("insert");
}

#[tokio::test]
async fn list_memories_first_page_returns_next_cursor_when_more_exist() {
    let fact_store = Arc::new(MockFactStore::new());
    let tenant = tenant("org_page", "user_page");
    let base = Utc::now();
    for index in 0..3 {
        insert_fact_at(
            &fact_store,
            &tenant,
            &format!("memory {index}"),
            base - Duration::seconds(i64::from(index)),
        )
        .await;
    }

    let engine = engine_with_stores(fact_store, None);
    let first = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant.clone(),
            memory_type: None,
            query_text: None,
            limit: 2,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("first page");

    assert_eq!(first.memories.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = engine
        .list_memories(ListMemoriesInput {
            tenant,
            memory_type: None,
            query_text: None,
            limit: 2,
            cursor: first.next_cursor,
            include_deleted: false,
        })
        .await
        .expect("second page");

    assert_eq!(second.memories.len(), 1);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn list_memories_invalid_cursor_returns_validation_error() {
    let engine = engine_with_stores(Arc::new(MockFactStore::new()), None);
    let error = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_x", "user_x"),
            memory_type: None,
            query_text: None,
            limit: 10,
            cursor: Some("bad-cursor".to_string()),
            include_deleted: false,
        })
        .await
        .unwrap_err();

    assert!(matches!(error, MemcoreError::ValidationError(message) if message == "invalid cursor"));
}

#[tokio::test]
async fn list_memories_pagination_preserves_tenant_isolation() {
    let fact_store = Arc::new(MockFactStore::new());
    let tenant_a = tenant("org_iso", "user_a");
    let tenant_b = tenant("org_iso", "user_b");
    let now = Utc::now();
    insert_fact_at(&fact_store, &tenant_a, "a1", now).await;
    insert_fact_at(&fact_store, &tenant_a, "a2", now - Duration::seconds(1)).await;
    insert_fact_at(&fact_store, &tenant_b, "b1", now).await;

    let engine = engine_with_stores(fact_store, None);
    let output = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant_a,
            memory_type: None,
            query_text: None,
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(output.memories.len(), 2);
    assert!(output.memories.iter().all(|fact| fact.user_id == "user_a"));
}

#[tokio::test]
async fn list_org_users_paginates_across_pages() {
    let fact_store = Arc::new(MockFactStore::new());
    let base = Utc::now();
    for index in 0..3 {
        insert_fact_at(
            &fact_store,
            &tenant("org_users_page", &format!("user_{index}")),
            "memory",
            base - Duration::seconds(i64::from(index)),
        )
        .await;
    }

    let engine = engine_with_stores(fact_store, None);
    let first = engine
        .list_org_users(ListOrgUsersInput {
            org_id: "org_users_page".to_string(),
            limit: 2,
            cursor: None,
        })
        .await
        .expect("first page");

    assert_eq!(first.users.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = engine
        .list_org_users(ListOrgUsersInput {
            org_id: "org_users_page".to_string(),
            limit: 2,
            cursor: first.next_cursor,
        })
        .await
        .expect("second page");

    assert_eq!(second.users.len(), 1);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn list_memory_events_paginates_with_operation_filter() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let tenant = tenant("org_evt", "user_evt");
    let base = Utc::now();

    for index in 0..3 {
        let mut event = MemoryEvent::new(
            tenant.org_id.clone(),
            tenant.user_id.clone(),
            None,
            MemoryEventOperation::Add,
            None,
            None,
            None,
            None,
            serde_json::json!({}),
        );
        event.created_at = base - Duration::seconds(i64::from(index));
        event_store
            .record_event(&tenant, event)
            .await
            .expect("record");
    }

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), Some(event_store));
    let first = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant.clone(),
            fact_id: None,
            operation: Some(MemoryEventOperation::Add),
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 2,
            cursor: None,
        })
        .await
        .expect("first page");

    assert_eq!(first.events.len(), 2);
    let cursor = first.next_cursor.clone().expect("next cursor");
    decode_cursor(&cursor).expect("valid cursor");

    let second = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant,
            fact_id: None,
            operation: Some(MemoryEventOperation::Add),
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 2,
            cursor: first.next_cursor,
        })
        .await
        .expect("second page");

    assert_eq!(second.events.len(), 1);
    assert!(second.next_cursor.is_none());
}
