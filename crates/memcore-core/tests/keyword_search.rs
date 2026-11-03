use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_core::{
    FactStore, ListMemoriesInput, ListMemoryEventsInput, MemoryEngine, MemoryEvent,
    MemoryEventOperation, MemoryEventStore, MemoryType, SearchOrgMemoryEventsInput, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{FactSearchQuery, MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with_stores(
    fact_store: Arc<MockFactStore>,
    event_store: Arc<MockMemoryEventStore>,
) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(event_store)
}

async fn insert_fact(
    store: &MockFactStore,
    org_id: &str,
    user_id: &str,
    content: &str,
    memory_type: MemoryType,
    summary: Option<&str>,
) {
    let now = Utc::now();
    let fact = memcore_core::Fact::new(
        Uuid::new_v4(),
        org_id,
        user_id,
        memory_type,
        content,
        summary.map(str::to_string),
        memcore_core::MemorySource::UserMessage,
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
        .insert_fact(&tenant(org_id, user_id), fact)
        .await
        .expect("insert");
}

async fn insert_event(
    store: &MockMemoryEventStore,
    org_id: &str,
    user_id: &str,
    previous_content: Option<&str>,
    new_content: Option<&str>,
    provider_name: Option<&str>,
    model_name: Option<&str>,
    operation: MemoryEventOperation,
) {
    let tenant = tenant(org_id, user_id);
    let event = MemoryEvent::new(
        org_id,
        user_id,
        None,
        operation,
        previous_content.map(str::to_string),
        new_content.map(str::to_string),
        provider_name.map(str::to_string),
        model_name.map(str::to_string),
        json!({}),
    );
    store.record_event(&tenant, event).await.expect("record");
}

#[tokio::test]
async fn memory_list_q_finds_matching_content() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "learning Rust async",
        MemoryType::Skill,
        None,
    )
    .await;
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "python basics",
        MemoryType::Skill,
        None,
    )
    .await;

    let engine = engine_with_stores(fact_store, Arc::new(MockMemoryEventStore::new()));
    let output = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_kw", "user_a"),
            memory_type: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(output.memories.len(), 1);
    assert!(
        output.memories[0]
            .content
            .to_ascii_lowercase()
            .contains("rust")
    );
}

#[tokio::test]
async fn memory_list_q_is_case_insensitive() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "RUST programming",
        MemoryType::Skill,
        None,
    )
    .await;

    let engine = engine_with_stores(fact_store, Arc::new(MockMemoryEventStore::new()));
    let output = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_kw", "user_a"),
            memory_type: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(output.memories.len(), 1);
}

#[tokio::test]
async fn memory_list_q_does_not_return_other_users_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "rust for user a",
        MemoryType::Skill,
        None,
    )
    .await;
    insert_fact(
        &fact_store,
        "org_kw",
        "user_b",
        "rust for user b",
        MemoryType::Skill,
        None,
    )
    .await;

    let engine = engine_with_stores(fact_store, Arc::new(MockMemoryEventStore::new()));
    let output = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_kw", "user_a"),
            memory_type: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(output.memories.len(), 1);
    assert_eq!(output.memories[0].user_id, "user_a");
}

#[tokio::test]
async fn memory_list_q_does_not_return_other_orgs_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(
        &fact_store,
        "org_a",
        "user_a",
        "rust org a",
        MemoryType::Skill,
        None,
    )
    .await;
    insert_fact(
        &fact_store,
        "org_b",
        "user_a",
        "rust org b",
        MemoryType::Skill,
        None,
    )
    .await;

    let engine = engine_with_stores(fact_store, Arc::new(MockMemoryEventStore::new()));
    let output = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_a", "user_a"),
            memory_type: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(output.memories.len(), 1);
    assert_eq!(output.memories[0].org_id, "org_a");
}

#[tokio::test]
async fn memory_type_filter_works_with_q() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "rust skill",
        MemoryType::Skill,
        None,
    )
    .await;
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "rust profile",
        MemoryType::Profile,
        None,
    )
    .await;

    let engine = engine_with_stores(fact_store, Arc::new(MockMemoryEventStore::new()));
    let output = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_kw", "user_a"),
            memory_type: Some(MemoryType::Profile),
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(output.memories.len(), 1);
    assert_eq!(output.memories[0].memory_type, MemoryType::Profile);
}

