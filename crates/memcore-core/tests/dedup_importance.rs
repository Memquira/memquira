use std::sync::Arc;

use chrono::Utc;
use memcore_core::{
    AddMemoryInput, CandidateFact, Fact, FactOperation, FactOperationDecision, FactStore,
    ListMemoriesInput, MemoryEngine, MemoryEventOperation, MemoryEventStore, MemoryMessage,
    MemorySource, MemoryType, MessageRole, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with(
    fact_store: Arc<MockFactStore>,
    llm: MockLlmProvider,
    event_store: Option<Arc<MockMemoryEventStore>>,
) -> MemoryEngine {
    let mut engine = MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(4)),
    );
    if let Some(store) = event_store {
        engine = engine.with_event_store(store);
    }
    engine
}

fn high_importance_candidate(content: &str, memory_type: MemoryType) -> CandidateFact {
    CandidateFact::new(content, memory_type, 0.9, 0.8, None, json!({})).expect("candidate")
}

fn placeholder_message() -> Vec<MemoryMessage> {
    vec![MemoryMessage {
        role: MessageRole::User,
        content: "placeholder extraction message".to_string(),
        ..Default::default()
    }]
}

async fn insert_existing_fact(
    store: &MockFactStore,
    org_id: &str,
    user_id: &str,
    content: &str,
    memory_type: MemoryType,
) -> Fact {
    let now = Utc::now();
    let fact = Fact::new(
        Uuid::new_v4(),
        org_id,
        user_id,
        memory_type,
        content,
        None,
        MemorySource::UserMessage,
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
        .insert_fact(&tenant(org_id, user_id), fact.clone())
        .await
        .expect("insert");
    fact
}

#[tokio::test]
async fn exact_duplicate_is_not_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User is learning Rust.",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "user is learning rust",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "ignored".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);

    let listed = fact_store
        .search_facts(memcore_core::FactSearchQuery::new(
            tenant("org_dedup", "user_a"),
            10,
        ))
        .await
        .expect("search");
    assert_eq!(listed.len(), 1);
}

#[tokio::test]
async fn case_insensitive_duplicate_is_not_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "RUST programming",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "rust programming",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);
}

#[tokio::test]
async fn punctuation_only_difference_is_not_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User is learning Rust",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "User is learning Rust.",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.noop, 1);
    assert_eq!(output.added, 0);
}

#[tokio::test]
async fn high_token_overlap_duplicate_is_not_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "alpha beta gamma delta epsilon zeta eta theta iota",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);
}

#[tokio::test]
async fn distinct_memory_is_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User is learning Rust",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "User enjoys hiking on weekends",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
    assert_eq!(output.noop, 0);
}

#[tokio::test]
async fn duplicate_detection_scoped_by_user_id() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User is learning Rust",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "user is learning rust",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_b"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn duplicate_detection_scoped_by_org_id() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_a",
        "user_a",
        "User is learning Rust",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "user is learning rust",
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_b", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn duplicate_detection_scoped_by_memory_type() {
    let fact_store = Arc::new(MockFactStore::new());
    insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User is learning Rust",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "user is learning rust",
            MemoryType::Profile,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn duplicate_add_becomes_noop_with_audit_event() {
    let fact_store = Arc::new(MockFactStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let existing = insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User is learning Rust.",
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            "user is learning rust",
            MemoryType::Skill,
        )]),
        Some(event_store.clone()),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.noop, 1);

    let events = event_store
        .list_events(memcore_core::MemoryEventQuery::new(
            tenant("org_dedup", "user_a"),
            10,
        ))
        .await
        .expect("list events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, MemoryEventOperation::NoOp);
    assert_eq!(events[0].fact_id, Some(existing.id));
}

#[tokio::test]
async fn update_operation_still_updates_target_fact() {
    let fact_store = Arc::new(MockFactStore::new());
    let existing = insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User prefers Python for scripting",
        MemoryType::Preference,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate(
                "User prefers Rust for scripting",
                MemoryType::Preference,
            )])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(existing.id),
                reason: Some("provider update".to_string()),
                confidence: 0.95,
            }),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.updated, 1);
    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 0);

    let stored = fact_store
        .get_fact(&tenant("org_dedup", "user_a"), existing.id)
        .await
        .expect("get")
        .expect("fact");
    assert_eq!(stored.content, "User prefers Rust for scripting");
}

#[tokio::test]
async fn delete_operation_still_deletes_target_fact() {
    let fact_store = Arc::new(MockFactStore::new());
    let existing = insert_existing_fact(
        &fact_store,
        "org_dedup",
        "user_a",
        "User no longer uses Python",
        MemoryType::Preference,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate(
                "User no longer uses Python for any work",
                MemoryType::Preference,
            )])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Delete,
                target_fact_id: Some(existing.id),
                reason: Some("provider delete".to_string()),
                confidence: 0.95,
            }),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.deleted, 1);
    assert_eq!(output.added, 0);

    assert!(
        fact_store
            .get_fact(&tenant("org_dedup", "user_a"), existing.id)
            .await
            .expect("get")
            .is_none()
    );
}

#[tokio::test]
async fn vague_low_importance_fact_is_skipped() {
    let engine = engine_with(
        Arc::new(MockFactStore::new()),
        MockLlmProvider::new().with_extraction_candidates(vec![
            CandidateFact::new(
                "User said okay.",
                MemoryType::Conversation,
                0.9,
                0.6,
                None,
                json!({}),
            )
            .expect("candidate"),
        ]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);
}

#[tokio::test]
async fn stable_memory_type_boost_allows_borderline_fact() {
    let engine = engine_with(
        Arc::new(MockFactStore::new()),
        MockLlmProvider::new().with_extraction_candidates(vec![
            CandidateFact::new(
                "User prefers Rust for backend services",
                MemoryType::Preference,
                0.9,
                0.52,
                None,
                json!({}),
            )
            .expect("candidate"),
        ]),
        None,
    )
    .with_min_importance(0.55);

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_dedup", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn adding_same_memory_twice_lists_one_memory() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with(fact_store.clone(), MockLlmProvider::new(), None);

    let input = AddMemoryInput {
        tenant: tenant("org_dedup", "user_a"),
        messages: vec![MemoryMessage {
            role: MessageRole::User,
            content: "I am learning Rust.".to_string(),
            ..Default::default()
        }],
        metadata: json!({}),
        session: Default::default(),
    };

    let first = engine.add_memory(input.clone()).await.expect("first add");
    assert_eq!(first.added, 1);

    let second = engine.add_memory(input).await.expect("second add");
    assert_eq!(second.added, 0);
    assert_eq!(second.noop, 1);

    let listed = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant("org_dedup", "user_a"),
            memory_type: None,
            query_text: None,
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list");

    assert_eq!(listed.memories.len(), 1);
}
