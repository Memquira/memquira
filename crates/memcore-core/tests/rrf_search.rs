use std::sync::Arc;

use chrono::Utc;
use memcore_core::{
    BuildContextInput, Fact, FactStore, MemoryEngine, MemorySource, MemoryType, SearchMemoryInput,
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

fn embedding_with_cosine_similarity(reference: &[f32], similarity: f32) -> Vec<f32> {
    let dim = reference.len();
    let ref_norm: f32 = reference.iter().map(|x| x * x).sum::<f32>().sqrt();
    let ref_unit: Vec<f32> = reference.iter().map(|x| x / ref_norm).collect();

    let mut orthogonal = vec![0.0_f32; dim];
    orthogonal[dim.saturating_sub(1)] = 1.0;
    let proj: f32 = orthogonal
        .iter()
        .zip(ref_unit.iter())
        .map(|(a, b)| a * b)
        .sum();
    for (o, r) in orthogonal.iter_mut().zip(ref_unit.iter()) {
        *o -= proj * r;
    }
    let orth_norm: f32 = orthogonal.iter().map(|x| x * x).sum::<f32>().sqrt();
    if orth_norm > 0.0 {
        for v in &mut orthogonal {
            *v /= orth_norm;
        }
    }

    let sin_theta = (1.0 - similarity * similarity).max(0.0).sqrt();
    ref_unit
        .iter()
        .zip(orthogonal.iter())
        .map(|(r, o)| similarity * r + sin_theta * o)
        .collect()
}

async fn insert_fact_only(
    fact_store: &MockFactStore,
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

    fact_store
        .insert_fact(&tenant(org_id, user_id), fact.clone())
        .await
        .expect("insert fact");

    fact
}

async fn insert_scored_fact_with_vector(
    fact_store: &MockFactStore,
    vector_store: &MockVectorStore,
    org_id: &str,
    user_id: &str,
    content: &str,
    memory_type: MemoryType,
    embedding: Vec<f32>,
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

    fact_store
        .insert_fact(&tenant(org_id, user_id), fact.clone())
        .await
        .expect("insert fact");

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
                memory_type,
                metadata: json!({}),
            },
        )
        .await
        .expect("upsert vector");

    fact
}

#[tokio::test]
async fn semantic_only_match_appears_in_search() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "semantic only retrieval query zulu";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    let fact = insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "unrelated content without keyword overlap alpha bravo",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.95),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert_eq!(output.results.len(), 1);
    assert_eq!(output.results[0].fact_id, fact.id);
}

#[tokio::test]
async fn keyword_only_match_appears_in_search() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());

    let fact = insert_fact_only(
        &fact_store,
        "org_rrf",
        "user_a",
        "keyword only match UNIQUE_PHRASE_XYZ123 for rrf test",
        MemoryType::Skill,
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: "UNIQUE_PHRASE_XYZ123".to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert_eq!(output.results.len(), 1);
    assert_eq!(output.results[0].fact_id, fact.id);
}

#[tokio::test]
async fn fact_in_both_semantic_and_keyword_lists_ranks_higher() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "dual source overlap query tango";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    let dual = insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "dual source overlap query tango memory content",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.99),
    )
    .await;

    insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "semantic only unrelated bravo charlie delta",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.85),
    )
    .await;

    insert_fact_only(
        &fact_store,
        "org_rrf",
        "user_a",
        "dual source overlap query tango keyword only echo",
        MemoryType::Skill,
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert!(output.results.len() >= 2);
    assert_eq!(output.results[0].fact_id, dual.id);
    assert!(output.results[0].score > output.results[1].score);
}

#[tokio::test]
async fn duplicate_fact_appears_only_once_in_results() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "duplicate once query victor";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    let fact = insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "duplicate once query victor memory",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.95),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    let matches: Vec<_> = output
        .results
        .iter()
        .filter(|result| result.fact_id == fact.id)
        .collect();
    assert_eq!(matches.len(), 1);
}

#[tokio::test]
async fn search_respects_requested_limit() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "limit test query whiskey";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    for index in 0..5 {
        insert_scored_fact_with_vector(
            &fact_store,
            &vector_store,
            "org_rrf",
            "user_a",
            &format!("limit test query whiskey memory {index}"),
            MemoryType::Skill,
            embedding_with_cosine_similarity(&query_embedding, 0.9 - index as f32 * 0.05),
        )
        .await;
    }

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            limit: 2,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert_eq!(output.results.len(), 2);
}

#[tokio::test]
async fn search_preserves_tenant_isolation_with_rrf() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "tenant rrf query xray";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_a",
        "user_a",
        "tenant rrf query xray org a memory",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.95),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_b", "user_a"),
            query: query.to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert!(output.results.is_empty());
}

#[tokio::test]
async fn memory_type_filter_still_applies_to_rrf_search() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "memory type filter query yankee";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "memory type filter query yankee conversation",
        MemoryType::Conversation,
        embedding_with_cosine_similarity(&query_embedding, 0.99),
    )
    .await;

    let skill = insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "memory type filter query yankee skill",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.85),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            limit: 10,
            memory_types: Some(vec![MemoryType::Skill]),
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert_eq!(output.results.len(), 1);
    assert_eq!(output.results[0].fact_id, skill.id);
}

#[tokio::test]
async fn deleted_facts_are_excluded_from_rrf_search() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "deleted fact query zulu";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    let fact = insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "deleted fact query zulu memory",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.95),
    )
    .await;

    fact_store
        .soft_delete_fact(&tenant("org_rrf", "user_a"), fact.id)
        .await
        .expect("delete");

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert!(output.results.is_empty());
}

#[tokio::test]
async fn context_uses_fused_ranking_order() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query = "context fused query alpha";
    let query_embedding = deterministic_embedding(query, 8).expect("embed");

    insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "context fused query alpha lower ranked memory",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.88),
    )
    .await;

    insert_scored_fact_with_vector(
        &fact_store,
        &vector_store,
        "org_rrf",
        "user_a",
        "context fused query alpha HIGHER_FUSED_MEMORY",
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.99),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .build_context(BuildContextInput {
            tenant: tenant("org_rrf", "user_a"),
            query: query.to_string(),
            max_memories: 10,
            memory_types: None,
            include_metadata: false,
            budget: Default::default(),
            format_options: Default::default(),
            ..Default::default()
        })
        .await
        .expect("build context");

    let higher_pos = output
        .context
        .find("HIGHER_FUSED_MEMORY")
        .expect("higher in context");
    let lower_pos = output
        .context
        .find("lower ranked memory")
        .expect("lower in context");
    assert!(higher_pos < lower_pos);
    assert!(output.memories[0].content.contains("HIGHER_FUSED_MEMORY"));
}
