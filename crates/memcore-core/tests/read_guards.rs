//! SQL-authoritative fact search (R1), SQL-authoritative
//! embedding dedup (R2) and the bounded rejection counter (R6), on the in-process mocks.
//!
//! Case ids (R1-A..D, R2-A..D, E1..E3; R1-E is `read_path_parity.rs`). Every fault is injected by a wrapper
//! that records whether it fired (rule X1); other tenants' stores are compared before and after
//! every tenant-A case (X2); metric keys and error texts are scanned for every canary (X3).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::candidates::SearchCandidatesInput;
use memcore_core::d76_context::D76ContextInput;
use memcore_core::observability::{
    VECTOR_HIT_PATHS, VECTOR_HIT_REJECT_REASONS, VECTOR_HIT_REJECTED_METRIC,
};
use memcore_core::ports::{
    FactFulltextQuery, OrgUserListQuery, OrgUserSummary, RetentionPruneResult, ScoredFact,
    StoredFactState, VectorPointState,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, EpisodeComponents, Fact, FactSearchQuery,
    FactStore, MemoryEngine, MemoryMessage, MemoryType, MessageRole, RetrievalMode,
    SearchMemoryInput, SearchOptions, TenantContext, VectorRecord, VectorSearchQuery,
    VectorSearchResult, VectorStore, VectorWriteIntent,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use metrics::{
    Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 32;
const TENANTS: [(&str, &str); 3] = [("org_a", "u1"), ("org_a", "u2"), ("org_b", "u1")];

// ---------------------------------------------------------------------------------------------
// Metrics capture (thread-local recorder; `#[tokio::test]` runs on one thread)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Captured {
    counters: Arc<Mutex<BTreeMap<String, u64>>>,
    other_keys: Arc<Mutex<BTreeSet<String>>>,
}

impl Captured {
    fn get(&self, path: &str, reason: &str) -> u64 {
        let key = format!("{VECTOR_HIT_REJECTED_METRIC}{{path={path},reason={reason}}}");
        self.counters
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    fn rejection_keys(&self) -> BTreeSet<String> {
        self.counters
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(VECTOR_HIT_REJECTED_METRIC))
            .cloned()
            .collect()
    }

    fn all_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.counters.lock().unwrap().keys().cloned().collect();
        keys.extend(self.other_keys.lock().unwrap().iter().cloned());
        keys
    }
}

fn key_string(key: &Key) -> String {
    let mut labels: Vec<String> = key
        .labels()
        .map(|label| format!("{}={}", label.key(), label.value()))
        .collect();
    labels.sort();
    format!("{}{{{}}}", key.name(), labels.join(","))
}

struct CounterHandle {
    key: String,
    captured: Captured,
}

impl CounterFn for CounterHandle {
    fn increment(&self, value: u64) {
        *self
            .captured
            .counters
            .lock()
            .unwrap()
            .entry(self.key.clone())
            .or_default() += value;
    }

    fn absolute(&self, value: u64) {
        self.captured
            .counters
            .lock()
            .unwrap()
            .insert(self.key.clone(), value);
    }
}

struct TestRecorder(Captured);

impl Recorder for TestRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(Arc::new(CounterHandle {
            key: key_string(key),
            captured: self.0.clone(),
        }))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        self.0.other_keys.lock().unwrap().insert(key_string(key));
        Gauge::noop()
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        self.0.other_keys.lock().unwrap().insert(key_string(key));
        Histogram::noop()
    }
}

// ---------------------------------------------------------------------------------------------
// Fault-injecting wrappers (X1: a fault counts only if it fired exactly once)
// ---------------------------------------------------------------------------------------------

/// Fails the n-th `get_fact` call after arming, BEFORE the inner call (no effect), and records it.
struct FaultFactStore {
    inner: Arc<MockFactStore>,
    get_fact_calls: AtomicUsize,
    fail_get_fact_at: AtomicUsize,
    fired: AtomicUsize,
}

impl FaultFactStore {
    fn new(inner: Arc<MockFactStore>) -> Self {
        Self {
            inner,
            get_fact_calls: AtomicUsize::new(0),
            fail_get_fact_at: AtomicUsize::new(0),
            fired: AtomicUsize::new(0),
        }
    }

    fn arm_get_fact(&self, nth: usize) {
        self.get_fact_calls.store(0, Ordering::SeqCst);
        self.fired.store(0, Ordering::SeqCst);
        self.fail_get_fact_at.store(nth, Ordering::SeqCst);
    }
}

