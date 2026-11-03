use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_core::{
    ApplyRetentionInput, Fact, FactStore, ListMemoriesInput, ListMemoryEventsInput, MemoryEngine,
    MemoryEvent, MemoryEventOperation, MemoryEventStore, MemorySource, MemoryType, RetentionPolicy,
    TenantContext,
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

async fn insert_event(
    event_store: &MockMemoryEventStore,
    tenant: &TenantContext,
    created_days_ago: i64,
) {
    let mut event = MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        None,
        MemoryEventOperation::Add,
        None,
        Some("new".to_string()),
        Some("mock".to_string()),
        Some("mock-llm".to_string()),
        json!({}),
    );
    event.created_at = Utc::now() - Duration::days(created_days_ago);
    event_store
        .record_event(tenant, event)
        .await
        .expect("record event");
}

fn enabled_policy(fact_days: u32, event_days: u32) -> RetentionPolicy {
    RetentionPolicy {
        enabled: true,
        fact_retention_days: if fact_days == 0 {
            None
        } else {
            Some(fact_days)
        },
        event_retention_days: if event_days == 0 {
            None
        } else {
            Some(event_days)
        },
    }
}

#[tokio::test]
async fn disabled_policy_returns_zero_counts() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_events(fact_store.clone());
    let tenant = tenant("org_a", "user_a");
    insert_fact(&fact_store, &tenant, "old", 400).await;

    let output = engine
        .apply_retention(ApplyRetentionInput {
            tenant,
            policy: RetentionPolicy::disabled(),
            dry_run: false,
        })
        .await
        .expect("apply");

    assert_eq!(output.facts_matched, 0);
    assert_eq!(output.facts_deleted, 0);
}

#[tokio::test]
async fn dry_run_counts_old_facts_without_deleting() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_events(fact_store.clone());
    let tenant = tenant("org_a", "user_a");

    insert_fact(&fact_store, &tenant, "old", 400).await;
    insert_fact(&fact_store, &tenant, "recent", 10).await;

    let output = engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant.clone(),
            policy: enabled_policy(365, 0),
            dry_run: true,
        })
        .await
        .expect("dry-run");

    assert!(output.dry_run);
    assert_eq!(output.facts_matched, 1);
    assert_eq!(output.facts_deleted, 0);

    let listed = engine
        .list_memories(ListMemoriesInput {
            tenant,
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(listed.memories.len(), 2);
}

#[tokio::test]
async fn apply_soft_deletes_old_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_events(fact_store.clone());
    let tenant = tenant("org_a", "user_a");

    insert_fact(&fact_store, &tenant, "old", 400).await;
    insert_fact(&fact_store, &tenant, "recent", 10).await;

    let output = engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant.clone(),
            policy: enabled_policy(365, 0),
            dry_run: false,
        })
        .await
        .expect("apply");

    assert_eq!(output.facts_matched, 1);
    assert_eq!(output.facts_deleted, 1);

    let listed = engine
        .list_memories(ListMemoriesInput {
            tenant,
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(listed.memories.len(), 1);
    assert_eq!(listed.memories[0].content, "recent");
}

#[tokio::test]
async fn dry_run_counts_old_events_without_deleting() {
    let fact_store = Arc::new(MockFactStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(event_store.clone());

    let tenant = tenant("org_a", "user_a");
    insert_event(&event_store, &tenant, 120).await;
    insert_event(&event_store, &tenant, 5).await;

    let output = engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant.clone(),
            policy: enabled_policy(0, 90),
            dry_run: true,
        })
        .await
        .expect("dry-run");

    assert_eq!(output.events_matched, 1);
    assert_eq!(output.events_deleted, 0);

    let events = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant,
            operation: None,
            fact_id: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 20,
            cursor: None,
        })
        .await
        .expect("list events");

    assert_eq!(events.events.len(), 2);
}

#[tokio::test]
async fn apply_deletes_old_events() {
    let fact_store = Arc::new(MockFactStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(event_store.clone());

    let tenant = tenant("org_a", "user_a");
    insert_event(&event_store, &tenant, 120).await;
    insert_event(&event_store, &tenant, 5).await;

    let output = engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant.clone(),
            policy: enabled_policy(0, 90),
            dry_run: false,
        })
        .await
        .expect("apply");

    assert_eq!(output.events_matched, 1);
    assert_eq!(output.events_deleted, 1);

    let events = engine
        .list_memory_events(ListMemoryEventsInput {
            tenant,
            operation: None,
            fact_id: None,
            created_after: None,
            created_before: None,
            query_text: None,
            limit: 20,
            cursor: None,
        })
        .await
        .expect("list events");

    assert_eq!(events.events.len(), 1);
}

#[tokio::test]
async fn retention_does_not_affect_other_user() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_events(fact_store.clone());

    insert_fact(&fact_store, &tenant("org_a", "user_a"), "old a", 400).await;
    insert_fact(&fact_store, &tenant("org_a", "user_b"), "old b", 400).await;

    engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant("org_a", "user_a"),
            policy: enabled_policy(365, 0),
            dry_run: false,
        })
        .await
        .expect("apply");

    let listed_b = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_a", "user_b"),
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(listed_b.memories.len(), 1);
}

#[tokio::test]
async fn retention_does_not_affect_other_org() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_events(fact_store.clone());

    insert_fact(&fact_store, &tenant("org_a", "user_a"), "old", 400).await;
    insert_fact(&fact_store, &tenant("org_b", "user_a"), "old", 400).await;

    engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant("org_a", "user_a"),
            policy: enabled_policy(365, 0),
            dry_run: false,
        })
        .await
        .expect("apply");

    let listed_b = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_b", "user_a"),
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(listed_b.memories.len(), 1);
}
