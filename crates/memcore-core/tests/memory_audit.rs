use std::sync::Arc;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AddMemoryInput, CandidateFact, DeleteMemoryInput, FactOperation, FactOperationDecision,
    ForgetUserInput, MemoryEngine, MemoryEvent, MemoryEventOperation, MemoryEventQuery,
    MemoryEventStore, MemoryMessage, MemoryType, MessageRole, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant should be valid")
}

fn high_importance_candidate(content: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.8, None, json!({}))
        .expect("candidate should be valid")
}

fn engine_with_audit(
    fact_store: Arc<MockFactStore>,
    vector_store: Arc<MockVectorStore>,
    event_store: Arc<MockMemoryEventStore>,
    llm: MockLlmProvider,
) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        vector_store,
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(event_store)
    .with_audit_provider_info(Some("mock".to_string()), Some("mock-llm".to_string()))
}

struct FailingMemoryEventStore;

#[async_trait]
impl MemoryEventStore for FailingMemoryEventStore {
    async fn record_event(
        &self,
        _tenant: &TenantContext,
        _event: memcore_core::MemoryEvent,
    ) -> MemcoreResult<memcore_core::MemoryEvent> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }

    async fn list_events(
        &self,
        _query: MemoryEventQuery,
    ) -> MemcoreResult<Vec<memcore_core::MemoryEvent>> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }

    async fn delete_events_older_than(
        &self,
        _tenant: &TenantContext,
        _cutoff: chrono::DateTime<chrono::Utc>,
        _dry_run: bool,
    ) -> MemcoreResult<usize> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }

    async fn count_events_by_org(&self, _org_id: &str) -> MemcoreResult<usize> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }

    async fn erase_user_audit_content(&self, _tenant: &TenantContext) -> MemcoreResult<usize> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }

    async fn count_user_audit_content(&self, _tenant: &TenantContext) -> MemcoreResult<usize> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }

    async fn list_events_by_org(
        &self,
        _query: memcore_core::ports::OrgMemoryEventQuery,
    ) -> MemcoreResult<Vec<memcore_core::MemoryEvent>> {
        Err(MemcoreError::StorageError(
            "audit store unavailable".to_string(),
        ))
    }
}

#[tokio::test]
async fn add_operation_records_add_event() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "I am learning Rust.".to_string(),
                ..Default::default()
            }],
            metadata: json!({ "session": "abc" }),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let events = event_store
        .list_events(MemoryEventQuery::new(tenant, 10))
        .await
        .expect("list events should succeed");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, MemoryEventOperation::Add);
    assert_eq!(events[0].fact_id, Some(output.memories[0].id));
    assert_eq!(
        events[0].new_content.as_deref(),
        Some("I am learning Rust.")
    );
    assert!(events[0].input_text.is_none());
    assert_eq!(events[0].provider_name.as_deref(), Some("mock"));
}

#[tokio::test]
async fn update_operation_records_update_event() {
    let fact_store = Arc::new(MockFactStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "User prefers Python.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("initial add should succeed");

    let target_id = initial.memories[0].id;
    let engine = engine_with_audit(
        fact_store,
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("User prefers Rust.")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(target_id),
                reason: Some("preference changed".to_string()),
                confidence: 0.95,
            }),
    );

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "User prefers Rust.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("update should succeed");

    let events = event_store
        .list_events(MemoryEventQuery::new(tenant, 10))
        .await
        .expect("list events should succeed");

    let update_event = events
        .iter()
        .find(|event| event.operation == MemoryEventOperation::Update)
        .expect("update event should exist");
    assert_eq!(update_event.fact_id, Some(target_id));
    assert_eq!(
        update_event.previous_content.as_deref(),
        Some("User prefers Python.")
    );
    assert_eq!(
        update_event.new_content.as_deref(),
        Some("User prefers Rust.")
    );
}

