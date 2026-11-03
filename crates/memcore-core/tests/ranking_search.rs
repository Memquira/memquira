use std::sync::Arc;

use chrono::Utc;
use memcore_core::{
    Fact, FactStore, MemoryEngine, MemorySearchResult, MemorySource, MemoryType, RankingConfig,
    SearchMemoryInput, TenantContext, VectorRecord, VectorStore,
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

async fn insert_scored_fact(
    fact_store: &MockFactStore,
    vector_store: &MockVectorStore,
    org_id: &str,
    user_id: &str,
    content: &str,
    importance: f32,
    confidence: f32,
    updated_at: chrono::DateTime<Utc>,
    memory_type: MemoryType,
    embedding: Vec<f32>,
) -> Fact {
    let fact = Fact::new(
        Uuid::new_v4(),
        org_id,
        user_id,
        memory_type,
        content,
        None,
        MemorySource::UserMessage,
        confidence,
        importance,
        None,
        None,
        updated_at,
        updated_at,
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
async fn search_results_are_sorted_by_score_descending() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query_embedding = deterministic_embedding("ranking search query alpha", 8).expect("embed");

    insert_scored_fact(
        &fact_store,
        &vector_store,
        "org_rank",
        "user_a",
        "first ranked memory alpha bravo",
        0.7,
        0.8,
        Utc::now(),
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.75),
    )
    .await;
    insert_scored_fact(
        &fact_store,
        &vector_store,
        "org_rank",
        "user_a",
        "second ranked memory charlie delta",
        0.9,
        0.85,
        Utc::now(),
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.70),
    )
    .await;
    insert_scored_fact(
        &fact_store,
        &vector_store,
        "org_rank",
        "user_a",
        "third ranked memory echo foxtrot",
        0.5,
        0.6,
        Utc::now(),
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.95),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_rank", "user_a"),
            query: "ranking search query alpha".to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert_eq!(output.results.len(), 3);
    for window in output.results.windows(2) {
        assert!(window[0].score >= window[1].score);
    }
}

#[tokio::test]
async fn search_preserves_tenant_isolation_with_ranking() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let query_embedding = deterministic_embedding("tenant ranking query", 8).expect("embed");

    insert_scored_fact(
        &fact_store,
        &vector_store,
        "org_a",
        "user_a",
        "org a memory alpha bravo",
        0.9,
        0.9,
        Utc::now(),
        MemoryType::Skill,
        embedding_with_cosine_similarity(&query_embedding, 0.95),
    )
    .await;

    let output = engine_with(fact_store, vector_store)
        .search_memory(SearchMemoryInput {
            tenant: tenant("org_b", "user_a"),
            query: "tenant ranking query".to_string(),
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search");

    assert!(output.results.is_empty());
}

#[test]
fn apply_ranking_sorts_in_memory_results() {
    use memcore_core::apply_ranking;

    let now = Utc::now();
    let mut results = vec![
        MemorySearchResult {
            fact_id: Uuid::new_v4(),
            content: "low".to_string(),
            memory_type: MemoryType::Conversation,
            score: 0.9,
            vector_score: None,
            confidence: 0.5,
            importance: 0.5,
            valid_at: None,
            metadata: json!({}),
            structure: Default::default(),
        },
        MemorySearchResult {
            fact_id: Uuid::new_v4(),
            content: "high".to_string(),
            memory_type: MemoryType::Profile,
            score: 0.7,
            vector_score: None,
            confidence: 0.9,
            importance: 0.95,
            valid_at: None,
            metadata: json!({}),
            structure: Default::default(),
        },
    ];

    apply_ranking(&mut results, |_| Some(now), now, &RankingConfig::default());

    assert_eq!(results[0].content, "high");
    assert!(results[0].score >= results[1].score);
}