#[tokio::test]
async fn pagination_works_with_q() {
    let fact_store = Arc::new(MockFactStore::new());
    let base = Utc::now();
    for index in 0..3 {
        let now = base - Duration::seconds(i64::from(index));
        let fact = memcore_core::Fact::new(
            Uuid::new_v4(),
            "org_page",
            "user_a",
            MemoryType::Skill,
            format!("rust memory {index}"),
            None,
            memcore_core::MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            now,
            now,
            json!({}),
        )
        .expect("fact");
        fact_store
            .insert_fact(&tenant("org_page", "user_a"), fact)
            .await
            .expect("insert");
    }

    let engine = engine_with_stores(fact_store, Arc::new(MockMemoryEventStore::new()));
    let first = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_page", "user_a"),
            memory_type: None,
            query_text: Some("rust".to_string()),
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
            tenant: tenant("org_page", "user_a"),
            memory_type: None,
            query_text: Some("rust".to_string()),
            limit: 2,
            cursor: first.next_cursor,
            include_deleted: false,
        })
        .await
        .expect("second page");

    assert_eq!(second.memories.len(), 1);
}

#[tokio::test]
async fn user_event_q_finds_matching_previous_content() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        Some("old rust content"),
        Some("new content"),
        None,
        None,
        MemoryEventOperation::Update,
    )
    .await;
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        Some("python only"),
        Some("updated"),
        None,
        None,
        MemoryEventOperation::Update,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_ev", "user_a"),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
    assert_eq!(
        output.events[0].previous_content.as_deref(),
        Some("old rust content")
    );
}

#[tokio::test]
async fn user_event_q_finds_matching_new_content() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        None,
        Some("prefers Rust async"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_ev", "user_a"),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: Some("async".to_string()),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
}

#[tokio::test]
async fn user_event_q_is_case_insensitive() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        None,
        Some("RUST"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_ev", "user_a"),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
}

#[tokio::test]
async fn user_event_q_works_with_operation_filter() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        None,
        Some("rust add"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        None,
        Some("rust delete"),
        None,
        None,
        MemoryEventOperation::Delete,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_ev", "user_a"),
            fact_id: None,
            operation: Some(MemoryEventOperation::Delete),
            created_after: None,
            created_before: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].operation, MemoryEventOperation::Delete);
}

#[tokio::test]
async fn user_event_q_preserves_tenant_isolation() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_ev",
        "user_a",
        None,
        Some("rust secret"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;
    insert_event(
        &event_store,
        "org_ev",
        "user_b",
        None,
        Some("rust other user"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant: tenant("org_ev", "user_a"),
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: Some("rust".to_string()),
            limit: 10,
            cursor: None,
        })
        .await
        .expect("list");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].user_id, "user_a");
}

#[tokio::test]
async fn admin_event_q_searches_across_users_in_same_org() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_admin",
        "user_1",
        None,
        Some("rust user one"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;
    insert_event(
        &event_store,
        "org_admin",
        "user_2",
        None,
        Some("rust user two"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_admin".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: Some("rust".to_string()),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 2);
}

#[tokio::test]
async fn admin_event_q_does_not_return_other_orgs_events() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    insert_event(
        &event_store,
        "org_a",
        "user_a",
        None,
        Some("rust org a"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;
    insert_event(
        &event_store,
        "org_b",
        "user_a",
        None,
        Some("rust org b"),
        None,
        None,
        MemoryEventOperation::Add,
    )
    .await;

    let engine = engine_with_stores(Arc::new(MockFactStore::new()), event_store);
    let output = engine
        .search_org_memory_events(SearchOrgMemoryEventsInput {
            org_id: "org_a".to_string(),
            user_id: None,
            fact_id: None,
            operation: None,
            created_after: None,
            created_before: None,
            query_text: Some("rust".to_string()),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("search");

    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].org_id, "org_a");
}

#[tokio::test]
async fn fact_search_q_matches_summary() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_fact(
        &fact_store,
        "org_kw",
        "user_a",
        "unrelated body",
        MemoryType::Profile,
        Some("rust summary note"),
    )
    .await;

    let results = fact_store
        .search_facts(FactSearchQuery {
            tenant: tenant("org_kw", "user_a"),
            memory_types: None,
            query_text: Some("summary".to_string()),
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("search");

    assert_eq!(results.len(), 1);
}
