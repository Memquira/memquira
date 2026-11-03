use std::sync::Arc;

use memcore_common::MemcoreError;
use memcore_core::{
    AddMemoryInput, BuildContextInput, CandidateFact, DeleteMemoryInput, EMPTY_CONTEXT_MESSAGE,
    EmbeddingDeduplicationConfig, FactOperation, FactOperationDecision, FactStore, ForgetUserInput,
    ListMemoriesInput, MemoryEngine, MemoryMessage, MemoryType, MessageRole, SearchMemoryInput,
    TenantContext, VectorSearchQuery, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant should be valid")
}

fn engine_with_mocks(
    fact_store: Arc<dyn FactStore>,
    vector_store: Arc<dyn VectorStore>,
    llm: MockLlmProvider,
    embedding: MockEmbeddingProvider,
) -> MemoryEngine {
    engine_with_mocks_and_pii(fact_store, vector_store, llm, embedding, false)
}

fn engine_with_mocks_and_pii(
    fact_store: Arc<dyn FactStore>,
    vector_store: Arc<dyn VectorStore>,
    llm: MockLlmProvider,
    embedding: MockEmbeddingProvider,
    enable_pii_redaction: bool,
) -> MemoryEngine {
    MemoryEngine::new(fact_store, vector_store, Arc::new(llm), Arc::new(embedding))
        .with_pii_redaction(enable_pii_redaction)
}

fn default_engine() -> MemoryEngine {
    engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    )
}

#[tokio::test]
async fn add_memory_inserts_extracted_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
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

    assert_eq!(output.added, 1);
    assert_eq!(output.memories.len(), 1);

    let stored = fact_store
        .get_fact(&tenant, output.memories[0].id)
        .await
        .expect("get should succeed")
        .expect("fact should exist");
    assert_eq!(stored.content, "I am learning Rust.");
}