#[tokio::test]
async fn delete_operation_records_delete_event() {
    let fact_store = Arc::new(MockFactStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "remove this memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("initial add should succeed");

    let target_id = initial.memories[0].id;
    let engine = engine_with_audit(
        fact_store,
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("remove this memory")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Delete,
                target_fact_id: Some(target_id),
                reason: None,
                confidence: 0.9,
            }),
    );

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "remove this memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("delete lifecycle should succeed");

    let events = event_store
        .list_events(MemoryEventQuery::new(tenant, 10))
        .await
        .expect("list events should succeed");

    let delete_event = events
        .iter()
        .find(|event| event.operation == MemoryEventOperation::Delete)
        .expect("delete event should exist");
    assert_eq!(delete_event.fact_id, Some(target_id));
    assert_eq!(
        delete_event.previous_content.as_deref(),
        Some("remove this memory")
    );
}

#[tokio::test]
async fn noop_operation_records_noop_event() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("skip this memory")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::NoOp,
                target_fact_id: None,
                reason: Some("duplicate".to_string()),
                confidence: 0.9,
            }),
    );

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "skip this memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("noop should succeed");

    let events = event_store
        .list_events(MemoryEventQuery::new(tenant, 10))
        .await
        .expect("list events should succeed");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, MemoryEventOperation::NoOp);
    assert_eq!(events[0].metadata["reason"], "duplicate");
}

#[tokio::test]
async fn forget_user_records_forget_user_event() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "temporary memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    engine
        .forget_user(ForgetUserInput {
            tenant: tenant.clone(),
        })
        .await
        .expect("forget should succeed");

    let events = event_store
        .list_events(MemoryEventQuery::new(tenant, 10))
        .await
        .expect("list events should succeed");

    let forget_event = events
        .iter()
        .find(|event| event.operation == MemoryEventOperation::ForgetUser)
        .expect("forget event should exist");
    assert!(forget_event.fact_id.is_none());
    assert_eq!(forget_event.metadata["deleted"], true);
    // Only content-free tombstones remain for the forgotten user.
    assert!(events.iter().all(MemoryEvent::is_content_free_tombstone));
    // The engine's own tombstone carries no provider or model name.
    assert!(forget_event.provider_name.is_none() && forget_event.model_name.is_none());
}

/// Pre-freeze check: a `forget_user` event with text in a free-form field (metadata,
/// provider / model name) or a fact id is not a tombstone and is erased by forget-user.
#[tokio::test]
async fn forget_user_erases_forged_tombstones_carrying_text() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );
    let target = tenant("org_t", "user_t");
    let other = tenant("org_t", "user_other");
    let forged =
        |t: &TenantContext, fact_id, provider: Option<&str>, model: Option<&str>, metadata| {
            MemoryEvent::new(
                t.org_id.clone(),
                t.user_id.clone(),
                fact_id,
                MemoryEventOperation::ForgetUser,
                None,
                None,
                provider.map(str::to_string),
                model.map(str::to_string),
                metadata,
            )
        };
    for t in [&target, &other] {
        for event in [
            forged(
                t,
                None,
                None,
                None,
                json!({"deleted": true, "note": "SENTINEL-T"}),
            ),
            forged(t, None, None, None, json!({"fact": "SENTINEL-T"})),
            forged(t, None, Some("SENTINEL-T"), None, json!({"deleted": true})),
            forged(t, None, None, Some("SENTINEL-T"), json!({"deleted": true})),
            forged(
                t,
                Some(uuid::Uuid::new_v4()),
                None,
                None,
                json!({"deleted": true}),
            ),
        ] {
            event_store.record_event(t, event).await.unwrap();
        }
    }
    engine
        .forget_user(ForgetUserInput {
            tenant: target.clone(),
        })
        .await
        .expect("forget should succeed");
    let remaining = event_store
        .list_events(MemoryEventQuery::new(target.clone(), 50))
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1, "only the new tombstone: {remaining:?}");
    assert!(remaining[0].is_content_free_tombstone());
    assert!(!format!("{remaining:?}").contains("SENTINEL-T"));
    let others = event_store
        .list_events(MemoryEventQuery::new(other.clone(), 50))
        .await
        .unwrap();
    assert_eq!(others.len(), 5, "the other user's events are untouched");
}

