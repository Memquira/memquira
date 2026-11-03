use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::MemcoreResult;
use memcore_core::{
    AddMemoryInput, CandidateFact, Fact, FactOperation, FactOperationDecision, FactStore,
    MemoryEngine, MemoryEventOperation, MemoryEventStore, MemoryMessage, MemorySource, MemoryType,
    MessageRole, TenantContext, VectorRecord, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

/// Candidate text used for embedding-duplicate scenarios (distinct from stored fact content).
const EMBEDDING_CANDIDATE: &str = "User enjoys outdoor recreation activities in nature on weekends";

/// Fact content that passes text dedup but shares embedding with `EMBEDDING_CANDIDATE` via vector store.
const EMBEDDING_EXISTING_FACT_CONTENT: &str =
    "Quantum physics research involves complex mathematical models";

/// Pair with low embedding similarity (well below default 0.92 threshold).
const BELOW_THRESHOLD_EXISTING: &str = "alpha bravo charlie delta echo foxtrot golf hotel india";
const BELOW_THRESHOLD_CANDIDATE: &str = "juliet kilo lima mike november oscar papa quebec romeo";

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).expect("tenant")
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

fn engine_with(
    fact_store: Arc<MockFactStore>,
    vector_store: Arc<MockVectorStore>,
    llm: MockLlmProvider,
    event_store: Option<Arc<MockMemoryEventStore>>,
) -> MemoryEngine {
    let mut engine = MemoryEngine::new(
        fact_store,
        vector_store,
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(4)),
    );
    if let Some(store) = event_store {
        engine = engine.with_event_store(store);
    }
    engine
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

async fn insert_fact_with_vector_embedding(
    fact_store: &MockFactStore,
    vector_store: &MockVectorStore,
    org_id: &str,
    user_id: &str,
    fact_content: &str,
    embedding: Vec<f32>,
    memory_type: MemoryType,
) -> Fact {
    let fact = insert_existing_fact(fact_store, org_id, user_id, fact_content, memory_type).await;
    vector_store
        .upsert_vector(
            &tenant(org_id, user_id),
            VectorRecord {
                id: Uuid::new_v4(),
                fact_id: fact.id,
                org_id: org_id.to_string(),
                user_id: user_id.to_string(),
                embedding,
                content: fact_content.to_string(),
                memory_type,
                metadata: json!({}),
            },
        )
        .await
        .expect("upsert vector");
    fact
}

async fn insert_fact_with_embedding_vector(
    fact_store: &MockFactStore,
    vector_store: &MockVectorStore,
    org_id: &str,
    user_id: &str,
    fact_content: &str,
    embedding_source: &str,
    memory_type: MemoryType,
) -> Fact {
    let embedding = deterministic_embedding(embedding_source, 4).expect("embedding");
    insert_fact_with_vector_embedding(
        fact_store,
        vector_store,
        org_id,
        user_id,
        fact_content,
        embedding,
        memory_type,
    )
    .await
}

/// Builds a unit vector with the given cosine similarity to `reference`.
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

struct CountingEmbeddingProvider {
    inner: MockEmbeddingProvider,
    calls: Arc<AtomicUsize>,
}

impl CountingEmbeddingProvider {
    fn new(dimensions: usize) -> Self {
        Self {
            inner: MockEmbeddingProvider::new(dimensions),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl memcore_core::EmbeddingProvider for CountingEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.embed_text(text).await
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.embed_batch(texts).await
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

#[tokio::test]
async fn embedding_duplicate_is_not_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    insert_fact_with_embedding_vector(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        EMBEDDING_CANDIDATE,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            EMBEDDING_CANDIDATE,
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 0);
    assert_eq!(output.noop, 1);

    let listed = fact_store
        .search_facts(memcore_core::FactSearchQuery::new(
            tenant("org_embed", "user_a"),
            10,
        ))
        .await
        .expect("search");
    assert_eq!(listed.len(), 1);
}

#[tokio::test]
async fn embedding_duplicate_increments_noop_count() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    insert_fact_with_embedding_vector(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        EMBEDDING_CANDIDATE,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store,
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            EMBEDDING_CANDIDATE,
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
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
async fn embedding_duplicate_records_noop_audit_event() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let event_store = Arc::new(MockMemoryEventStore::new());
    let existing = insert_fact_with_embedding_vector(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        EMBEDDING_CANDIDATE,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store,
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            EMBEDDING_CANDIDATE,
            MemoryType::Skill,
        )]),
        Some(event_store.clone()),
    );

    engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    let events = event_store
        .list_events(memcore_core::MemoryEventQuery::new(
            tenant("org_embed", "user_a"),
            10,
        ))
        .await
        .expect("list events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, MemoryEventOperation::NoOp);
    assert_eq!(events[0].fact_id, Some(existing.id));
    assert!(
        events[0]
            .metadata
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("embedding similarity")
    );
}

