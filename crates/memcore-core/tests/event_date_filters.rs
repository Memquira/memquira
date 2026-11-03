use std::sync::Arc;

use chrono::{TimeZone, Utc};
use memcore_core::{
    ListMemoryEventsInput, MemoryEngine, MemoryEvent, MemoryEventOperation, MemoryEventStore,
    SearchOrgMemoryEventsInput, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;

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

async fn insert_event_at(
    store: &MockMemoryEventStore,
    org_id: &str,
    user_id: &str,
    created_at: chrono::DateTime<Utc>,
) {
    let tenant = tenant(org_id, user_id);
    let mut event = MemoryEvent::new(
        org_id.to_string(),
        user_id.to_string(),
        None,
        MemoryEventOperation::Add,
        None,
        None,
        None,
        None,
        json!({}),
    );
    event.created_at = created_at;
    store.record_event(&tenant, event).await.expect("record");
}

#[tokio::test]
async fn user_event_query_filters_by_created_after() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let jan = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
    let mar = Utc.with_ymd_and_hms(2026, 3, 10, 0, 0, 0).unwrap();
    insert_event_at(&event_store, "org_date", "user_a", jan).await;
    insert_event_at(&event_store, "org_date", "user_a", mar).await;

    let engine = engine_with_events(event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_date", "user_a"),
            fact_id: None,
            operation: None,
            created_after: Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()),
            created_before: None,
            query_text: None,
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].created_at, mar);
}

#[tokio::test]
async fn user_event_query_filters_by_created_before() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let jan = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
    let mar = Utc.with_ymd_and_hms(2026, 3, 10, 0, 0, 0).unwrap();
    insert_event_at(&event_store, "org_date", "user_a", jan).await;
    insert_event_at(&event_store, "org_date", "user_a", mar).await;

    let engine = engine_with_events(event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_date", "user_a"),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()),
            query_text: None,
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].created_at, jan);
}

#[tokio::test]
async fn org_event_query_filters_by_date_range() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let early = Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap();
    let mid = Utc.with_ymd_and_hms(2026, 3, 10, 0, 0, 0).unwrap();
    let late = Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap();
    insert_event_at(&event_store, "org_range", "user_a", early).await;
    insert_event_at(&event_store, "org_range", "user_a", mid).await;
    insert_event_at(&event_store, "org_other", "user_x", mid).await;
    insert_event_at(&event_store, "org_range", "user_b", late).await;

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_range".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()),
            created_before: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
            query_text: None,
            limit: 10,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].created_at, mid);
}

#[tokio::test]
async fn date_filtered_results_are_ordered_descending() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event_at(
        &event_store,
        "org_order",
        "user_a",
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    )
    .await;
    insert_event_at(
        &event_store,
        "org_order",
        "user_a",
        Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
    )
    .await;
    insert_event_at(
        &event_store,
        "org_order",
        "user_a",
        Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
    )
    .await;

    let engine = engine_with_events(event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_order", "user_a"),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 3);
    assert!(output.events[0].created_at > output.events[1].created_at);
    assert!(output.events[1].created_at > output.events[2].created_at);
}

#[tokio::test]
async fn date_filter_respects_limit() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    for day in 1..=5 {
        insert_event_at(
            &event_store,
            "org_limit_date",
            "user_a",
            Utc.with_ymd_and_hms(2026, 1, day, 0, 0, 0).unwrap(),
        )
        .await;
    }

    let engine = engine_with_events(event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_limit_date".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
            created_before: None,
            query_text: None,
            limit: 2,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 2);
}

#[tokio::test]
async fn invalid_date_range_returns_validation_error() {
    let engine = engine_with_events(Arc::new(MockMemoryEventStore::new()));
    let error = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_x", "user_a"),
            fact_id: None,
            operation: None,
            created_after: Some(Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap()),
            created_before: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
            query_text: None,
            limit: 10,
            cursor: None,
        })
        .await
        .expect_err("invalid range");

    assert!(matches!(
        error,
        memcore_common::MemcoreError::ValidationError(_)
    ));
}