#[async_trait]
impl FactStore for FaultFactStore {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.inner.insert_fact(tenant, fact).await
    }
    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.inner.update_fact(tenant, fact).await
    }
    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
        let n = self.get_fact_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_get_fact_at.load(Ordering::SeqCst) == n {
            self.fired.fetch_add(1, Ordering::SeqCst);
            return Err(MemcoreError::Internal(
                "injected sql read failure".to_string(),
            ));
        }
        self.inner.get_fact(tenant, fact_id).await
    }
    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.inner.get_fact_including_deleted(tenant, fact_id).await
    }
    async fn list_fact_states(
        &self,
        tenant: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        self.inner.list_fact_states(tenant, after, limit).await
    }
    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool> {
        self.inner.fact_id_exists_in_any_tenant(fact_id).await
    }
    async fn search_facts(&self, query: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        self.inner.search_facts(query).await
    }
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>> {
        self.inner.fulltext_search_facts(query).await
    }
    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.inner.soft_delete_fact(tenant, fact_id).await
    }
    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_user_data(tenant).await
    }
    async fn delete_facts_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        self.inner
            .delete_facts_older_than(tenant, cutoff, dry_run)
            .await
    }
    async fn count_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_facts_by_org(org_id).await
    }
    async fn count_active_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_active_facts_by_org(org_id).await
    }
    async fn count_facts_by_user(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.inner.count_facts_by_user(tenant).await
    }
    async fn count_users_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_users_by_org(org_id).await
    }
    async fn list_users_by_org(
        &self,
        query: OrgUserListQuery,
    ) -> MemcoreResult<Vec<OrgUserSummary>> {
        self.inner.list_users_by_org(query).await
    }
}

/// Fails the n-th `search_vectors` call after arming, BEFORE the inner call, and records it.
struct FaultVectorStore {
    inner: Arc<MockVectorStore>,
    search_calls: AtomicUsize,
    fail_search_at: AtomicUsize,
    fired: AtomicUsize,
}

impl FaultVectorStore {
    fn new(inner: Arc<MockVectorStore>) -> Self {
        Self {
            inner,
            search_calls: AtomicUsize::new(0),
            fail_search_at: AtomicUsize::new(0),
            fired: AtomicUsize::new(0),
        }
    }

    fn arm_search(&self, nth: usize) {
        self.search_calls.store(0, Ordering::SeqCst);
        self.fired.store(0, Ordering::SeqCst);
        self.fail_search_at.store(nth, Ordering::SeqCst);
    }
}

#[async_trait]
impl VectorStore for FaultVectorStore {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        self.inner.upsert_vector(tenant, record).await
    }
    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        self.inner.write_vector(tenant, record, intent).await
    }
    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        let n = self.search_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_search_at.load(Ordering::SeqCst) == n {
            self.fired.fetch_add(1, Ordering::SeqCst);
            return Err(MemcoreError::Internal(
                "injected vector search failure".to_string(),
            ));
        }
        self.inner.search_vectors(query).await
    }
    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }
    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_by_user(tenant).await
    }
    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.inner.fact_vector_points(tenant, fact_id).await
    }
}

// ---------------------------------------------------------------------------------------------
// Standard fixture FX (protocol §2), seeded through the engine
// ---------------------------------------------------------------------------------------------

fn tenant(index: usize) -> TenantContext {
    TenantContext::new(TENANTS[index].0, TENANTS[index].1).expect("tenant")
}

fn canary(index: usize, n: usize) -> String {
    format!("CANARY-{}-{}-{n}", TENANTS[index].0, TENANTS[index].1)
}

fn fact_text(index: usize, n: usize) -> String {
    let topic = [
        "kayaking on mountain lakes",
        "baking sourdough bread",
        "restoring vintage radios",
    ];
    format!("Fact {n} {} prefers {}", canary(index, n), topic[n - 1])
}

fn episode_text(index: usize, n: usize) -> String {
    format!(
        "Episode {n} message {} about weekend plans",
        canary(index, 10 + n)
    )
}

fn all_canaries() -> Vec<String> {
    (0..TENANTS.len())
        .flat_map(|t| (1..=20).map(move |n| canary(t, n)))
        .collect()
}

struct Fx {
    facts: Arc<FaultFactStore>,
    raw_facts: Arc<MockFactStore>,
    vectors: Arc<FaultVectorStore>,
    raw_vectors: Arc<MockVectorStore>,
    episode_store: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
}

