use std::sync::Arc;

use memcore_core::{
    MemoryEngine, MemoryEvent, MemoryEventOperation, MemoryEventStore, SearchOrgMemoryEventsInput,
    TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with_events(event_store: Arc<MockMemoryEventStore>) -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(event_store)
}

async fn insert_event(
    store: &MockMemoryEventStore,
    org_id: &str,
    user_id: &str,
    operation: MemoryEventOperation,
    fact_id: Option<Uuid>,
) -> MemoryEvent {
    let tenant = tenant(org_id, user_id);
    let event = MemoryEvent::new(
        org_id.to_string(),
        user_id.to_string(),
        fact_id,
        operation,
        Some("secret input text".to_string()),
        Some("previous".to_string()),
        Some("mock".to_string()),
        Some("mock-llm".to_string()),
        json!({}),
    );
    store
        .record_event(&tenant, event.clone())
        .await
        .expect("record event");
    event
}

#[tokio::test]
async fn org_audit_search_returns_events_for_current_org() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_audit_a",
        "user_1",
        MemoryEventOperation::Add,
        None,
    )
    .await;
    insert_event(
        &event_store,
        "org_audit_a",
        "user_2",
        MemoryEventOperation::Update,
        Some(Uuid::new_v4()),
    )
    .await;

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_audit_a".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 2);
}

#[tokio::test]
async fn org_audit_search_excludes_other_org() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_target",
        "user_a",
        MemoryEventOperation::Add,
        None,
    )
    .await;
    insert_event(
        &event_store,
        "org_other",
        "user_b",
        MemoryEventOperation::Add,
        None,
    )
    .await;

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_target".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].org_id, "org_target");
}

#[tokio::test]
async fn org_audit_search_user_id_filter_works() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_filter",
        "user_a",
        MemoryEventOperation::Add,
        None,
    )
    .await;
    insert_event(
        &event_store,
        "org_filter",
        "user_b",
        MemoryEventOperation::Add,
        None,
    )
    .await;

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_filter".to_string(),
            user_id: Some("user_a".to_string()),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].user_id, "user_a");
}

#[tokio::test]
async fn org_audit_search_fact_id_filter_works() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let fact_id = Uuid::new_v4();
    insert_event(
        &event_store,
        "org_fact",
        "user_a",
        MemoryEventOperation::Update,
        Some(fact_id),
    )
    .await;
    insert_event(
        &event_store,
        "org_fact",
        "user_a",
        MemoryEventOperation::Add,
        None,
    )
    .await;

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_fact".to_string(),
            user_id: None,
            fact_id: Some(fact_id),
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].fact_id, Some(fact_id));
}

#[tokio::test]
async fn org_audit_search_operation_filter_works() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_op",
        "user_a",
        MemoryEventOperation::Delete,
        None,
    )
    .await;
    insert_event(
        &event_store,
        "org_op",
        "user_a",
        MemoryEventOperation::Add,
        None,
    )
    .await;

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_op".to_string(),
            user_id: None,
            fact_id: None,
            operation: Some(MemoryEventOperation::Delete),
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].operation, MemoryEventOperation::Delete);
}

#[tokio::test]
async fn org_audit_search_respects_limit() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    for index in 0..5 {
        insert_event(
            &event_store,
            "org_limit",
            &format!("user_{index}"),
            MemoryEventOperation::Add,
            None,
        )
        .await;
    }

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_limit".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 2,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 2);
    assert!(output.next_cursor.is_some());
}

#[tokio::test]
async fn org_audit_search_rejects_limit_above_max() {
    let engine = engine_with_events(Arc::new(MockMemoryEventStore::new()));
    let error = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_x".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: memcore_core::MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT + 1,
            cursor: None,
        })
        .await
        .expect_err("limit should be rejected");

    assert!(matches!(
        error,
        memcore_common::MemcoreError::ValidationError(_)
    ));
}
