//! Failure boundaries of one write (`MemoryEngine::add_memory`) with
//! deterministic fault injection. Invented data; no model calls.
//!
//! Consistency contract under test (see `MemoryEngine::compensate_partial_write`):
//! - an error after some candidates were added removes the facts this request added (soft delete +
//!   vector delete + delete event) and returns the original error;
//! - the in-flight candidate whose row was written but whose vector write failed stays as an active
//!   row without a point; a retry of the same fact heals it (exact dedup + vector repair);
//! - a request cancelled mid-way (client disconnect / deadline) is NOT compensated: its earlier
//!   adds remain (documented remaining failure state);
//! - concurrent identical retries store one fact (atomic exact-dedup key).

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider, MemoryEventQuery,
    SummarizationInput, VectorStore,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, Fact, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, MemoryEngine,
    MemoryEventStore, MemoryMessage, MemoryType, MessageRole, TenantContext, VectorRecord,
    VectorSearchQuery, VectorSearchResult,
};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use tokio::sync::Notify;
use uuid::Uuid;

// ------------------------------------------------------------------ providers with fault hooks

struct Embed {
    fail_on: Mutex<Option<(String, MemcoreError)>>,
}

fn vector(text: &str) -> Vec<f32> {
    let mut v = [0.0_f32; 16];
    for (i, b) in text.bytes().enumerate() {
        v[i % 16] += f32::from(b) / 255.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let mut fail = self.fail_on.lock().unwrap();
        if fail.as_ref().is_some_and(|(t, _)| t == text) {
            return Err(fail.take().unwrap().1);
        }
        Ok(vector(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        16
    }
}

struct Llm {
    queue: Mutex<Vec<Vec<CandidateFact>>>,
    /// When set, the classification of this text waits for `release` (to cancel a request mid-way).
    hold_on: Mutex<Option<String>>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut q = self.queue.lock().unwrap();
        let next = if q.is_empty() {
            Vec::new()
        } else {
            q.remove(0)
        };
        if next.is_empty() {
            return Err(MemcoreError::ProviderError(
                "extraction unavailable (injected)".to_string(),
            ));
        }
        Ok(FactExtractionOutput::from_candidates(next))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        let hold =
            self.hold_on.lock().unwrap().as_deref() == Some(input.candidate_fact.content.as_str());
        if hold {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: None,
            confidence: 0.9,
        })
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

/// MockVectorStore that fails the upsert of one chosen fact text once, and tracks which facts have
/// a point.
struct Vectors {
    inner: MockVectorStore,
    fail_text: Mutex<Option<String>>,
    points: Mutex<HashSet<Uuid>>,
    upserts: AtomicUsize,
}

#[async_trait]
impl VectorStore for Vectors {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        self.upserts.fetch_add(1, Ordering::SeqCst);
        {
            let mut fail = self.fail_text.lock().unwrap();
            if fail.as_deref().is_some_and(|t| record.content == t) {
                *fail = None;
                return Err(MemcoreError::StorageError(
                    "vector store unavailable (injected)".to_string(),
                ));
            }
        }
        let id = record.fact_id;
        self.inner.upsert_vector(tenant, record).await?;
        self.points.lock().unwrap().insert(id);
        Ok(())
    }
    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(query).await
    }
    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.points.lock().unwrap().remove(&fact_id);
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }
    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_by_user(tenant).await
    }
    /// Forwarded: memcore's dedup heal (`heal_fact_vector`) repairs a missing vector only when the
    /// store reports a fact's points (Qdrant and the mock do).
    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<memcore_core::ports::VectorPointState>>> {
        self.inner.fact_vector_points(tenant, fact_id).await
    }
}

// ------------------------------------------------------------------ world

struct World {
    facts: Arc<MockFactStore>,
    events: Arc<MockMemoryEventStore>,
    vectors: Arc<Vectors>,
    embed: Arc<Embed>,
    llm: Arc<Llm>,
    engine: Arc<MemoryEngine>,
}

fn tenant() -> TenantContext {
    TenantContext::new("org_v2_pw", "user_pw").unwrap()
}

fn cand(text: &str) -> CandidateFact {
    CandidateFact::new(text, MemoryType::Profile, 0.9, 0.9, None, json!({})).unwrap()
}

const A: &str = "Rin keeps a telescope on the balcony.";
const B: &str = "Rin studies Portuguese every evening.";
const C: &str = "Rin volunteers at the harbour museum.";