#[tokio::test]
async fn embedding_distinct_memory_is_inserted() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let candidate_content = "Enterprise quarterly budget review requires finance approval";
    let candidate_emb = deterministic_embedding(candidate_content, 4).expect("embed");
    let stored_emb = embedding_with_cosine_similarity(&candidate_emb, 0.5);
    insert_fact_with_vector_embedding(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        stored_emb,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            candidate_content,
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
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
async fn embedding_deduplication_is_tenant_scoped() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    insert_fact_with_embedding_vector(
        &fact_store,
        &vector_store,
        "org_a",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        EMBEDDING_CANDIDATE,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            EMBEDDING_CANDIDATE,
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
async fn embedding_deduplication_is_user_scoped() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    insert_fact_with_embedding_vector(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        EMBEDDING_CANDIDATE,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            EMBEDDING_CANDIDATE,
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_b"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn embedding_deduplication_respects_memory_type_filter() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    insert_fact_with_embedding_vector(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        EMBEDDING_EXISTING_FACT_CONTENT,
        EMBEDDING_CANDIDATE,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            EMBEDDING_CANDIDATE,
            MemoryType::Profile,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn embedding_below_threshold_does_not_block_insert() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let candidate_emb = deterministic_embedding(BELOW_THRESHOLD_CANDIDATE, 4).expect("embed");
    let stored_emb = embedding_with_cosine_similarity(&candidate_emb, 0.85);
    assert!(
        cosine_similarity(&stored_emb, &candidate_emb) < 0.92,
        "test setup should be below default threshold"
    );
    insert_fact_with_vector_embedding(
        &fact_store,
        &vector_store,
        "org_embed",
        "user_a",
        BELOW_THRESHOLD_EXISTING,
        stored_emb,
        MemoryType::Skill,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        vector_store,
        MockLlmProvider::new().with_extraction_candidates(vec![high_importance_candidate(
            BELOW_THRESHOLD_CANDIDATE,
            MemoryType::Skill,
        )]),
        None,
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
}

#[tokio::test]
async fn candidate_embedding_is_reused_for_insert() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let embedding_provider = Arc::new(CountingEmbeddingProvider::new(4));

    let engine = MemoryEngine::new(
        fact_store.clone(),
        vector_store.clone(),
        Arc::new(MockLlmProvider::new().with_extraction_candidates(vec![
            high_importance_candidate("Brand new unique memory content xyz", MemoryType::Skill),
        ])),
        embedding_provider.clone(),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.added, 1);
    assert_eq!(
        embedding_provider.call_count(),
        1,
        "embedding should be generated once for dedup and reused on insert"
    );
}

#[tokio::test]
async fn lifecycle_update_still_works_with_embedding_dedup_enabled() {
    let fact_store = Arc::new(MockFactStore::new());
    let existing = insert_existing_fact(
        &fact_store,
        "org_embed",
        "user_a",
        "User prefers Python for scripting tasks",
        MemoryType::Preference,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        MockLlmProvider::new()
            .with_extraction_candidates(vec![high_importance_candidate(
                "User prefers Rust for scripting tasks",
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
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.updated, 1);
    assert_eq!(output.noop, 0);
}

#[tokio::test]
async fn lifecycle_delete_still_works_with_embedding_dedup_enabled() {
    let fact_store = Arc::new(MockFactStore::new());
    let existing = insert_existing_fact(
        &fact_store,
        "org_embed",
        "user_a",
        "User no longer uses Python daily",
        MemoryType::Preference,
    )
    .await;

    let engine = engine_with(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
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
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.deleted, 1);
    assert_eq!(output.added, 0);
}

#[tokio::test]
async fn low_importance_skip_still_works_before_embedding_dedup() {
    let embedding_provider = Arc::new(CountingEmbeddingProvider::new(4));
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new().with_extraction_candidates(vec![
            CandidateFact::new(
                "User said okay.",
                MemoryType::Conversation,
                0.9,
                0.6,
                None,
                json!({}),
            )
            .expect("candidate"),
        ])),
        embedding_provider.clone(),
    );

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    assert_eq!(output.noop, 1);
    assert_eq!(output.added, 0);
    assert_eq!(
        embedding_provider.call_count(),
        0,
        "low-importance skip should occur before embedding generation"
    );
}

#[tokio::test]
async fn embedding_dedup_failure_returns_error_without_insert() {
    use memcore_common::MemcoreError;

    let fact_store = Arc::new(MockFactStore::new());
    let engine = MemoryEngine::new(
        fact_store.clone(),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new().with_extraction_candidates(vec![
            high_importance_candidate("Some new memory content", MemoryType::Skill),
        ])),
        Arc::new(
            MockEmbeddingProvider::new(4).with_fail_error(MemcoreError::ProviderError(
                "embedding unavailable".to_string(),
            )),
        ),
    );

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant("org_embed", "user_a"),
            messages: placeholder_message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("should fail");

    assert!(matches!(error, MemcoreError::ProviderError(_)));

    let listed = fact_store
        .search_facts(memcore_core::FactSearchQuery::new(
            tenant("org_embed", "user_a"),
            10,
        ))
        .await
        .expect("search");
    assert!(listed.is_empty());
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}
