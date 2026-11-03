//! Search with one retrieval leg at a time.
//!
//! `Hybrid` must stay exactly the original fused search; `Semantic` runs the vector leg alone and
//! `Fulltext` the ranked full-text leg alone (no embedding call).

use std::sync::Arc;

use chrono::Utc;
use memcore_core::{
    Fact, FactStore, MemoryEngine, MemorySource, MemoryType, RetrievalMode, SearchMemoryInput,
    TenantContext, VectorRecord, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const ORG: &str = "org_modes";
const USER: &str = "user_modes";

fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

async fn add(
    facts: &MockFactStore,
    vectors: &MockVectorStore,
    content: &str,
    with_vector: bool,
) -> Fact {
    let now = Utc::now();
    let fact = Fact::new(
        Uuid::new_v4(),
        ORG,
        USER,
        MemoryType::Preference,
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
    facts.insert_fact(&tenant(), fact.clone()).await.unwrap();
    if with_vector {
        vectors
            .upsert_vector(
                &tenant(),
                VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id: fact.id,
                    org_id: ORG.to_string(),
                    user_id: USER.to_string(),
                    embedding: deterministic_embedding(content, 8).unwrap(),
                    content: content.to_string(),
                    memory_type: MemoryType::Preference,
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
    }
    fact
}

fn input(query: &str) -> SearchMemoryInput {
    SearchMemoryInput {
        tenant: tenant(),
        query: query.to_string(),
        limit: 10,
        memory_types: None,
        metadata_filter: None,
    }
}

async fn setup() -> (MemoryEngine, Fact, Fact) {
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    // Only in the full-text index: no vector, so the semantic leg can never return it.
    let text_only = add(&facts, &vectors, "Priya collects vintage postcards.", false).await;
    // Only in the vector index, sharing no term with the question.
    let vector_only = add(&facts, &vectors, "Enjoys quiet evenings.", true).await;
    let engine = MemoryEngine::new(
        facts,
        vectors,
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    (engine, text_only, vector_only)
}

fn ids(output: &memcore_core::SearchMemoryOutput) -> Vec<Uuid> {
    output.results.iter().map(|result| result.fact_id).collect()
}

#[tokio::test]
async fn hybrid_mode_is_exactly_the_default_search() {
    let (engine, _, _) = setup().await;
    let question = "What does Priya collect?";
    let default = engine.search_memory(input(question)).await.unwrap();
    let hybrid = engine
        .search_memory_with_mode(input(question), RetrievalMode::Hybrid)
        .await
        .unwrap();
    assert_eq!(default, hybrid);
}

#[tokio::test]
async fn fulltext_mode_finds_term_matches_the_substring_leg_misses() {
    let (engine, text_only, _) = setup().await;
    let question = "What does Priya collect?";
    // The substring leg needs the whole question verbatim, and the fact has no vector.
    let hybrid = engine.search_memory(input(question)).await.unwrap();
    assert!(!ids(&hybrid).contains(&text_only.id));

    let fulltext = engine
        .search_memory_with_mode(input(question), RetrievalMode::Fulltext)
        .await
        .unwrap();
    assert_eq!(ids(&fulltext), vec![text_only.id]);
}

#[tokio::test]
async fn semantic_mode_uses_only_the_vector_leg() {
    let (engine, text_only, vector_only) = setup().await;
    let semantic = engine
        .search_memory_with_mode(
            input("Priya collects vintage postcards."),
            RetrievalMode::Semantic,
        )
        .await
        .unwrap();
    assert_eq!(ids(&semantic), vec![vector_only.id]);
    assert!(!ids(&semantic).contains(&text_only.id));
}

#[tokio::test]
async fn fulltext_mode_with_only_stop_words_returns_nothing() {
    let (engine, _, _) = setup().await;
    let output = engine
        .search_memory_with_mode(input("what is it?"), RetrievalMode::Fulltext)
        .await
        .unwrap();
    assert!(output.results.is_empty());
}

#[tokio::test]
async fn rrf_mode_fuses_both_legs() {
    let (engine, text_only, vector_only) = setup().await;
    // The text-only fact matches the question's terms; the vector-only one is the vector leg's
    // only candidate. RRF returns both, which neither single leg does.
    let rrf = engine
        .search_memory_with_mode(input("Priya postcards"), RetrievalMode::Rrf)
        .await
        .unwrap();
    let found = ids(&rrf);
    assert!(found.contains(&text_only.id), "{found:?}");
    assert!(found.contains(&vector_only.id), "{found:?}");
}

#[tokio::test]
async fn a_fact_both_legs_rank_first_leads_the_rrf_list() {
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let both = add(&facts, &vectors, "Omar restores vintage motorbikes.", true).await;
    for filler in ["Likes tea.", "Owns a cat.", "Reads novels."] {
        add(&facts, &vectors, filler, true).await;
    }
    let text_only = add(&facts, &vectors, "Omar sold a motorbike helmet.", false).await;
    let engine = MemoryEngine::new(
        facts,
        vectors,
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    let question = "Omar restores vintage motorbikes.";
    let rrf = engine
        .search_memory_with_mode(input(question), RetrievalMode::Rrf)
        .await
        .unwrap();
    assert_eq!(ids(&rrf).first(), Some(&both.id));
    assert!(ids(&rrf).contains(&text_only.id));
    // Scores are the RRF sums, best first.
    assert!(rrf.results.windows(2).all(|w| w[0].score >= w[1].score));
}

#[tokio::test]
async fn the_configured_default_mode_drives_search_memory() {
    let (engine, text_only, _) = setup().await;
    let engine = engine.with_default_retrieval_mode(RetrievalMode::Fulltext);
    let output = engine
        .search_memory(input("What does Priya collect?"))
        .await
        .unwrap();
    assert_eq!(ids(&output), vec![text_only.id]);
}

#[tokio::test]
async fn rrf_k_changes_scores_not_membership() {
    let (engine, _, _) = setup().await;
    let question = "Priya postcards";
    let k60 = engine
        .search_memory_with_mode(input(question), RetrievalMode::Rrf)
        .await
        .unwrap();
    let engine = engine.with_rrf_config(memcore_core::RrfConfig {
        k: 1.0,
        ..memcore_core::RrfConfig::default()
    });
    let k1 = engine
        .search_memory_with_mode(input(question), RetrievalMode::Rrf)
        .await
        .unwrap();
    let mut a = ids(&k60);
    let mut b = ids(&k1);
    a.sort();
    b.sort();
    assert_eq!(a, b);
    assert!(
        (k1.results[0].score - 1.0 / 2.0).abs() < 1e-6,
        "rank 1 with k = 1 scores 1/2"
    );
}

#[tokio::test]
async fn the_scoped_users_name_is_not_a_fulltext_term_but_hybrid_ignores_names() {
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let kayak = add(&facts, &vectors, "Bought a red kayak.", false).await;
    add(&facts, &vectors, "Priya likes tea.", false).await;
    add(&facts, &vectors, "Priya went out.", false).await;
    let engine = MemoryEngine::new(
        facts,
        vectors,
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    let question = "What kayak did Priya buy?";
    let names = vec!["Priya".to_string()];

    let unscoped = engine
        .search_memory_with_mode(input(question), RetrievalMode::Fulltext)
        .await
        .unwrap();
    assert_eq!(unscoped.results.len(), 3);

    for mode in [RetrievalMode::Fulltext, RetrievalMode::Rrf] {
        let scoped = engine
            .search_memory_with_options(
                input(question),
                memcore_core::SearchOptions {
                    mode: Some(mode),
                    user_names: names.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(ids(&scoped), vec![kayak.id], "{mode:?}");
    }

    let hybrid_named = engine
        .search_memory_with_options(
            input(question),
            memcore_core::SearchOptions {
                mode: Some(RetrievalMode::Hybrid),
                user_names: names,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let hybrid = engine.search_memory(input(question)).await.unwrap();
    assert_eq!(hybrid_named, hybrid);
}

/// Records the candidate count each vector search asks for.
struct CountingVectors {
    inner: MockVectorStore,
    limits: std::sync::Mutex<Vec<usize>>,
}

#[async_trait::async_trait]
impl VectorStore for CountingVectors {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> memcore_common::MemcoreResult<()> {
        self.inner.upsert_vector(tenant, record).await
    }

    async fn search_vectors(
        &self,
        query: memcore_core::VectorSearchQuery,
    ) -> memcore_common::MemcoreResult<Vec<memcore_core::VectorSearchResult>> {
        self.limits.lock().unwrap().push(query.limit);
        self.inner.search_vectors(query).await
    }

    async fn delete_by_fact_id(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> memcore_common::MemcoreResult<()> {
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> memcore_common::MemcoreResult<()> {
        self.inner.delete_by_user(tenant).await
    }
}

async fn engine_with_facts(
    n: usize,
    max_limit: Option<usize>,
) -> (MemoryEngine, Arc<CountingVectors>) {
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(CountingVectors {
        inner: MockVectorStore::new(),
        limits: std::sync::Mutex::new(Vec::new()),
    });
    for index in 0..n {
        let content = format!("Fact number {index} about gardening and tomatoes.");
        let now = Utc::now();
        let fact = Fact::new(
            Uuid::new_v4(),
            ORG,
            USER,
            MemoryType::Preference,
            &content,
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
        .unwrap();
        facts.insert_fact(&tenant(), fact.clone()).await.unwrap();
        vectors
            .upsert_vector(
                &tenant(),
                VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id: fact.id,
                    org_id: ORG.to_string(),
                    user_id: USER.to_string(),
                    embedding: deterministic_embedding(&content, 8).unwrap(),
                    content,
                    memory_type: MemoryType::Preference,
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
    }
    let mut engine = MemoryEngine::new(
        facts,
        vectors.clone(),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    if let Some(max) = max_limit {
        engine = engine
            .with_search_max_limit(max)
            .expect("a maximum within 50..=200");
    }
    (engine, vectors)
}

fn limited(limit: usize) -> SearchMemoryInput {
    SearchMemoryInput {
        limit,
        ..input("tomatoes in the garden")
    }
}

/// With the default maximum, the public limit and candidate pool are exactly as before.
#[tokio::test]
async fn the_default_search_maximum_is_fifty_with_the_old_candidate_pool() {
    let (engine, vectors) = engine_with_facts(120, None).await;
    assert_eq!(engine.search_max_limit(), 50);
    for (limit, pool) in [(10, 20), (30, 50), (50, 50)] {
        let out = engine
            .search_memory_with_mode(limited(limit), RetrievalMode::Semantic)
            .await
            .unwrap();
        assert_eq!(out.results.len(), limit);
        assert_eq!(vectors.limits.lock().unwrap().pop(), Some(pool), "{limit}");
    }
    assert!(
        engine
            .search_memory_with_mode(limited(51), RetrievalMode::Semantic)
            .await
            .is_err()
    );
}

/// A raised maximum admits deeper requests, which fetch exactly `limit` candidates, and leaves
/// requests up to 50 exactly as they were (same pool, same ordered results).
#[tokio::test]
async fn a_raised_search_maximum_is_additive() {
    let (engine, vectors) = engine_with_facts(120, Some(100)).await;
    assert_eq!(engine.search_max_limit(), 100);
    let deep = engine
        .search_memory_with_mode(limited(100), RetrievalMode::Semantic)
        .await
        .unwrap();
    assert_eq!(deep.results.len(), 100);
    assert_eq!(vectors.limits.lock().unwrap().pop(), Some(100));
    assert!(
        engine
            .search_memory_with_mode(limited(101), RetrievalMode::Semantic)
            .await
            .is_err()
    );
    // Limits up to 50 keep the pre-pool, and their results are the leading part of the same
    // semantic order the deep request returns.
    for (limit, pool) in [(30, 50), (50, 50)] {
        let out = engine
            .search_memory_with_mode(limited(limit), RetrievalMode::Semantic)
            .await
            .unwrap();
        assert_eq!(vectors.limits.lock().unwrap().pop(), Some(pool), "{limit}");
        let ids: Vec<Uuid> = out.results.iter().map(|r| r.fact_id).collect();
        let deep_ids: Vec<Uuid> = deep.results.iter().take(limit).map(|r| r.fact_id).collect();
        assert_eq!(ids, deep_ids, "{limit}");
    }
}

/// An out-of-range maximum is rejected, never clamped.
#[tokio::test]
async fn an_out_of_range_search_maximum_is_rejected() {
    for bad in [0, 49, 201, 10_000] {
        let (engine, _) = engine_with_facts(0, None).await;
        assert!(engine.with_search_max_limit(bad).is_err(), "{bad}");
    }
    for good in [50, 100, 200] {
        let (engine, _) = engine_with_facts(0, None).await;
        assert_eq!(
            engine
                .with_search_max_limit(good)
                .unwrap()
                .search_max_limit(),
            good
        );
    }
}

/// Facts for the tie tests: three share one text (so their embeddings, and scores, are equal)
/// and differ only in provenance; two more have distinct texts.
fn tie_fixture() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("Priya adopted a rescue dog.", &["D3:7"]),
        ("Priya adopted a rescue dog.", &["D1:2"]),
        ("Priya adopted a rescue dog.", &["D2:5"]),
        ("Priya walks the dog every morning.", &["D4:1"]),
        ("Omar repairs vintage radios.", &["D5:9"]),
    ]
}

async fn tie_engine(order: &[usize]) -> MemoryEngine {
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let fixture = tie_fixture();
    for &index in order {
        let (content, sources) = fixture[index];
        let now = Utc::now();
        let fact = Fact::new(
            Uuid::new_v4(),
            ORG,
            USER,
            MemoryType::Event,
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
        .unwrap()
        .with_structure(memcore_core::FactStructure {
            source_message_ids: sources.iter().map(|s| s.to_string()).collect(),
            ..memcore_core::FactStructure::default()
        });
        facts.insert_fact(&tenant(), fact.clone()).await.unwrap();
        vectors
            .upsert_vector(
                &tenant(),
                VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id: fact.id,
                    org_id: ORG.to_string(),
                    user_id: USER.to_string(),
                    embedding: deterministic_embedding(content, 8).unwrap(),
                    content: content.to_string(),
                    memory_type: MemoryType::Event,
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
    }
    MemoryEngine::new(
        facts,
        vectors,
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
}

/// The logical order: (content, source ids) of each hit, best first.
async fn logical_order(engine: &MemoryEngine) -> Vec<(String, Vec<String>)> {
    engine
        .search_memory_with_mode(
            input("Priya adopted a rescue dog."),
            RetrievalMode::Semantic,
        )
        .await
        .unwrap()
        .results
        .into_iter()
        .map(|r| (r.content, r.structure.source_message_ids))
        .collect()
}

/// Rebuilding the same logical store (new random fact ids) and inserting in any order give
/// the same logical candidate order; exactly tied hits follow content, then provenance.
#[tokio::test]
async fn tied_vector_hits_have_the_same_logical_order_across_rebuilds_and_insert_orders() {
    let first = logical_order(&tie_engine(&[0, 1, 2, 3, 4]).await).await;
    let rebuilt = logical_order(&tie_engine(&[0, 1, 2, 3, 4]).await).await;
    let reversed = logical_order(&tie_engine(&[4, 3, 2, 1, 0]).await).await;
    let shuffled = logical_order(&tie_engine(&[2, 4, 0, 3, 1]).await).await;
    assert_eq!(first, rebuilt);
    assert_eq!(first, reversed);
    assert_eq!(first, shuffled);
    // The three tied copies lead (identical text to the query) and are ordered by provenance.
    let tied: Vec<Vec<String>> = first
        .iter()
        .filter(|(content, _)| content == "Priya adopted a rescue dog.")
        .map(|(_, sources)| sources.clone())
        .collect();
    assert_eq!(
        tied,
        vec![
            vec!["D1:2".to_string()],
            vec!["D2:5".to_string()],
            vec!["D3:7".to_string()]
        ]
    );
    assert_eq!(first.len(), 5);
}

/// Hits whose scores differ keep the vector store's score order; the tie-break never reorders
/// them.
#[tokio::test]
async fn non_tied_vector_hits_keep_their_score_order() {
    let engine = tie_engine(&[3, 4]).await;
    let query = "Priya walks the dog every morning.";
    let engine_order: Vec<String> = engine
        .search_memory_with_mode(input(query), RetrievalMode::Semantic)
        .await
        .unwrap()
        .results
        .into_iter()
        .map(|r| r.content)
        .collect();
    // Scores from the embeddings themselves, best first.
    let q = deterministic_embedding(query, 8).unwrap();
    let cosine = |text: &str| {
        let v = deterministic_embedding(text, 8).unwrap();
        let dot: f32 = q.iter().zip(&v).map(|(a, b)| a * b).sum();
        let norm = |x: &[f32]| x.iter().map(|a| a * a).sum::<f32>().sqrt();
        dot / (norm(&q) * norm(&v))
    };
    let mut expected = vec![
        "Priya walks the dog every morning.".to_string(),
        "Omar repairs vintage radios.".to_string(),
    ];
    expected.sort_by(|a, b| cosine(b).partial_cmp(&cosine(a)).unwrap());
    assert_ne!(
        cosine(&expected[0]),
        cosine(&expected[1]),
        "fixture must not tie"
    );
    assert_eq!(engine_order, expected);
}

/// `vector_score` passes the vector store's raw similarity through unchanged; `score` stays the
/// strategy's score and the order is untouched. No vector leg, no `vector_score`.
#[tokio::test]
async fn vector_score_is_the_raw_vector_similarity_and_changes_nothing_else() {
    use std::collections::HashMap;

    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    for text in [
        "Omar restores vintage motorbikes.",
        "Enjoys quiet evenings.",
        "Priya collects vintage postcards.",
        "Sam plans a trip to Lisbon.",
    ] {
        add(&facts, &vectors, text, true).await;
    }
    let keyword_only = add(
        &facts,
        &vectors,
        "Priya bakes sourdough bread every week.",
        false,
    )
    .await;
    let engine = MemoryEngine::new(
        facts,
        vectors.clone(),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );

    let question = "vintage motorbikes";
    let raw: HashMap<Uuid, f32> = vectors
        .search_vectors(memcore_core::VectorSearchQuery {
            tenant: tenant(),
            embedding: deterministic_embedding(question, 8).unwrap(),
            limit: 50,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .unwrap()
        .into_iter()
        .map(|hit| (hit.fact_id, hit.score))
        .collect();
    let semantic = engine
        .search_memory_with_mode(input(question), RetrievalMode::Semantic)
        .await
        .unwrap();
    assert_eq!(semantic.results.len(), 4);
    for (index, result) in semantic.results.iter().enumerate() {
        // Exactly the vector store's value: no RRF, normalisation or rescaling.
        assert_eq!(result.vector_score, Some(raw[&result.fact_id]));
        // `score` is still the RRF score of the result's rank (k = 60, weight 1).
        let expected = 1.0 / (60.0 + (index + 1) as f32);
        assert!(
            (result.score - expected).abs() < 1e-7,
            "{index}: {}",
            result.score
        );
    }
    assert!(
        semantic
            .results
            .windows(2)
            .all(|pair| pair[0].vector_score >= pair[1].vector_score)
    );

    let fulltext = engine
        .search_memory_with_mode(input("Priya sourdough bread"), RetrievalMode::Fulltext)
        .await
        .unwrap();
    assert!(!fulltext.results.is_empty());
    assert!(fulltext.results.iter().all(|r| r.vector_score.is_none()));

    let hybrid = engine
        .search_memory(input("Priya bakes sourdough"))
        .await
        .unwrap();
    for result in &hybrid.results {
        assert_eq!(
            result.vector_score.is_none(),
            result.fact_id == keyword_only.id,
            "{}",
            result.content
        );
    }
    assert!(ids(&hybrid).contains(&keyword_only.id));
}