impl World {
    fn new() -> Self {
        let facts = Arc::new(MockFactStore::new());
        let events = Arc::new(MockMemoryEventStore::new());
        let vectors = Arc::new(Vectors {
            inner: MockVectorStore::new(),
            fail_text: Mutex::new(None),
            points: Mutex::new(HashSet::new()),
            upserts: AtomicUsize::new(0),
        });
        let embed = Arc::new(Embed {
            fail_on: Mutex::new(None),
        });
        let llm = Arc::new(Llm {
            queue: Mutex::new(Vec::new()),
            hold_on: Mutex::new(None),
            reached: Notify::new(),
            release: Notify::new(),
        });
        let engine = Arc::new(
            MemoryEngine::new(facts.clone(), vectors.clone(), llm.clone(), embed.clone())
                .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
                    enabled: false,
                    ..Default::default()
                })
                .with_event_store(events.clone()),
        );
        Self {
            facts,
            events,
            vectors,
            embed,
            llm,
            engine,
        }
    }

    fn queue(&self, texts: &[&str]) {
        self.llm
            .queue
            .lock()
            .unwrap()
            .push(texts.iter().map(|t| cand(t)).collect());
    }

    async fn add(&self) -> MemcoreResult<memcore_core::AddMemoryOutput> {
        self.engine
            .add_memory(AddMemoryInput {
                tenant: tenant(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "Rin's week.".to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
    }

    async fn query(&self, include_deleted: bool) -> Vec<Fact> {
        self.facts
            .search_facts(FactSearchQuery {
                tenant: tenant(),
                memory_types: None,
                query_text: None,
                limit: 100,
                cursor: None,
                include_deleted,
            })
            .await
            .unwrap()
    }

    /// Every stored row, deleted ones included (the mock keeps deletions apart from the row).
    async fn all(&self) -> Vec<Fact> {
        self.query(true).await
    }

    /// Active facts: returned by the live query (not deleted) and not superseded / invalidated.
    async fn live(&self) -> Vec<Fact> {
        self.query(false)
            .await
            .into_iter()
            .filter(|f| f.is_active())
            .collect()
    }

    async fn active(&self) -> Vec<String> {
        let mut v: Vec<String> = self.live().await.into_iter().map(|f| f.content).collect();
        v.sort();
        v
    }

    async fn compensation_events(&self) -> usize {
        self.events
            .list_events(MemoryEventQuery::new(tenant(), 100))
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                e.metadata.get("reason").and_then(|r| r.as_str())
                    == Some("partial_write_compensation")
            })
            .count()
    }

    fn has_point(&self, f: &Fact) -> bool {
        self.vectors.points.lock().unwrap().contains(&f.id)
    }
}

// ------------------------------------------------------------------ boundaries

#[tokio::test]
async fn failure_before_any_write_stores_nothing() {
    let w = World::new();
    // no queued extraction: the extractor fails before any candidate
    assert!(w.add().await.is_err());
    assert!(w.all().await.is_empty());
    assert_eq!(w.vectors.upserts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failure_after_earlier_adds_is_compensated_and_a_retry_stores_each_fact_once() {
    let w = World::new();
    w.queue(&[A, B, C]);
    *w.embed.fail_on.lock().unwrap() = Some((
        C.to_string(),
        MemcoreError::ProviderError("connection reset (injected)".into()),
    ));
    let err = w.add().await.expect_err("the failure is reported");
    assert!(
        matches!(err, MemcoreError::ProviderError(_)),
        "the original error is returned: {err:?}"
    );
    assert!(
        w.active().await.is_empty(),
        "the facts this request added were removed"
    );
    let rows = w.all().await;
    assert_eq!(rows.len(), 2, "A and B remain as deleted audit rows");
    assert!(
        w.live().await.is_empty() && rows.iter().all(|f| !w.has_point(f)),
        "no active row, no point"
    );
    assert_eq!(
        w.compensation_events().await,
        2,
        "each compensated fact has a delete event"
    );
    // retry: everything once
    w.queue(&[A, B, C]);
    w.add().await.expect("retry succeeds");
    let mut want = vec![A.to_string(), B.to_string(), C.to_string()];
    want.sort();
    assert_eq!(w.active().await, want);
    assert!(
        w.live().await.iter().all(|f| w.has_point(f)),
        "every active fact has its point"
    );
}

#[tokio::test]
async fn a_timeout_is_compensated_like_any_error() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.embed.fail_on.lock().unwrap() = Some((
        B.to_string(),
        MemcoreError::Timeout("provider operation timed out".into()),
    ));
    assert!(matches!(w.add().await, Err(MemcoreError::Timeout(_))));
    assert!(w.active().await.is_empty());
}

#[tokio::test]
async fn failure_between_row_and_vector_leaves_one_unindexed_row_that_a_retry_heals() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.vectors.fail_text.lock().unwrap() = Some(B.to_string());
    assert!(w.add().await.is_err());
    // A (completed) is compensated; B's row was written, its vector failed (the in-flight candidate)
    let active = w.live().await;
    assert_eq!(
        active
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>(),
        vec![B],
        "only the in-flight row stays"
    );
    assert!(
        !w.has_point(&active[0]),
        "it has no point (documented remaining state)"
    );
    // retry heals: B is an exact duplicate whose vector is repaired; A is added again
    w.queue(&[A, B]);
    w.add().await.expect("retry succeeds");
    let active = w.live().await;
    assert_eq!(active.len(), 2, "A and B once each");
    assert!(
        active.iter().all(|f| w.has_point(f)),
        "both indexed after the retry"
    );
}

#[tokio::test]
async fn concurrent_identical_retries_store_one_fact() {
    let w = World::new();
    w.queue(&[A]);
    w.queue(&[A]);
    let (x, y) = tokio::join!(w.add(), w.add());
    assert!(x.is_ok() && y.is_ok());
    assert_eq!(w.active().await, vec![A.to_string()]);
}

#[tokio::test]
async fn a_cancelled_request_is_not_compensated_documented_remaining_state() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let engine = w.engine.clone();
    let task = tokio::spawn(async move {
        engine
            .add_memory(AddMemoryInput {
                tenant: tenant(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "Rin's week.".to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
    });
    w.llm.reached.notified().await; // A is stored; B's classification is in flight
    task.abort(); // client disconnect / deadline: the future is dropped
    let _ = task.await;
    assert_eq!(
        w.active().await,
        vec![A.to_string()],
        "a dropped request keeps its earlier adds (not compensated)"
    );
    // a retry of the same request stores each fact once (A is an exact duplicate)
    *w.llm.hold_on.lock().unwrap() = None;
    w.queue(&[A, B]);
    w.add().await.expect("retry");
    assert_eq!(w.active().await.len(), 2);
}