#[tokio::test]
async fn add_memory_creates_vectors_for_inserted_facts() {
    let vector_store = Arc::new(MockVectorStore::new());
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Vector content".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let results = vector_store
        .search_vectors(VectorSearchQuery {
            tenant,
            embedding: vec![1.0, 0.0, 0.0, 0.0],
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("vector search should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact_id, output.memories[0].id);
}

#[tokio::test]
async fn embedding_provider_output_is_stored_in_vector_record() {
    let vector_store = Arc::new(MockVectorStore::new());
    let content = "I am learning Rust.";
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let expected =
        deterministic_embedding(content, 4).expect("deterministic embedding should succeed");
    let results = vector_store
        .search_vectors(VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: expected.clone(),
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("vector search should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact_id, output.memories[0].id);
    assert!(
        results[0].score > 0.99,
        "stored vector should match embedding provider output (score={})",
        results[0].score
    );
    assert_eq!(results[0].content, content);
}

#[tokio::test]
async fn empty_messages_returns_validation_error() {
    let engine = default_engine();
    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("empty messages should fail");

    assert_eq!(
        error,
        MemcoreError::ValidationError("messages cannot be empty".to_string())
    );
}

#[tokio::test]
async fn empty_tenant_org_id_returns_validation_error() {
    let engine = default_engine();
    let error = engine
        .add_memory(AddMemoryInput {
            tenant: TenantContext {
                org_id: "   ".to_string(),
                user_id: "user_a".to_string(),
            },
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "hello memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("empty org_id should fail");

    assert_eq!(
        error,
        MemcoreError::ValidationError("org_id cannot be empty".to_string())
    );
}

#[tokio::test]
async fn empty_tenant_user_id_returns_validation_error() {
    let engine = default_engine();
    let error = engine
        .add_memory(AddMemoryInput {
            tenant: TenantContext {
                org_id: "org_a".to_string(),
                user_id: "".to_string(),
            },
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "hello memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("empty user_id should fail");

    assert_eq!(
        error,
        MemcoreError::ValidationError("user_id cannot be empty".to_string())
    );
}

#[tokio::test]
async fn candidate_below_importance_threshold_is_skipped() {
    let low_importance = CandidateFact::new(
        "low value",
        MemoryType::Conversation,
        0.9,
        0.2,
        None,
        json!({}),
    )
    .expect("candidate should be valid");

    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new().with_extraction_candidates(vec![low_importance]),
        MockEmbeddingProvider::new(4),
    )
    .with_min_importance(0.55);

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "ignored".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);
    assert!(output.memories.is_empty());
}

#[tokio::test]
async fn operation_summary_added_count_is_correct() {
    let custom_a = CandidateFact::new("first", MemoryType::Skill, 0.9, 0.8, None, json!({}))
        .expect("candidate should be valid");
    let custom_b = CandidateFact::new("second", MemoryType::Preference, 0.85, 0.7, None, json!({}))
        .expect("candidate should be valid");

    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new().with_extraction_candidates(vec![custom_a, custom_b]),
        MockEmbeddingProvider::new(4),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "batch".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    assert_eq!(output.added, 2);
    assert_eq!(output.updated, 0);
    assert_eq!(output.deleted, 0);
    assert_eq!(output.noop, 0);
}

#[tokio::test]
async fn configurable_llm_extraction_is_used_without_external_calls() {
    let custom = CandidateFact::new(
        "Configured extraction",
        MemoryType::System,
        0.95,
        0.9,
        None,
        json!({ "source": "test" }),
    )
    .expect("candidate should be valid");

    let llm = MockLlmProvider::new().with_extraction_candidates(vec![custom]);
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm,
        MockEmbeddingProvider::new(4),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "trigger".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    assert_eq!(output.memories[0].content, "Configured extraction");
    assert_eq!(output.memories[0].metadata["source"], "test");
}

#[tokio::test]
async fn search_memory_returns_vector_matches_after_add() {
    let content = "I am learning Rust.";
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let output = engine
        .search_memory(SearchMemoryInput {
            tenant,
            query: content.to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search memory should succeed");

    assert_eq!(output.results.len(), 1);
    assert_eq!(output.results[0].content, content);
    assert!(output.results[0].score > 0.0);
    assert!(output.results[0].score <= 1.0);
    assert!(output.results[0].importance > 0.0);
    assert!(output.results[0].confidence > 0.0);
}

#[tokio::test]
async fn build_context_formats_memories_after_add() {
    let content = "I am learning Rust.";
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let output = engine
        .build_context(BuildContextInput {
            tenant,
            query: content.to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: Default::default(),
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("build context should succeed");

    assert!(output.context.contains("Relevant long-term memories:"));
    assert!(output.context.contains(content));
    assert_eq!(output.memories.len(), 1);
}

#[tokio::test]
async fn build_context_returns_empty_message_when_no_matches() {
    let engine = default_engine();
    let tenant = tenant("org_a", "user_a");

    let output = engine
        .build_context(BuildContextInput {
            tenant,
            query: "nothing stored yet".to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: Default::default(),
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("build context should succeed");

    assert_eq!(output.context, EMPTY_CONTEXT_MESSAGE);
    assert!(output.memories.is_empty());
}

#[tokio::test]
async fn add_memory_stores_redacted_facts_when_redaction_enabled() {
    let engine = engine_with_mocks_and_pii(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
        true,
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Email me at secret@example.com".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    assert!(output.memories[0].content.contains("[REDACTED_EMAIL]"));
    assert!(!output.memories[0].content.contains("secret@example.com"));
}

#[tokio::test]
async fn provider_receives_original_content_when_redaction_disabled() {
    let llm = MockLlmProvider::new();
    let llm_arc = Arc::new(llm);
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm_arc.clone(),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_pii_redaction(false);

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Email me at secret@example.com".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let received = llm_arc.last_extraction_messages();
    assert_eq!(received.len(), 1);
    assert!(received[0].content.contains("secret@example.com"));
}

#[tokio::test]
async fn provider_receives_redacted_content_when_redaction_enabled() {
    let llm = MockLlmProvider::new();
    let llm_arc = Arc::new(llm);
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm_arc.clone(),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_pii_redaction(true);

    let tenant = tenant("org_a", "user_a");
    engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Email me at secret@example.com".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let received = llm_arc.last_extraction_messages();
    assert_eq!(received.len(), 1);
    assert!(received[0].content.contains("[REDACTED_EMAIL]"));
    assert!(!received[0].content.contains("secret@example.com"));
}

#[tokio::test]
async fn list_memories_returns_tenant_scoped_facts() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant_a = tenant("org_a", "user_a");
    let tenant_b = tenant("org_a", "user_b");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant_a.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "only for user_a".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let listed_a = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant_a,
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list should succeed");

    assert_eq!(listed_a.memories.len(), 1);
    assert_eq!(listed_a.memories[0].content, "only for user_a");
    assert!(listed_a.next_cursor.is_none());

    let listed_b = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant_b,
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list should succeed");

    assert!(listed_b.memories.is_empty());
}

#[tokio::test]
async fn delete_memory_soft_deletes_fact_and_removes_vector() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "delete this memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let memory_id = output.memories[0].id;

    let delete_output = engine
        .delete_memory(DeleteMemoryInput {
            tenant: tenant.clone(),
            memory_id,
        })
        .await
        .expect("delete should succeed");

    assert!(delete_output.deleted);

    let fact = fact_store
        .get_fact(&tenant, memory_id)
        .await
        .expect("get should succeed");
    assert!(fact.is_none());

    let listed = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant.clone(),
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list should succeed");
    assert!(listed.memories.is_empty());

    let search = engine
        .search_memory(SearchMemoryInput {
            tenant: tenant.clone(),
            query: "delete this memory".to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search should succeed");
    assert!(search.results.is_empty());

    let vectors = vector_store
        .search_vectors(VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: deterministic_embedding("delete this memory", 4)
                .expect("embedding should succeed"),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("vector search should succeed");
    assert!(vectors.is_empty());
}

#[tokio::test]
async fn delete_memory_returns_not_found_for_missing_id() {
    let engine = default_engine();
    let tenant = tenant("org_a", "user_a");

    let error = engine
        .delete_memory(DeleteMemoryInput {
            tenant,
            memory_id: uuid::Uuid::new_v4(),
        })
        .await
        .expect_err("delete should fail");

    assert_eq!(
        error,
        MemcoreError::NotFound("memory not found".to_string())
    );
}

#[tokio::test]
async fn forget_user_removes_facts_and_vectors_for_tenant_only() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant_a = tenant("org_a", "user_a");
    let tenant_b = tenant("org_a", "user_b");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant_a.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user a memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant_b.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user b memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory should succeed");

    let output = engine
        .forget_user(ForgetUserInput {
            tenant: tenant_a.clone(),
        })
        .await
        .expect("forget should succeed");

    assert!(output.deleted);

    let listed_a = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant_a,
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list should succeed");
    assert!(listed_a.memories.is_empty());

    let listed_b = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant_b,
            memory_type: None,
            query_text: None,
            limit: 20,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list should succeed");
    assert_eq!(listed_b.memories.len(), 1);
}

fn high_importance_candidate(content: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.8, None, json!({}))
        .expect("candidate should be valid")
}

#[tokio::test]
async fn noop_operation_does_not_insert_fact() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("skip this memory")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::NoOp,
                target_fact_id: None,
                reason: Some("duplicate".to_string()),
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let output = engine
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
        .expect("add memory should succeed");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);
    assert!(output.memories.is_empty());

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
        .expect("list should succeed");
    assert!(listed.memories.is_empty());
}

#[tokio::test]
async fn update_operation_updates_existing_fact() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
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
    let updated_candidate = high_importance_candidate("User prefers Rust.");

    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![updated_candidate])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(target_id),
                reason: Some("preference changed".to_string()),
                confidence: 0.95,
            }),
        MockEmbeddingProvider::new(4),
    );

    let output = engine
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

    assert_eq!(output.added, 0);
    assert_eq!(output.updated, 1);
    assert_eq!(output.memories.len(), 1);
    assert_eq!(output.memories[0].content, "User prefers Rust.");

    let stored = fact_store
        .get_fact(&tenant, target_id)
        .await
        .expect("get should succeed")
        .expect("fact should exist");
    assert_eq!(stored.content, "User prefers Rust.");
}

#[tokio::test]
async fn update_operation_updates_vector_embedding() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "old content here".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("initial add should succeed");

    let target_id = initial.memories[0].id;
    let new_content = "new content here";
    let engine = engine_with_mocks(
        fact_store,
        vector_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate(new_content)])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(target_id),
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: new_content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("update should succeed");

    let expected =
        deterministic_embedding(new_content, 4).expect("deterministic embedding should succeed");
    let results = vector_store
        .search_vectors(VectorSearchQuery {
            tenant,
            embedding: expected,
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("vector search should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact_id, target_id);
    assert_eq!(results[0].content, new_content);
    assert!(results[0].score > 0.99);
}

#[tokio::test]
async fn delete_operation_soft_deletes_fact_via_lifecycle() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
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
    let engine = engine_with_mocks(
        fact_store.clone(),
        vector_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("remove this memory")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Delete,
                target_fact_id: Some(target_id),
                reason: Some("obsolete".to_string()),
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let output = engine
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

    assert_eq!(output.deleted, 1);
    assert_eq!(output.added, 0);
    assert!(output.memories.is_empty());

    let fact = fact_store
        .get_fact(&tenant, target_id)
        .await
        .expect("get should succeed");
    assert!(fact.is_none());
}

#[tokio::test]
async fn delete_operation_removes_vector_via_lifecycle() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        vector_store.clone(),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let content = "vector delete target";
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("initial add should succeed");

    let target_id = initial.memories[0].id;
    let engine = engine_with_mocks(
        fact_store,
        vector_store.clone(),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate(content)])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Delete,
                target_fact_id: Some(target_id),
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("delete lifecycle should succeed");

    let embedding =
        deterministic_embedding(content, 4).expect("deterministic embedding should succeed");
    let results = vector_store
        .search_vectors(VectorSearchQuery {
            tenant,
            embedding,
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("vector search should succeed");
    assert!(results.is_empty());
}

#[tokio::test]
async fn update_without_target_fact_id_returns_validation_error() {
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("updated content")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "updated content".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("update without target should fail");

    assert_eq!(error.code(), "validation_error");
    assert!(error.to_string().contains("target_fact_id is required"));
}

#[tokio::test]
async fn delete_without_target_fact_id_returns_validation_error() {
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("deleted content")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Delete,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "deleted content".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("delete without target should fail");

    assert_eq!(error.code(), "validation_error");
    assert!(error.to_string().contains("target_fact_id is required"));
}

#[tokio::test]
async fn update_target_from_another_tenant_returns_not_found() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant_a = tenant("org_a", "user_a");
    let tenant_b = tenant("org_a", "user_b");
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant_a.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user a fact memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("initial add should succeed");

    let target_id = initial.memories[0].id;
    let engine = engine_with_mocks(
        fact_store,
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("user b update")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(target_id),
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant_b,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user b update".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("cross-tenant update should fail");

    assert_eq!(
        error,
        MemcoreError::NotFound("memory not found".to_string())
    );
}

#[tokio::test]
async fn delete_target_from_another_tenant_returns_not_found() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant_a = tenant("org_a", "user_a");
    let tenant_b = tenant("org_a", "user_b");
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant_a.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user a fact memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("initial add should succeed");

    let target_id = initial.memories[0].id;
    let engine = engine_with_mocks(
        fact_store,
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("user b delete")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Delete,
                target_fact_id: Some(target_id),
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant_b,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "user b delete".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("cross-tenant delete should fail");

    assert_eq!(
        error,
        MemcoreError::NotFound("memory not found".to_string())
    );
}

#[tokio::test]
async fn lifecycle_operation_summary_counts_are_correct() {
    let fact_store = Arc::new(MockFactStore::new());
    let engine = engine_with_mocks(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new(),
        MockEmbeddingProvider::new(4),
    );

    let tenant = tenant("org_a", "user_a");
    let initial = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "seed fact memory".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("seed add should succeed");

    let target_id = initial.memories[0].id;
    let engine = engine_with_mocks(
        fact_store,
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![
                high_importance_candidate("updated fact"),
                high_importance_candidate("noop fact"),
                high_importance_candidate("deleted fact"),
            ])
            .with_classification_decisions(vec![
                FactOperationDecision {
                    operation: FactOperation::Update,
                    target_fact_id: Some(target_id),
                    reason: None,
                    confidence: 0.9,
                },
                FactOperationDecision {
                    operation: FactOperation::NoOp,
                    target_fact_id: None,
                    reason: None,
                    confidence: 0.9,
                },
                FactOperationDecision {
                    operation: FactOperation::Delete,
                    target_fact_id: Some(target_id),
                    reason: None,
                    confidence: 0.9,
                },
            ]),
        MockEmbeddingProvider::new(4),
    )
    .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
        enabled: false,
        ..Default::default()
    });

    let output = engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "batch lifecycle".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("lifecycle batch should succeed");

    assert_eq!(output.added, 0);
    assert_eq!(output.updated, 1);
    assert_eq!(output.noop, 1);
    assert_eq!(output.deleted, 1);
}

#[tokio::test]
async fn archive_operation_is_treated_as_noop() {
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("archive me")])
            .with_classification_decision(FactOperationDecision {
                operation: FactOperation::Archive,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            }),
        MockEmbeddingProvider::new(4),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "archive me".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("archive should noop");

    assert_eq!(output.noop, 1);
    assert_eq!(output.added, 0);
}

#[tokio::test]
async fn classification_provider_error_propagates() {
    let engine = engine_with_mocks(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate("fail classify")])
            .with_classification_fail_error(MemcoreError::ProviderError(
                "classification unavailable".to_string(),
            )),
        MockEmbeddingProvider::new(4),
    );

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_a", "user_a"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "fail classify".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("classification failure should propagate");

    assert_eq!(
        error,
        MemcoreError::ProviderError("classification unavailable".to_string())
    );
}
