use std::sync::Arc;

use memcore_core::{
    AddMemoryInput, DeleteMemoryInput, ExportUserDataInput, MemoryEngine, MemoryMessage,
    MessageRole, TenantContext, USER_EXPORT_FORMAT_VERSION,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant should be valid")
}

fn engine_with_events() -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(Arc::new(MockMemoryEventStore::new()))
}

#[tokio::test]
async fn export_includes_facts_after_add_memory() {
    let engine = engine_with_events();
    let tenant = tenant("org_a", "user_a");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Export test memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant,
            include_events: true,
            include_deleted: false,
        })
        .await
        .expect("export should succeed");

    assert_eq!(export.format_version, USER_EXPORT_FORMAT_VERSION);
    assert_eq!(export.org_id, "org_a");
    assert_eq!(export.user_id, "user_a");
    assert_eq!(export.facts.len(), 1);
    assert_eq!(export.facts[0].content, "Export test memory");
}

#[tokio::test]
async fn export_includes_events_when_requested() {
    let engine = engine_with_events();
    let tenant = tenant("org_a", "user_a");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Audit export memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant,
            include_events: true,
            include_deleted: false,
        })
        .await
        .expect("export should succeed");

    assert!(!export.memory_events.is_empty());
}

#[tokio::test]
async fn export_excludes_events_when_disabled() {
    let engine = engine_with_events();
    let tenant = tenant("org_a", "user_a");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "No events in export".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant,
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export should succeed");

    assert!(export.memory_events.is_empty());
}

#[tokio::test]
async fn export_excludes_deleted_facts_by_default() {
    let engine = engine_with_events();
    let tenant = tenant("org_a", "user_a");

    let added = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "to be deleted".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let memory_id = added.memories[0].id;
    engine
        .delete_memory(DeleteMemoryInput {
            tenant: tenant.clone(),
            memory_id,
        })
        .await
        .expect("delete should succeed");

    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant,
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export should succeed");

    assert!(export.facts.is_empty());
}

#[tokio::test]
async fn export_includes_deleted_facts_when_requested() {
    let engine = engine_with_events();
    let tenant = tenant("org_a", "user_a");

    let added = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "deleted but exported".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let memory_id = added.memories[0].id;
    engine
        .delete_memory(DeleteMemoryInput {
            tenant: tenant.clone(),
            memory_id,
        })
        .await
        .expect("delete should succeed");

    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant,
            include_events: false,
            include_deleted: true,
        })
        .await
        .expect("export should succeed");

    assert_eq!(export.facts.len(), 1);
    assert_eq!(export.facts[0].content, "deleted but exported");
}

#[tokio::test]
async fn export_is_scoped_to_tenant_user() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = MemoryEngine::new(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    );

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user a only memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add user a should succeed");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_b"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user b only memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add user b should succeed");

    let export_a = engine
        .export_user_data(ExportUserDataInput {
            tenant: tenant("org_a", "user_a"),
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export user a should succeed");

    assert_eq!(export_a.facts.len(), 1);
    assert_eq!(export_a.facts[0].content, "user a only memory");

    let export_b = engine
        .export_user_data(ExportUserDataInput {
            tenant: tenant("org_b", "user_a"),
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export other org should succeed with empty facts");

    assert!(export_b.facts.is_empty());
}