impl Fx {
    fn engine(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            Arc::new(llm),
            Arc::new(MockEmbeddingProvider::new(DIMS)),
        )
        .with_episodes(Some(EpisodeComponents {
            store: self.episode_store.clone(),
            vectors: self.episode_vectors.clone(),
        }))
        .with_search_max_limit(100)
        .expect("search max limit")
    }

    fn search_engine(&self) -> MemoryEngine {
        self.engine(MockLlmProvider::new())
    }

    async fn fact_by_n(&self, index: usize, n: usize) -> Fact {
        let marker = canary(index, n);
        self.all_facts(index, true)
            .await
            .into_iter()
            .find(|fact| fact.content.contains(&marker))
            .expect("fixture fact")
    }

    async fn all_facts(&self, index: usize, include_deleted: bool) -> Vec<Fact> {
        let mut query = FactSearchQuery::new(tenant(index), 100);
        query.include_deleted = include_deleted;
        let mut facts = self.raw_facts.search_facts(query).await.expect("facts");
        facts.sort_by_key(|fact| fact.id);
        facts
    }

    /// Hash-free snapshot of one tenant: fact rows (id, text, live) and fact points (fact id, text).
    async fn snapshot(&self, index: usize) -> String {
        let live: BTreeSet<Uuid> = self
            .all_facts(index, false)
            .await
            .iter()
            .map(|fact| fact.id)
            .collect();
        let rows: Vec<String> = self
            .all_facts(index, true)
            .await
            .iter()
            .map(|fact| format!("{}|{}|{}", fact.id, fact.content, live.contains(&fact.id)))
            .collect();
        let mut points: Vec<String> = self
            .raw_vectors
            .search_vectors(VectorSearchQuery {
                tenant: tenant(index),
                embedding: vec![1.0; DIMS],
                limit: 1000,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("points")
            .iter()
            .map(|hit| format!("{}|{}", hit.fact_id, hit.content))
            .collect();
        points.sort();
        format!("{rows:?}\n{points:?}")
    }

    async fn other_tenants(&self) -> (String, String) {
        (self.snapshot(1).await, self.snapshot(2).await)
    }

    async fn add_point(&self, index: usize, fact_id: Uuid, content: &str, embed_from: &str) {
        let embedding = deterministic_embedding(embed_from, DIMS).expect("embedding");
        self.add_point_vec(index, fact_id, content, embedding).await;
    }

    async fn add_point_vec(&self, index: usize, fact_id: Uuid, content: &str, embedding: Vec<f32>) {
        self.raw_vectors
            .upsert_vector(
                &tenant(index),
                VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id,
                    org_id: TENANTS[index].0.to_string(),
                    user_id: TENANTS[index].1.to_string(),
                    embedding,
                    content: content.to_string(),
                    memory_type: MemoryType::Skill,
                    metadata: json!({}),
                },
            )
            .await
            .expect("point");
    }
}

/// A unit vector with exactly `similarity` cosine to `reference` (as in `embedding_dedup.rs`).
fn with_cosine(reference: &[f32], similarity: f32) -> Vec<f32> {
    let norm: f32 = reference.iter().map(|x| x * x).sum::<f32>().sqrt();
    let unit: Vec<f32> = reference.iter().map(|x| x / norm).collect();
    let mut orthogonal = vec![0.0_f32; unit.len()];
    orthogonal[unit.len() - 1] = 1.0;
    let projection: f32 = orthogonal.iter().zip(&unit).map(|(a, b)| a * b).sum();
    for (o, u) in orthogonal.iter_mut().zip(&unit) {
        *o -= projection * u;
    }
    let orthogonal_norm: f32 = orthogonal.iter().map(|x| x * x).sum::<f32>().sqrt();
    for o in &mut orthogonal {
        *o /= orthogonal_norm;
    }
    let sin = (1.0 - similarity * similarity).max(0.0).sqrt();
    unit.iter()
        .zip(&orthogonal)
        .map(|(u, o)| similarity * u + sin * o)
        .collect()
}

fn candidate(content: &str) -> CandidateFact {
    CandidateFact::new(
        content,
        MemoryType::Skill,
        0.9,
        0.8,
        None,
        json!({ "src": "fixture" }),
    )
    .expect("candidate")
}

