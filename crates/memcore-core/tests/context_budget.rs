use std::sync::Arc;

use memcore_core::{
    BuildContextInput, ContextBudget, Fact, FactStore, MemoryEngine, MemorySource, MemoryType,
    TenantContext, VectorRecord, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
}

fn engine_with(fact_store: Arc<MockFactStore>, vector_store: Arc<MockVectorStore>) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        vector_store,
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
}

async fn insert_memory(
    fact_store: &MockFactStore,
    vector_store: &MockVectorStore,
    org_id: &str,
    user_id: &str,
    content: &str,
) -> Fact {
    let now = chrono::Utc::now();
    let fact = Fact::new(
        Uuid::new_v4(),
        org_id,
        user_id,
        MemoryType::Skill,
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

    fact_store
        .insert_fact(&tenant(org_id, user_id), fact.clone())
        .await
        .expect("insert");

    let embedding = deterministic_embedding(content, 8).expect("embed");
    vector_store
        .upsert_vector(
            &tenant(org_id, user_id),
            VectorRecord {
                id: Uuid::new_v4(),
                fact_id: fact.id,
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                embedding,
                content: content.to_string(),
                memory_type: MemoryType::Skill,
                metadata: json!({}),
            },
        )
        .await
        .expect("vector");

    fact
}

#[tokio::test]
async fn default_budget_is_applied() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let content = "budget default test memory alpha bravo";
    insert_memory(&fact_store, &vector_store, "org_budget", "user_a", content).await;

    let output = engine_with(fact_store, vector_store)
        .build_context(BuildContextInput {
            tenant: tenant("org_budget", "user_a"),
            query: content.to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: ContextBudget::default(),
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("context");

    assert_eq!(output.budget.max_tokens, 2000);
    assert_eq!(output.budget.reserved_tokens, 300);
    assert_eq!(output.budget.available_tokens, 1700);
    assert_eq!(output.budget.included_memories, 1);
}

#[tokio::test]
async fn tight_budget_skips_oversized_memories() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let long_content = format!("{} {}", "very long memory ".repeat(300), "budget skip test");
    let short_content = "budget skip test tiny memory";

    insert_memory(
        &fact_store,
        &vector_store,
        "org_budget",
        "user_a",
        &long_content,
    )
    .await;
    insert_memory(
        &fact_store,
        &vector_store,
        "org_budget",
        "user_a",
        short_content,
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .build_context(BuildContextInput {
            tenant: tenant("org_budget", "user_a"),
            query: "budget skip test".to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: ContextBudget {
                max_tokens: 120,
                reserved_tokens: 20,
            },
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("context");

    assert_eq!(output.budget.included_memories, 1);
    assert_eq!(output.budget.skipped_memories, 1);
    assert_eq!(output.memories.len(), 1);
    assert_eq!(output.memories[0].content, short_content);
}

#[tokio::test]
async fn invalid_reserved_gte_max_is_rejected() {
    let engine = engine_with(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
    );

    let result = engine
        .build_context(BuildContextInput {
            tenant: tenant("org_budget", "user_a"),
            query: "test".to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: ContextBudget {
                max_tokens: 100,
                reserved_tokens: 100,
            },
            format_options: Default::default(),
            ..Default::default()
        })
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn used_tokens_never_exceed_available_tokens() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());

    for index in 0..5 {
        insert_memory(
            &fact_store,
            &vector_store,
            "org_budget",
            "user_a",
            &format!("budget used tokens memory {index} alpha bravo"),
        )
        .await;
    }

    let output = engine_with(fact_store, vector_store)
        .build_context(BuildContextInput {
            tenant: tenant("org_budget", "user_a"),
            query: "budget used tokens".to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: ContextBudget {
                max_tokens: 150,
                reserved_tokens: 30,
            },
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("context");

    assert!(output.budget.used_tokens <= output.budget.available_tokens);
}

#[tokio::test]
async fn tenant_isolation_preserved_for_context_budget() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let content = "tenant budget isolation memory alpha";

    insert_memory(&fact_store, &vector_store, "org_a", "user_a", content).await;

    let output = engine_with(fact_store, vector_store)
        .build_context(BuildContextInput {
            tenant: tenant("org_b", "user_a"),
            query: content.to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: ContextBudget::default(),
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("context");

    assert_eq!(output.budget.included_memories, 0);
    assert!(output.memories.is_empty());
}