/// Forget-user erases only exactly this (org, user)'s content events.
#[tokio::test]
async fn forget_user_erases_only_the_users_audit_content() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );
    let target = tenant("org_a", "user_a");
    let others = [tenant("org_a", "user_b"), tenant("org_b", "user_a")];
    for t in std::iter::once(&target).chain(others.iter()) {
        engine
            .add_memory(AddMemoryInput {
                tenant: t.clone(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: format!("invented memory of {}/{}", t.org_id, t.user_id),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .expect("add should succeed");
    }
    let before: Vec<usize> = futures_counts(&event_store, &others).await;
    assert!(event_store.count_user_audit_content(&target).await.unwrap() > 0);
    engine
        .forget_user(ForgetUserInput {
            tenant: target.clone(),
        })
        .await
        .expect("forget should succeed");
    assert_eq!(
        event_store.count_user_audit_content(&target).await.unwrap(),
        0
    );
    assert_eq!(futures_counts(&event_store, &others).await, before);
}

async fn futures_counts(store: &MockMemoryEventStore, tenants: &[TenantContext]) -> Vec<usize> {
    let mut out = Vec::new();
    for t in tenants {
        out.push(store.count_user_audit_content(t).await.unwrap());
    }
    out
}

/// A failed audit erasure is a failed forget-user, never a false success.
#[tokio::test]
async fn forget_user_fails_when_audit_erasure_fails() {
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(Arc::new(FailingMemoryEventStore));
    let result = engine
        .forget_user(ForgetUserInput {
            tenant: tenant("org_a", "user_a"),
        })
        .await;
    assert!(
        result.is_err(),
        "audit erasure failure must fail forget-user"
    );
}

#[tokio::test]
async fn event_store_enforces_tenant_isolation_on_list() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant_a = tenant("org_a", "user_a");
    let tenant_b = tenant("org_a", "user_b");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant_a.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "only user a memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let listed_a = event_store
        .list_events(MemoryEventQuery::new(tenant_a, 10))
        .await
        .expect("list should succeed");
    assert_eq!(listed_a.len(), 1);

    let listed_b = event_store
        .list_events(MemoryEventQuery::new(tenant_b, 10))
        .await
        .expect("list should succeed");
    assert!(listed_b.is_empty());
}

#[tokio::test]
async fn listing_events_by_fact_id_works() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "target fact memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let fact_id = output.memories[0].id;
    let mut query = MemoryEventQuery::new(tenant.clone(), 10);
    query.fact_id = Some(fact_id);

    let events = event_store
        .list_events(query)
        .await
        .expect("list should succeed");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].fact_id, Some(fact_id));
}

#[tokio::test]
async fn listing_events_by_operation_works() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "hello memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let mut query = MemoryEventQuery::new(tenant, 10);
    query.operation = Some(MemoryEventOperation::Add);

    let events = event_store
        .list_events(query)
        .await
        .expect("list should succeed");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, MemoryEventOperation::Add);
}

#[tokio::test]
async fn audit_event_failure_does_not_break_main_operation() {
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(Arc::new(FailingMemoryEventStore));

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "still works fine".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("main operation should succeed despite audit failure");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn delete_memory_route_records_delete_event() {
    let event_store = Arc::new(MockMemoryEventStore::new());
    let engine = engine_with_audit(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        event_store.clone(),
        MockLlmProvider::new(),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "delete via route".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed");

    let memory_id = output.memories[0].id;
    engine
        .delete_memory(DeleteMemoryInput {
            tenant: tenant.clone(),
            memory_id,
        })
        .await
        .expect("delete should succeed");

    let mut query = MemoryEventQuery::new(tenant, 10);
    query.operation = Some(MemoryEventOperation::Delete);

    let events = event_store
        .list_events(query)
        .await
        .expect("list should succeed");
    assert!(
        events
            .iter()
            .any(|event| event.metadata.get("source") == Some(&json!("delete_memory")))
    );
}