async fn fx() -> Fx {
    let raw_facts = Arc::new(MockFactStore::new());
    let raw_vectors = Arc::new(MockVectorStore::new());
    let fx = Fx {
        facts: Arc::new(FaultFactStore::new(raw_facts.clone())),
        raw_facts,
        vectors: Arc::new(FaultVectorStore::new(raw_vectors.clone())),
        raw_vectors,
        episode_store: Arc::new(MockEpisodeStore::new()),
        episode_vectors: Arc::new(MockVectorStore::new()),
    };
    for index in 0..TENANTS.len() {
        let candidates = (1..=3)
            .map(|n| {
                CandidateFact::new(
                    fact_text(index, n),
                    MemoryType::Skill,
                    0.9,
                    0.8,
                    None,
                    json!({ "label": format!("F{n}") }),
                )
                .expect("candidate")
            })
            .collect();
        let mut messages: Vec<MemoryMessage> = (1..=3)
            .map(|n| MemoryMessage {
                role: MessageRole::User,
                content: episode_text(index, n),
                message_id: Some(format!("e{n}")),
                ..Default::default()
            })
            .collect();
        messages.push(MemoryMessage {
            role: MessageRole::User,
            content: "short message 4".to_string(), // 15 chars: stored, not embedded
            message_id: Some("e4".to_string()),
            ..Default::default()
        });
        let out = fx
            .engine(MockLlmProvider::new().with_extraction_candidates(candidates))
            .add_memory(AddMemoryInput {
                tenant: tenant(index),
                messages,
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .expect("seed");
        assert_eq!(out.added, 3, "fixture seeding must add F1-F3");
    }
    fx
}

async fn search(
    engine: &MemoryEngine,
    index: usize,
    query: &str,
    mode: RetrievalMode,
) -> MemcoreResult<Vec<memcore_core::MemorySearchResult>> {
    engine
        .search_memory_with_options(
            SearchMemoryInput {
                tenant: tenant(index),
                query: query.to_string(),
                limit: 10,
                memory_types: None,
                metadata_filter: None,
            },
            SearchOptions {
                mode: Some(mode),
                user_names: Vec::new(),
                include_history: false,
            },
        )
        .await
        .map(|out| out.results)
}

async fn ingest(fx: &Fx, index: usize, text: &str) -> MemcoreResult<AddMemoryOutput> {
    fx.engine(MockLlmProvider::new().with_extraction_candidates(vec![candidate(text)]))
        .add_memory(AddMemoryInput {
            tenant: tenant(index),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "x".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
}

/// W2 state for TA's F1: the SQL row gets new text; the point keeps the old text and old vector.
async fn make_f1_stale(fx: &Fx) -> (Fact, String, String) {
    let f1 = fx.fact_by_n(0, 1).await;
    let old = f1.content.clone();
    let new = format!("Fact 1 {} now prefers sea kayaking", canary(0, 1));
    let mut updated = f1.clone();
    updated.content = new.clone();
    fx.raw_facts
        .update_fact(&tenant(0), updated)
        .await
        .expect("row update");
    (f1, old, new)
}

const STALE_MARK: &str = "mountain lakes"; // only in F1's old text

fn assert_no_foreign_canary(texts: &[String], own: usize) {
    for text in texts {
        for (index, _) in TENANTS.iter().enumerate() {
            if index == own {
                continue;
            }
            for n in 1..=20 {
                assert!(
                    !text.contains(&canary(index, n)),
                    "X3: another tenant's canary in output"
                );
            }
        }
    }
}

fn assert_metric_keys_bounded(captured: &Captured) {
    let canaries = all_canaries();
    for key in captured.all_keys() {
        for c in &canaries {
            assert!(!key.contains(c.as_str()), "X3: canary in a metric key");
        }
    }
    for key in captured.rejection_keys() {
        let ok = VECTOR_HIT_PATHS.iter().any(|path| {
            VECTOR_HIT_REJECT_REASONS.iter().any(|reason| {
                key == format!("{VECTOR_HIT_REJECTED_METRIC}{{path={path},reason={reason}}}")
            })
        });
        assert!(ok, "E3: unexpected rejection series {key}");
    }
}

// ---------------------------------------------------------------------------------------------
// Group R: read gates (R1)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn r1_a_stale_point_after_row_update_is_not_served() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let before = fx.other_tenants().await;
    let (f1, old, new) = make_f1_stale(&fx).await;
    let engine = fx.search_engine();

    // The stale vector embeds the OLD text, so the old text is its best query (score 1.0).
    let semantic = search(&engine, 0, &old, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert!(
        semantic.iter().all(|r| r.fact_id != f1.id),
        "stale hit returned"
    );
    assert!(semantic.iter().all(|r| !r.content.contains(STALE_MARK)));
    assert!(captured.get("search", "stale_content") >= 1);

    // Hybrid: the keyword leg may return F1, but only with the SQL text and no vector score.
    let hybrid = search(&engine, 0, &new, RetrievalMode::Hybrid)
        .await
        .expect("search");
    let f1_hit = hybrid
        .iter()
        .find(|r| r.fact_id == f1.id)
        .expect("keyword hit");
    assert_eq!(f1_hit.content, new);
    assert_eq!(
        f1_hit.vector_score, None,
        "the stale vector must not score F1"
    );
    for mode in [
        RetrievalMode::Semantic,
        RetrievalMode::Hybrid,
        RetrievalMode::Rrf,
    ] {
        for query in [old.as_str(), new.as_str()] {
            let results = search(&engine, 0, query, mode).await.expect("search");
            assert!(
                results.iter().all(|r| !r.content.contains(STALE_MARK)),
                "X5"
            );
        }
    }
    assert_eq!(fx.other_tenants().await, before, "X2");
    assert_metric_keys_bounded(&captured);
}

#[tokio::test]
async fn r1_a2_payload_altered_point_is_not_served() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f2 = fx.fact_by_n(0, 2).await;
    // Replace F2's point with one whose payload text differs from SQL (vector of the SQL text).
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f2.id)
        .await
        .expect("delete");
    fx.add_point(
        0,
        f2.id,
        "PAYLOAD-ONLY text that SQL never had",
        &f2.content,
    )
    .await;
    let results = search(&fx.search_engine(), 0, &f2.content, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert!(results.iter().all(|r| r.fact_id != f2.id));
    assert!(results.iter().all(|r| !r.content.contains("PAYLOAD-ONLY")));
    assert_eq!(captured.get("search", "stale_content"), 1);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r1_b_point_of_absent_fact_is_not_served() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let orphan_text = "ORPHAN-B text of a fact that does not exist";
    fx.add_point(0, Uuid::new_v4(), orphan_text, orphan_text)
        .await;
    let results = search(&fx.search_engine(), 0, orphan_text, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert!(results.iter().all(|r| !r.content.contains("ORPHAN-B")));
    assert_eq!(captured.get("search", "no_live_row"), 1);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r1_c_point_of_soft_deleted_fact_is_not_served() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f2 = fx.fact_by_n(0, 2).await;
    fx.raw_facts
        .soft_delete_fact(&tenant(0), f2.id)
        .await
        .expect("soft delete");
    let results = search(&fx.search_engine(), 0, &f2.content, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert!(results.iter().all(|r| r.fact_id != f2.id));
    assert_eq!(captured.get("search", "no_live_row"), 1);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r1_d_context_and_candidates_never_render_stale_text() {
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let (f1, old, _) = make_f1_stale(&fx).await;
    let engine = fx.search_engine();

    let context = engine
        .d76_context(D76ContextInput {
            tenant: tenant(0),
            query: old.clone(),
            token_budget: 1500,
        })
        .await
        .expect("context");
    assert!(context.episodes_included);
    assert!(
        !context.context.text.contains(STALE_MARK),
        "stale text rendered"
    );
    assert!(context.context.items.iter().all(|item| item.id != f1.id));

    let candidates = engine
        .search_candidates(SearchCandidatesInput {
            tenant: tenant(0),
            query: old.clone(),
            fact_limit: 10,
            episode_limit: 10,
            reference_time: None,
        })
        .await
        .expect("candidates");
    assert!(candidates.results.iter().all(|c| c.id != f1.id));
    assert!(
        candidates
            .results
            .iter()
            .all(|c| !c.content.contains(STALE_MARK))
    );

    // Facts-only (C0) form, episodes off.
    let c0 = MemoryEngine::new(
        fx.facts.clone(),
        fx.vectors.clone(),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(DIMS)),
    )
    .with_search_max_limit(100)
    .expect("limit")
    .d76_context(D76ContextInput {
        tenant: tenant(0),
        query: old,
        token_budget: 1500,
    })
    .await
    .expect("c0 context");
    assert!(!c0.episodes_included);
    assert!(!c0.context.text.contains(STALE_MARK));
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r1_valid_hits_carry_sql_text_and_metadata_and_keep_their_rank() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f3 = fx.fact_by_n(0, 3).await;
    let results = search(&fx.search_engine(), 0, &f3.content, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert_eq!(results[0].fact_id, f3.id);
    assert_eq!(results[0].content, f3.content);
    assert_eq!(results[0].metadata, f3.metadata);
    assert_eq!(results[0].metadata, json!({ "label": "F3" }));
    assert!((results[0].vector_score.expect("score") - 1.0).abs() < 1e-5);
    assert_eq!(results.len(), 3, "all three live facts of TA, nothing else");
    assert!(
        captured.rejection_keys().is_empty(),
        "no rejection on consistent data"
    );
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r1_rejected_hit_keeps_its_rank_slot_so_valid_scores_are_unchanged() {
    // An orphan always occupied a rank slot (its fused score was computed, then it was dropped);
    // a stale hit now behaves the same way, so the fused scores of the other hits do not move.
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let engine = fx.search_engine();
    let f1 = fx.fact_by_n(0, 1).await;
    let baseline = search(&engine, 0, &f1.content, RetrievalMode::Semantic)
        .await
        .expect("search");
    let f2 = fx.fact_by_n(0, 2).await;
    let f3 = fx.fact_by_n(0, 3).await;
    let score_of = |results: &[memcore_core::MemorySearchResult], id: Uuid| {
        results
            .iter()
            .find(|r| r.fact_id == id)
            .map(|r| (r.score, r.vector_score))
    };
    // Make F1 (rank 1) stale through its row; F2 / F3 keep rank 2 / 3 and identical scores.
    let mut updated = f1.clone();
    updated.content = format!("Fact 1 {} changed text", canary(0, 1));
    fx.raw_facts
        .update_fact(&tenant(0), updated)
        .await
        .expect("update");
    let after = search(&engine, 0, &f1.content, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert!(after.iter().all(|r| r.fact_id != f1.id));
    assert_eq!(score_of(&after, f2.id), score_of(&baseline, f2.id));
    assert_eq!(score_of(&after, f3.id), score_of(&baseline, f3.id));
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r1_point_attributed_to_tenant_a_but_naming_tenant_b_fact_is_not_served() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let b1 = fx.fact_by_n(1, 1).await;
    // A point in TA's payload scope whose fact id is TB's live row (attribution conflict).
    fx.add_point(0, b1.id, &b1.content, &b1.content).await;
    let before = fx.snapshot(1).await;
    let results = search(&fx.search_engine(), 0, &b1.content, RetrievalMode::Semantic)
        .await
        .expect("search");
    assert!(
        results.iter().all(|r| r.fact_id != b1.id),
        "no cross-tenant fallback"
    );
    let texts: Vec<String> = results.iter().map(|r| r.content.clone()).collect();
    assert_no_foreign_canary(&texts, 0);
    assert_eq!(captured.get("search", "no_live_row"), 1);
    assert_eq!(fx.snapshot(1).await, before);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

// ---------------------------------------------------------------------------------------------
// Group R: dedup gates (R2)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn r2_a_orphan_of_soft_deleted_fact_does_not_suppress_a_new_fact() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let before = fx.other_tenants().await;
    let f2 = fx.fact_by_n(0, 2).await;
    fx.raw_facts
        .soft_delete_fact(&tenant(0), f2.id)
        .await
        .expect("soft delete");
    let out = ingest(&fx, 0, &f2.content).await.expect("ingest");
    assert_eq!(out.added, 1, "the re-stated fact must be stored");
    assert_eq!(out.discarded.duplicate_embedding, 0);
    assert_eq!(captured.get("dedup", "no_live_row"), 1);
    let live: Vec<Fact> = fx.all_facts(0, false).await;
    assert!(
        live.iter()
            .any(|f| f.content == f2.content && f.id != f2.id)
    );
    assert_eq!(fx.other_tenants().await, before, "X2");
}

#[tokio::test]
async fn r2_b_orphan_of_absent_fact_does_not_suppress() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let y = "Fact Y about an orphan only Qdrant remembers";
    fx.add_point(0, Uuid::new_v4(), y, y).await; // hard-deleted row: point without any SQL row
    let out = ingest(&fx, 0, y).await.expect("ingest");
    assert_eq!(out.added, 1);
    assert_eq!(captured.get("dedup", "no_live_row"), 1);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_c_stale_point_does_not_suppress_the_old_text() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let (_, old, _) = make_f1_stale(&fx).await;
    let out = ingest(&fx, 0, &old).await.expect("ingest");
    assert_eq!(out.added, 1, "never suppressed by the stale point");
    assert_eq!(out.discarded.duplicate_embedding, 0);
    assert_eq!(captured.get("dedup", "stale_content"), 1);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_d_genuine_duplicate_is_still_a_noop() {
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f1 = fx.fact_by_n(0, 1).await;
    let out = ingest(&fx, 0, &f1.content).await.expect("ingest");
    assert_eq!(out.added, 0);
    assert_eq!(out.noop, 1);
    assert_eq!(
        out.discarded.duplicate_content, 1,
        "the SQL exact-dedup branch decides"
    );
    assert_eq!(fx.all_facts(0, false).await.len(), 3);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_valid_embedding_duplicate_still_suppresses() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f3 = fx.fact_by_n(0, 3).await;
    // A different text whose point is F3's own, consistent vector: re-point F3's vector so the
    // candidate's embedding scores 1.0 against a live, text-matching row.
    let candidate_text = "A paraphrase that embeds exactly like fact three";
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f3.id)
        .await
        .expect("delete");
    fx.add_point(0, f3.id, &f3.content, candidate_text).await;
    let out = ingest(&fx, 0, candidate_text).await.expect("ingest");
    assert_eq!(out.added, 0);
    assert_eq!(out.discarded.duplicate_embedding, 1);
    assert!(captured.rejection_keys().is_empty());
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_invalid_hits_before_a_valid_one_are_skipped_and_the_valid_one_decides() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let candidate_text = "Candidate Z with three similar stored points";
    let reference = deterministic_embedding(candidate_text, DIMS).expect("embedding");
    // Ordered above the threshold (0.92): an orphan (absent row) at 0.99, a stale point (F2:
    // payload text != row) at 0.97, then the valid point (F3: payload == row) at 0.94.
    let f2 = fx.fact_by_n(0, 2).await;
    let f3 = fx.fact_by_n(0, 3).await;
    fx.add_point_vec(0, Uuid::new_v4(), "orphan Z", with_cosine(&reference, 0.99))
        .await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f2.id)
        .await
        .expect("delete");
    fx.add_point_vec(0, f2.id, "stale Z text", with_cosine(&reference, 0.97))
        .await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f3.id)
        .await
        .expect("delete");
    fx.add_point_vec(0, f3.id, &f3.content, with_cosine(&reference, 0.94))
        .await;
    let out = ingest(&fx, 0, candidate_text).await.expect("ingest");
    assert_eq!(
        out.added, 0,
        "the valid live match suppresses the duplicate"
    );
    assert_eq!(out.discarded.duplicate_embedding, 1);
    assert_eq!(
        captured.get("dedup", "no_live_row"),
        1,
        "the orphan was examined and skipped"
    );
    assert_eq!(
        captured.get("dedup", "stale_content"),
        1,
        "the stale point was examined and skipped"
    );
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_only_invalid_hits_never_suppress() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let candidate_text = "Candidate W whose similar points are all invalid";
    let f2 = fx.fact_by_n(0, 2).await;
    fx.add_point(0, Uuid::new_v4(), "orphan W", candidate_text)
        .await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f2.id)
        .await
        .expect("delete");
    fx.add_point(0, f2.id, "stale W text", candidate_text).await;
    let out = ingest(&fx, 0, candidate_text).await.expect("ingest");
    assert_eq!(out.added, 1);
    assert_eq!(captured.get("dedup", "no_live_row"), 1);
    assert_eq!(captured.get("dedup", "stale_content"), 1);
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_point_naming_another_tenants_fact_does_not_suppress() {
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let b1 = fx.fact_by_n(1, 1).await;
    let text = "Tenant A states something new and unrelated";
    // TA-scoped point whose fact id is TB's live row and whose text is TB's.
    fx.add_point(0, b1.id, &b1.content, text).await;
    let before = fx.snapshot(1).await;
    let out = ingest(&fx, 0, text).await.expect("ingest");
    assert_eq!(out.added, 1, "no cross-tenant suppression");
    assert_eq!(fx.snapshot(1).await, before, "X2");
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn r2_sql_error_during_dedup_fails_closed() {
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f3 = fx.fact_by_n(0, 3).await;
    let text = "A paraphrase that embeds exactly like fact three";
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f3.id)
        .await
        .expect("delete");
    fx.add_point(0, f3.id, &f3.content, text).await;

    // Control (no fault): exactly one get_fact call during this ingest, the dedup check.
    fx.facts.arm_get_fact(0);
    let control = ingest(&fx, 0, text).await.expect("control");
    assert_eq!(control.discarded.duplicate_embedding, 1);
    assert_eq!(fx.facts.get_fact_calls.load(Ordering::SeqCst), 1);

    let before = fx.snapshot(0).await;
    fx.facts.arm_get_fact(1);
    let err = ingest(&fx, 0, text).await.expect_err("must fail closed");
    assert_eq!(fx.facts.fired.load(Ordering::SeqCst), 1, "X1");
    assert_no_foreign_canary(&[err.to_string()], 0);
    assert!(!err.to_string().contains("fact three"));
    assert_eq!(
        fx.snapshot(0).await,
        before,
        "no fact written, nothing suppressed"
    );
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

// ---------------------------------------------------------------------------------------------
// Group E: backend errors and disclosure
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn e1_sql_error_in_the_per_hit_fetch_fails_the_search() {
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f1 = fx.fact_by_n(0, 1).await;
    let engine = fx.search_engine();
    fx.facts.arm_get_fact(1);
    let err = search(&engine, 0, &f1.content, RetrievalMode::Semantic)
        .await
        .expect_err("no payload fallback");
    assert_eq!(fx.facts.fired.load(Ordering::SeqCst), 1, "X1");
    assert!(!err.to_string().contains("CANARY"), "no text in the error");
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn e2_vector_search_error_fails_the_search() {
    let fx = fx().await;
    let x2_before = fx.other_tenants().await;
    let f1 = fx.fact_by_n(0, 1).await;
    let engine = fx.search_engine();
    fx.vectors.arm_search(1);
    let err = search(&engine, 0, &f1.content, RetrievalMode::Semantic)
        .await
        .expect_err("error");
    assert_eq!(fx.vectors.fired.load(Ordering::SeqCst), 1, "X1");
    assert!(!err.to_string().contains("CANARY"));
    assert_eq!(fx.other_tenants().await, x2_before, "X2");
}

#[tokio::test]
async fn e3_rejection_series_are_bounded_and_independent_of_tenant_count() {
    async fn series(extra_tenants: usize) -> BTreeSet<String> {
        let captured = Captured::default();
        let recorder = TestRecorder(captured.clone());
        let _guard = metrics::set_default_local_recorder(&recorder);
        let fx = fx().await;
        let (_, old, _) = make_f1_stale(&fx).await;
        let engine = fx.search_engine();
        let f2 = fx.fact_by_n(0, 2).await;
        fx.raw_facts
            .soft_delete_fact(&tenant(0), f2.id)
            .await
            .expect("delete");
        search(&engine, 0, &old, RetrievalMode::Semantic)
            .await
            .expect("search");
        search(&engine, 0, &f2.content, RetrievalMode::Semantic)
            .await
            .expect("search");
        ingest(&fx, 0, &old).await.expect("ingest");
        ingest(&fx, 0, &f2.content).await.expect("ingest");
        for extra in 0..extra_tenants {
            let t = TenantContext::new(format!("org_x{extra}"), "ux").expect("tenant");
            let text = format!("orphan of extra tenant {extra}");
            fx.raw_vectors
                .upsert_vector(
                    &t,
                    VectorRecord {
                        id: Uuid::new_v4(),
                        fact_id: Uuid::new_v4(),
                        org_id: t.org_id.clone(),
                        user_id: t.user_id.clone(),
                        embedding: deterministic_embedding(&text, DIMS).expect("embedding"),
                        content: text.clone(),
                        memory_type: MemoryType::Skill,
                        metadata: json!({}),
                    },
                )
                .await
                .expect("point");
            engine
                .search_memory_with_options(
                    SearchMemoryInput {
                        tenant: t,
                        query: text,
                        limit: 10,
                        memory_types: None,
                        metadata_filter: None,
                    },
                    SearchOptions {
                        mode: Some(RetrievalMode::Semantic),
                        user_names: Vec::new(),
                        include_history: false,
                    },
                )
                .await
                .expect("search");
        }
        assert_metric_keys_bounded(&captured);
        // Both searches see (and reject) F1's stale point and F2's soft-deleted point: the mock
        // returns every tenant point. The extra tenants' orphans add to no_live_row only.
        assert_eq!(captured.get("search", "stale_content"), 2);
        assert_eq!(
            captured.get("search", "no_live_row"),
            2 + extra_tenants as u64
        );
        assert_eq!(captured.get("dedup", "stale_content"), 1);
        assert_eq!(captured.get("dedup", "no_live_row"), 1);
        captured.rejection_keys()
    }
    let base = series(0).await;
    assert_eq!(base.len(), 4, "all four fixed series observed");
    assert_eq!(
        series(3).await,
        base,
        "the series set does not grow with tenants"
    );
}
