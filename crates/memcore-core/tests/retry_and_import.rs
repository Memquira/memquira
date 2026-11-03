//! Re-drivable deletion (R3), the exact-dedup retry
//! heal (R4) and the fail-closed append import (R7), on the in-process mocks.
//!
//! Case ids:
//! W1 / W1-R, W2 / W2-R, W3, D1-D4, W8-A..E, K1. Every fault is injected by a wrapper that logs
//! whether it fired (X1); other tenants are compared before and after every tenant-A case (X2);
//! errors are scanned for other tenants' canaries (X3).
//!
//! Test-machinery notes (the protocol's invariants are unchanged):
//! * W3 names `MEMCORE_QDRANT_WRITE_MODE=legacy`. On Qdrant (0599dcf) `Replace` never pre-deletes in
//!   either mode (`replace_reusing_point`); the delete-then-upsert replacement is the trait's
//!   default `write_vector`, which the mock store uses. The wrapper reproduces that default through
//!   its own `delete_by_fact_id` + `upsert_vector`, so the upsert fault lands after the pre-delete.
//! * W8-A names `write_vector#2`; the import path writes vectors with `upsert_vector`, so the fault
//!   is the second `upsert_vector` of the import (the frozen expected state: 2 facts, 1 point).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    FactFulltextQuery, MemoryEventQuery, OrgUserListQuery, OrgUserSummary, RetentionPruneResult,
    ScoredFact, StoredFactState, VectorPointState,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, DeleteMemoryInput, EmbeddingProvider,
    EpisodeComponents, EpisodeStore, ExportUserDataInput, Fact, FactOperation,
    FactOperationDecision, FactSearchQuery, FactStore, ForgetUserInput, ImportMode,
    ImportUserDataInput, MemoryEngine, MemoryEventOperation, MemoryEventStore, MemoryMessage,
    MemoryType, MessageRole, RetrievalMode, SearchMemoryInput, SearchOptions, TenantContext,
    UserMemoryExport, VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore,
    VectorWriteIntent, derived_import_fact_id,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 32;
const TENANTS: [(&str, &str); 3] = [("org_a", "u1"), ("org_a", "u2"), ("org_b", "u1")];

// ---------------------------------------------------------------------------------------------
// Fault plan (X1)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Fail before the inner call (no effect).
    Error,
    /// Report success without any effect (a lost write), to exercise the R4 verification.
    Swallow,
}

/// Per operation: calls since arm, fail at call n (0 = off), times fired, mode.
type OpState = (usize, usize, usize, Option<Mode>);

#[derive(Default)]
struct Faults {
    ops: Mutex<HashMap<&'static str, OpState>>,
}

impl Faults {
    fn arm(&self, op: &'static str, nth: usize, mode: Mode) {
        self.ops.lock().unwrap().insert(op, (0, nth, 0, Some(mode)));
    }

    /// Clears every arming and count (call before a request whose calls are counted).
    fn reset(&self) {
        self.ops.lock().unwrap().clear();
    }

    fn hit(&self, op: &'static str) -> Option<Mode> {
        let mut ops = self.ops.lock().unwrap();
        let entry = ops.entry(op).or_insert((0, 0, 0, None));
        entry.0 += 1;
        if entry.1 != 0 && entry.0 == entry.1 {
            entry.2 += 1;
            return entry.3;
        }
        None
    }

    fn fired(&self, op: &'static str) -> usize {
        self.ops.lock().unwrap().get(op).map(|e| e.2).unwrap_or(0)
    }

    fn calls(&self, op: &'static str) -> usize {
        self.ops.lock().unwrap().get(op).map(|e| e.0).unwrap_or(0)
    }
}

fn injected(op: &str) -> MemcoreError {
    MemcoreError::Internal(format!("injected {op} failure"))
}

struct FaultFactStore {
    inner: Arc<MockFactStore>,
    faults: Arc<Faults>,
}

#[async_trait]
impl FactStore for FaultFactStore {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        if self.faults.hit("insert_fact").is_some() {
            return Err(injected("insert_fact"));
        }
        self.inner.insert_fact(tenant, fact).await
    }
    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.inner.update_fact(tenant, fact).await
    }
    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
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
        if self.faults.hit("soft_delete_fact").is_some() {
            return Err(injected("soft_delete_fact"));
        }
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

struct FaultVectorStore {
    inner: Arc<MockVectorStore>,
    faults: Arc<Faults>,
}

#[async_trait]
impl VectorStore for FaultVectorStore {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        match self.faults.hit("upsert_vector") {
            Some(Mode::Error) => return Err(injected("upsert_vector")),
            Some(Mode::Swallow) => return Ok(()),
            None => {}
        }
        self.inner.upsert_vector(tenant, record).await
    }

    /// The trait's default replacement semantics (as the mock uses them), routed through this
    /// wrapper's own `delete_by_fact_id` / `upsert_vector`, so their faults apply too.
    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        match self.faults.hit("write_vector") {
            Some(Mode::Error) => return Err(injected("write_vector")),
            Some(Mode::Swallow) => return Ok(()),
            None => {}
        }
        if intent == VectorWriteIntent::Replace {
            match self.delete_by_fact_id(tenant, record.fact_id).await {
                Ok(()) | Err(MemcoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.upsert_vector(tenant, record).await
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        if self.faults.hit("search_vectors").is_some() {
            return Err(injected("search_vectors"));
        }
        self.inner.search_vectors(query).await
    }

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        if self.faults.hit("delete_by_fact_id").is_some() {
            return Err(injected("delete_by_fact_id"));
        }
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        if self.faults.hit("delete_by_user").is_some() {
            return Err(injected("delete_by_user"));
        }
        self.inner.delete_by_user(tenant).await
    }

    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        if self.faults.hit("fact_vector_points").is_some() {
            return Err(injected("fact_vector_points"));
        }
        self.inner.fact_vector_points(tenant, fact_id).await
    }
}

struct FaultEmbedding {
    inner: MockEmbeddingProvider,
    faults: Arc<Faults>,
}

#[async_trait]
impl EmbeddingProvider for FaultEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        if self.faults.hit("embed_text").is_some() {
            return Err(MemcoreError::ProviderError(
                "injected embedding failure".to_string(),
            ));
        }
        self.inner.embed_text(text).await
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        self.inner.embed_batch(texts).await
    }
    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

// ---------------------------------------------------------------------------------------------
// Standard fixture FX (protocol §2)
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

fn episode_messages(index: usize) -> Vec<MemoryMessage> {
    let mut messages: Vec<MemoryMessage> = (1..=3)
        .map(|n| MemoryMessage {
            role: MessageRole::User,
            content: format!(
                "Episode {n} message {} about weekend plans",
                canary(index, 10 + n)
            ),
            message_id: Some(format!("e{n}")),
            ..Default::default()
        })
        .collect();
    messages.push(MemoryMessage {
        role: MessageRole::User,
        content: "short message 4".to_string(), // stored, not embedded
        message_id: Some("e4".to_string()),
        ..Default::default()
    });
    messages
}

fn candidate(content: &str, label: &str) -> CandidateFact {
    CandidateFact::new(
        content,
        MemoryType::Skill,
        0.9,
        0.8,
        None,
        json!({ "label": label }),
    )
    .expect("candidate")
}

struct Fx {
    faults: Arc<Faults>,
    facts: Arc<FaultFactStore>,
    raw_facts: Arc<MockFactStore>,
    vectors: Arc<FaultVectorStore>,
    raw_vectors: Arc<MockVectorStore>,
    episode_store: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
}

impl Fx {
    fn engine(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            Arc::new(llm),
            Arc::new(FaultEmbedding {
                inner: MockEmbeddingProvider::new(DIMS),
                faults: self.faults.clone(),
            }),
        )
        .with_event_store(self.events.clone())
        .with_episodes(Some(EpisodeComponents {
            store: self.episode_store.clone(),
            vectors: self.episode_vectors.clone(),
        }))
        .with_search_max_limit(100)
        .expect("search max limit")
    }

    fn plain(&self) -> MemoryEngine {
        self.engine(MockLlmProvider::new())
    }

    async fn facts_of(&self, index: usize, include_deleted: bool) -> Vec<Fact> {
        let mut query = FactSearchQuery::new(tenant(index), 100);
        query.include_deleted = include_deleted;
        let mut facts = self.raw_facts.search_facts(query).await.expect("facts");
        facts.sort_by_key(|fact| fact.id);
        facts
    }

    async fn fact_n(&self, index: usize, n: usize) -> Fact {
        let marker = canary(index, n);
        self.facts_of(index, true)
            .await
            .into_iter()
            .find(|fact| fact.content.contains(&marker))
            .expect("fixture fact")
    }

    async fn points(&self, index: usize, fact_id: Uuid) -> Vec<VectorPointState> {
        self.raw_vectors
            .fact_vector_points(&tenant(index), fact_id)
            .await
            .expect("points")
            .expect("the mock reports points")
    }

    async fn state(&self, index: usize, fact_id: Uuid) -> Option<StoredFactState> {
        self.raw_facts
            .get_fact_including_deleted(&tenant(index), fact_id)
            .await
            .expect("state")
    }

    async fn delete_events(&self, index: usize, fact_id: Uuid) -> usize {
        let mut query = MemoryEventQuery::new(tenant(index), 100);
        query.fact_id = Some(fact_id);
        query.operation = Some(MemoryEventOperation::Delete);
        self.events.list_events(query).await.expect("events").len()
    }

    /// Fact rows (id, text, deleted), every fact point (fact id, text), episode rows (id, text,
    /// embedded) and the event count of one tenant.
    async fn snapshot(&self, index: usize) -> String {
        let t = tenant(index);
        let live: Vec<Uuid> = self
            .facts_of(index, false)
            .await
            .iter()
            .map(|f| f.id)
            .collect();
        let rows: Vec<String> = self
            .facts_of(index, true)
            .await
            .iter()
            .map(|f| format!("{}|{}|{}", f.id, f.content, !live.contains(&f.id)))
            .collect();
        let mut points: Vec<String> = self
            .raw_vectors
            .search_vectors(VectorSearchQuery {
                tenant: t.clone(),
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
        let mut episodes: Vec<String> = self
            .episode_store
            .list_episodes(&t, 1000)
            .await
            .expect("episodes")
            .iter()
            .map(|e| format!("{}|{}|{}", e.id, e.content, e.embedded))
            .collect();
        episodes.sort();
        let events = self
            .events
            .list_events(MemoryEventQuery::new(t, 200))
            .await
            .expect("events")
            .len();
        format!("{rows:?}\n{points:?}\n{episodes:?}\nevents={events}")
    }

    async fn others(&self) -> (String, String) {
        (self.snapshot(1).await, self.snapshot(2).await)
    }

    async fn add_point(&self, index: usize, fact_id: Uuid, content: &str) {
        self.raw_vectors
            .upsert_vector(
                &tenant(index),
                VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id,
                    org_id: TENANTS[index].0.to_string(),
                    user_id: TENANTS[index].1.to_string(),
                    embedding: deterministic_embedding(content, DIMS).expect("embedding"),
                    content: content.to_string(),
                    memory_type: MemoryType::Skill,
                    metadata: json!({}),
                },
            )
            .await
            .expect("point");
    }
}

async fn fx() -> Fx {
    let faults = Arc::new(Faults::default());
    let raw_facts = Arc::new(MockFactStore::new());
    let raw_vectors = Arc::new(MockVectorStore::new());
    let fx = Fx {
        facts: Arc::new(FaultFactStore {
            inner: raw_facts.clone(),
            faults: faults.clone(),
        }),
        vectors: Arc::new(FaultVectorStore {
            inner: raw_vectors.clone(),
            faults: faults.clone(),
        }),
        faults,
        raw_facts,
        raw_vectors,
        episode_store: Arc::new(MockEpisodeStore::new()),
        episode_vectors: Arc::new(MockVectorStore::new()),
        events: Arc::new(MockMemoryEventStore::new()),
    };
    for index in 0..TENANTS.len() {
        let candidates = (1..=3)
            .map(|n| candidate(&fact_text(index, n), &format!("F{n}")))
            .collect();
        let out = fx
            .engine(MockLlmProvider::new().with_extraction_candidates(candidates))
            .add_memory(AddMemoryInput {
                tenant: tenant(index),
                messages: episode_messages(index),
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .expect("seed");
        assert_eq!(out.added, 3);
    }
    fx.faults.reset();
    fx
}

/// One serialised ingest request with a stable message id, whose extraction yields `text`.
fn request(index: usize, message_id: &str) -> AddMemoryInput {
    AddMemoryInput {
        tenant: tenant(index),
        messages: vec![MemoryMessage {
            role: MessageRole::User,
            content: format!(
                "Message {message_id} for {} with enough text",
                canary(index, 20)
            ),
            message_id: Some(message_id.to_string()),
            ..Default::default()
        }],
        metadata: json!({}),
        session: Default::default(),
    }
}

async fn ingest(
    fx: &Fx,
    llm: MockLlmProvider,
    input: AddMemoryInput,
) -> MemcoreResult<AddMemoryOutput> {
    fx.engine(llm).add_memory(input).await
}

fn extracting(text: &str) -> MockLlmProvider {
    MockLlmProvider::new().with_extraction_candidates(vec![candidate(text, "new")])
}

fn updating(text: &str, target: Uuid) -> MockLlmProvider {
    extracting(text).with_classification_decision(FactOperationDecision {
        operation: FactOperation::Update,
        target_fact_id: Some(target),
        reason: Some("test update".to_string()),
        confidence: 0.9,
    })
}

async fn search(fx: &Fx, index: usize, query: &str, mode: RetrievalMode) -> Vec<(Uuid, String)> {
    fx.plain()
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
        .expect("search")
        .results
        .into_iter()
        .map(|r| (r.fact_id, r.content))
        .collect()
}

fn assert_no_foreign_canary(text: &str, own: usize) {
    for index in 0..TENANTS.len() {
        if index != own {
            for n in 1..=20 {
                assert!(!text.contains(&canary(index, n)), "X3: foreign canary");
            }
        }
    }
}

fn single_matching(points: &[VectorPointState], fact: &Fact) -> bool {
    points.len() == 1
        && points[0].content == fact.content
        && points[0].memory_type == fact.memory_type
}

async fn delete(fx: &Fx, index: usize, id: Uuid) -> MemcoreResult<bool> {
    fx.plain()
        .delete_memory(DeleteMemoryInput {
            tenant: tenant(index),
            memory_id: id,
        })
        .await
        .map(|out| out.deleted)
}

// ---------------------------------------------------------------------------------------------
// Group W: write windows and retry heal (R4)
// ---------------------------------------------------------------------------------------------

const NEW_FACT: &str = "Fact new CANARY-org_a-u1-7 enjoys mountain biking at dawn";

#[tokio::test]
async fn w1_then_w1_r_identical_retry_heals_the_missing_vector() {
    let fx = fx().await;
    let x2 = fx.others().await;

    // W1: the ADD's vector write fails after the row insert.
    fx.faults.arm("write_vector", 1, Mode::Error);
    let err = ingest(&fx, extracting(NEW_FACT), request(0, "w1"))
        .await
        .expect_err("5xx");
    assert_eq!(fx.faults.fired("write_vector"), 1, "X1");
    assert_no_foreign_canary(&err.to_string(), 0);
    let row = fx
        .facts_of(0, false)
        .await
        .into_iter()
        .find(|f| f.content == NEW_FACT)
        .expect("row");
    assert!(
        fx.points(0, row.id).await.is_empty(),
        "W1: live row without a point"
    );
    assert!(
        search(&fx, 0, NEW_FACT, RetrievalMode::Semantic)
            .await
            .iter()
            .all(|r| r.0 != row.id)
    );
    let keyword = search(&fx, 0, NEW_FACT, RetrievalMode::Hybrid).await;
    assert!(
        keyword.contains(&(row.id, NEW_FACT.to_string())),
        "keyword finds the SQL text"
    );

    // W1-R: the identical serialised request, no fault.
    fx.faults.reset();
    let out = ingest(&fx, extracting(NEW_FACT), request(0, "w1"))
        .await
        .expect("200");
    assert_eq!(out.added, 0);
    assert_eq!(out.noop, 1);
    assert_eq!(
        out.discarded.duplicate_content, 1,
        "the SQL exact-dedup branch was reached"
    );
    let rows: Vec<Fact> = fx
        .facts_of(0, true)
        .await
        .into_iter()
        .filter(|f| f.content == NEW_FACT)
        .collect();
    assert_eq!(rows.len(), 1, "no second row");
    assert_eq!(rows[0].id, row.id, "no new fact id");
    assert!(
        single_matching(&fx.points(0, row.id).await, &row),
        "exactly 1 point, payload = row"
    );
    assert!(
        search(&fx, 0, NEW_FACT, RetrievalMode::Semantic)
            .await
            .contains(&(row.id, NEW_FACT.to_string()))
    );
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn w2_then_w2_r_identical_retry_heals_the_stale_vector() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let f1 = fx.fact_n(0, 1).await;
    let new_text = format!("Fact 1 {} now prefers sea kayaking at night", canary(0, 1));

    // W2: the UPDATE's vector write fails after the row update.
    fx.faults.arm("write_vector", 1, Mode::Error);
    ingest(&fx, updating(&new_text, f1.id), request(0, "w2"))
        .await
        .expect_err("5xx");
    assert_eq!(fx.faults.fired("write_vector"), 1, "X1");
    assert_eq!(
        fx.state(0, f1.id).await.expect("row").fact.content,
        new_text
    );
    let points = fx.points(0, f1.id).await;
    assert_eq!(points.len(), 1);
    assert_eq!(
        points[0].content, f1.content,
        "W2: the point keeps the old text"
    );
    for mode in [
        RetrievalMode::Semantic,
        RetrievalMode::Hybrid,
        RetrievalMode::Rrf,
    ] {
        for query in [f1.content.as_str(), new_text.as_str()] {
            for (_, content) in search(&fx, 0, query, mode).await {
                assert!(
                    !content.contains("mountain lakes"),
                    "the old text is never served"
                );
            }
        }
    }
    assert!(
        search(&fx, 0, &new_text, RetrievalMode::Hybrid)
            .await
            .contains(&(f1.id, new_text.clone()))
    );

    // W2-R: the identical retry.
    fx.faults.reset();
    let out = ingest(&fx, updating(&new_text, f1.id), request(0, "w2"))
        .await
        .expect("200");
    assert_eq!(out.noop, 1);
    assert_eq!(out.updated, 0);
    let row = fx.state(0, f1.id).await.expect("row").fact;
    assert!(single_matching(&fx.points(0, f1.id).await, &row));
    assert!(
        search(&fx, 0, &new_text, RetrievalMode::Semantic)
            .await
            .contains(&(f1.id, new_text.clone()))
    );
    assert_eq!(fx.facts_of(0, true).await.len(), 3, "no fact added");
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn w3_missing_vector_after_pre_delete_is_healed_by_the_retry() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let f1 = fx.fact_n(0, 1).await;
    let new_text = format!("Fact 1 {} now restores old sailboats", canary(0, 1));

    // The replacement pre-deletes F1's point, then its upsert fails.
    fx.faults.arm("upsert_vector", 1, Mode::Error);
    ingest(&fx, updating(&new_text, f1.id), request(0, "w3"))
        .await
        .expect_err("5xx");
    assert_eq!(fx.faults.fired("upsert_vector"), 1, "X1");
    assert_eq!(
        fx.faults.calls("delete_by_fact_id"),
        1,
        "the fault came after the pre-delete"
    );
    assert_eq!(
        fx.state(0, f1.id).await.expect("row").fact.content,
        new_text
    );
    assert!(fx.points(0, f1.id).await.is_empty(), "W3: no F1 point");
    assert!(
        search(&fx, 0, &new_text, RetrievalMode::Semantic)
            .await
            .iter()
            .all(|r| r.0 != f1.id)
    );
    assert!(
        search(&fx, 0, &new_text, RetrievalMode::Hybrid)
            .await
            .contains(&(f1.id, new_text.clone()))
    );

    fx.faults.reset();
    let out = ingest(&fx, updating(&new_text, f1.id), request(0, "w3"))
        .await
        .expect("200");
    assert_eq!(out.noop, 1);
    let row = fx.state(0, f1.id).await.expect("row").fact;
    assert!(
        single_matching(&fx.points(0, f1.id).await, &row),
        "retry -> 1 point"
    );
    assert_eq!(fx.facts_of(0, true).await.len(), 3);
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn r4_healthy_exact_duplicate_noop_is_unchanged() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let f2 = fx.fact_n(0, 2).await;
    let before = fx.snapshot(0).await;
    let out = ingest(&fx, extracting(&f2.content), request(0, "h1"))
        .await
        .expect("200");
    assert_eq!(out.noop, 1);
    assert_eq!(out.discarded.duplicate_content, 1);
    assert_eq!(fx.faults.calls("write_vector"), 0, "no vector write");
    assert_eq!(fx.faults.calls("upsert_vector"), 0);
    assert_eq!(
        fx.faults.calls("embed_text"),
        0,
        "no embedding call for a healthy NOOP"
    );
    assert_eq!(fx.faults.calls("fact_vector_points"), 1, "one inspection");
    // Fact rows and fact points are unchanged (the request's new episode and NOOP event are not).
    let after = fx.snapshot(0).await;
    assert_eq!(
        after.lines().take(2).collect::<Vec<_>>(),
        before.lines().take(2).collect::<Vec<_>>()
    );
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn r4_several_points_are_reduced_to_one() {
    let fx = fx().await;
    let f2 = fx.fact_n(0, 2).await;
    fx.add_point(0, f2.id, &f2.content).await; // a second point for F2
    assert_eq!(fx.points(0, f2.id).await.len(), 2);
    ingest(&fx, extracting(&f2.content), request(0, "m2"))
        .await
        .expect("200");
    assert!(single_matching(&fx.points(0, f2.id).await, &f2));
}

#[tokio::test]
async fn r4_failed_embedding_is_an_error_not_a_noop() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let f2 = fx.fact_n(0, 2).await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f2.id)
        .await
        .expect("drop the point");
    // The exact-dedup NOOP needs no candidate embedding, so the first embed_text is the heal's.
    fx.faults.arm("embed_text", 1, Mode::Error);
    let err = ingest(&fx, extracting(&f2.content), request(0, "m3"))
        .await
        .expect_err("error");
    assert_eq!(fx.faults.fired("embed_text"), 1, "X1");
    assert!(matches!(err, MemcoreError::ProviderError(_)));
    assert!(
        fx.points(0, f2.id).await.is_empty(),
        "still missing; nothing acknowledged"
    );
    assert_eq!(fx.facts_of(0, true).await.len(), 3, "no duplicate fact");
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn r4_failed_vector_repair_is_an_error_not_a_noop() {
    let fx = fx().await;
    let f2 = fx.fact_n(0, 2).await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f2.id)
        .await
        .expect("drop the point");
    fx.faults.arm("write_vector", 1, Mode::Error);
    ingest(&fx, extracting(&f2.content), request(0, "m4"))
        .await
        .expect_err("error");
    assert_eq!(fx.faults.fired("write_vector"), 1, "X1");
    assert!(fx.points(0, f2.id).await.is_empty());
    assert_eq!(fx.facts_of(0, true).await.len(), 3);
}

#[tokio::test]
async fn r4_a_lost_repair_write_fails_the_verification() {
    let fx = fx().await;
    let f2 = fx.fact_n(0, 2).await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f2.id)
        .await
        .expect("drop the point");
    fx.faults.arm("write_vector", 1, Mode::Swallow);
    let err = ingest(&fx, extracting(&f2.content), request(0, "m5"))
        .await
        .expect_err("error");
    assert_eq!(fx.faults.fired("write_vector"), 1, "X1");
    assert!(
        matches!(err, MemcoreError::Conflict(_)),
        "verified once, then reported"
    );
    assert!(fx.points(0, f2.id).await.is_empty());
}

#[tokio::test]
async fn r4_retry_without_message_id_heals_facts_but_duplicates_its_episode() {
    // A retry without message ids stores its messages again.
    let fx = fx().await;
    let mut input = request(0, "x");
    input.messages[0].message_id = None;
    fx.faults.arm("write_vector", 1, Mode::Error);
    ingest(&fx, extracting(NEW_FACT), input.clone())
        .await
        .expect_err("5xx");
    assert_eq!(fx.faults.fired("write_vector"), 1, "X1");
    let episodes_after_first = fx
        .episode_store
        .list_episodes(&tenant(0), 1000)
        .await
        .unwrap()
        .len();
    fx.faults.reset();
    ingest(&fx, extracting(NEW_FACT), input).await.expect("200");
    let row = fx
        .facts_of(0, false)
        .await
        .into_iter()
        .find(|f| f.content == NEW_FACT)
        .expect("row");
    assert!(
        single_matching(&fx.points(0, row.id).await, &row),
        "the fact is healed"
    );
    let episodes = fx
        .episode_store
        .list_episodes(&tenant(0), 1000)
        .await
        .unwrap()
        .len();
    assert_eq!(
        episodes,
        episodes_after_first + 1,
        "the message is stored again (no message_id)"
    );
}

#[tokio::test]
async fn r4_limitation_a_fact_outside_the_exact_dedup_window_is_not_healed() {
    // Exact dedup reads the newest live facts of the memory type: DEDUP_SEARCH_LIMIT (50) plus the
    // stores' one pagination look-ahead row = 51. A W1 fact pushed out of that window is not
    let fx = fx().await;
    fx.faults.arm("write_vector", 1, Mode::Error);
    ingest(&fx, extracting(NEW_FACT), request(0, "w"))
        .await
        .expect_err("5xx");
    fx.faults.reset();
    let original = fx
        .facts_of(0, false)
        .await
        .into_iter()
        .find(|f| f.content == NEW_FACT)
        .unwrap();
    for i in 0..51 {
        let mut newer = original.clone();
        newer.id = Uuid::new_v4();
        newer.content = format!("Filler fact number {i} about topic {i} and nothing else");
        newer.updated_at = original.updated_at + chrono::Duration::seconds(i + 1);
        fx.raw_facts
            .insert_fact(&tenant(0), newer)
            .await
            .expect("filler");
    }
    let out = ingest(&fx, extracting(NEW_FACT), request(0, "w"))
        .await
        .expect("200");
    assert_eq!(
        out.added, 1,
        "classified as new: the window limit is a real boundary"
    );
    assert!(
        fx.points(0, original.id).await.is_empty(),
        "the original row stays unhealed"
    );
}

#[tokio::test]
async fn k1_message_id_conflict_is_unchanged_and_attempts_no_heal() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let before = fx.snapshot(0).await;
    let mut input = request(0, "e1");
    input.messages[0].content = "changed content under an existing message id".to_string();
    let err = ingest(&fx, extracting(NEW_FACT), input)
        .await
        .expect_err("409");
    assert!(err.is_message_id_conflict(), "MESSAGE_ID_CONFLICT");
    for op in [
        "write_vector",
        "upsert_vector",
        "fact_vector_points",
        "embed_text",
        "search_vectors",
    ] {
        assert_eq!(fx.faults.calls(op), 0, "no vector or embedding call: {op}");
    }
    assert_eq!(fx.snapshot(0).await, before, "unchanged");
    assert_eq!(fx.others().await, x2, "X2");
}

// ---------------------------------------------------------------------------------------------
// Group D: re-drivable delete (R3)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn d1_d2_d3_failed_cleanup_is_reported_and_retries_complete_it_once() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let f1 = fx.fact_n(0, 1).await;

    // D1
    fx.faults.arm("delete_by_fact_id", 1, Mode::Error);
    let err = delete(&fx, 0, f1.id).await.expect_err("5xx");
    assert_eq!(fx.faults.fired("delete_by_fact_id"), 1, "X1");
    assert!(!matches!(err, MemcoreError::NotFound(_)));
    assert!(
        fx.state(0, f1.id).await.expect("row").deleted,
        "soft-deleted"
    );
    assert_eq!(fx.points(0, f1.id).await.len(), 1, "the point remains");
    assert!(
        search(&fx, 0, &f1.content, RetrievalMode::Hybrid)
            .await
            .iter()
            .all(|r| r.0 != f1.id)
    );
    assert_eq!(
        fx.delete_events(0, f1.id).await,
        1,
        "the logical delete is audited once"
    );

    // D2
    fx.faults.reset();
    assert!(delete(&fx, 0, f1.id).await.expect("200"));
    assert!(fx.points(0, f1.id).await.is_empty(), "cleanup completed");
    assert_eq!(
        fx.delete_events(0, f1.id).await,
        1,
        "exactly one delete event in total"
    );

    // D3
    assert!(delete(&fx, 0, f1.id).await.expect("200"));
    assert_eq!(
        fx.delete_events(0, f1.id).await,
        1,
        "no new event; idempotent"
    );
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn d1_deleted_fact_is_invisible_to_dedup_before_cleanup() {
    let fx = fx().await;
    let f1 = fx.fact_n(0, 1).await;
    fx.faults.arm("delete_by_fact_id", 1, Mode::Error);
    delete(&fx, 0, f1.id).await.expect_err("5xx");
    assert_eq!(fx.faults.fired("delete_by_fact_id"), 1, "X1");
    fx.faults.reset();
    let out = ingest(&fx, extracting(&f1.content), request(0, "d"))
        .await
        .expect("200");
    assert_eq!(out.added, 1, "the orphan point suppresses nothing (R2)");
}

#[tokio::test]
async fn d4_unknown_and_other_tenant_ids_are_indistinguishable_404s() {
    let fx = fx().await;
    let x2_c = fx.snapshot(2).await;
    let b1 = fx.fact_n(1, 1).await;
    let b2 = fx.fact_n(1, 2).await;
    fx.raw_facts
        .soft_delete_fact(&tenant(1), b2.id)
        .await
        .expect("soft delete TB F2");
    let b_before = fx.snapshot(1).await;
    let mut messages = Vec::new();
    for id in [Uuid::new_v4(), b1.id, b2.id] {
        let err = delete(&fx, 0, id).await.expect_err("404");
        assert!(matches!(err, MemcoreError::NotFound(_)));
        messages.push(err.to_string());
    }
    assert!(
        messages.windows(2).all(|w| w[0] == w[1]),
        "identical errors"
    );
    assert_eq!(
        fx.faults.calls("delete_by_fact_id"),
        0,
        "no cleanup attempted"
    );
    assert_eq!(
        fx.points(1, b2.id).await.len(),
        1,
        "TB's orphan point untouched"
    );
    assert_eq!(fx.snapshot(1).await, b_before, "X2: TB unchanged");
    assert_eq!(fx.snapshot(2).await, x2_c, "X2: TC unchanged");
}

#[tokio::test]
async fn r3_live_fact_without_a_point_deletes_successfully() {
    // Case E: a missing point is completed cleanup (0599dcf answered 404 here after the soft delete).
    let fx = fx().await;
    let f3 = fx.fact_n(0, 3).await;
    fx.raw_vectors
        .delete_by_fact_id(&tenant(0), f3.id)
        .await
        .expect("drop the point");
    assert!(delete(&fx, 0, f3.id).await.expect("200"));
    assert!(fx.state(0, f3.id).await.expect("row").deleted);
    assert_eq!(fx.delete_events(0, f3.id).await, 1);
}

#[tokio::test]
async fn r3_failed_soft_delete_changes_nothing() {
    let fx = fx().await;
    let f3 = fx.fact_n(0, 3).await;
    let before = fx.snapshot(0).await;
    fx.faults.arm("soft_delete_fact", 1, Mode::Error);
    delete(&fx, 0, f3.id).await.expect_err("error");
    assert_eq!(fx.faults.fired("soft_delete_fact"), 1, "X1");
    assert_eq!(fx.snapshot(0).await, before);
}

// ---------------------------------------------------------------------------------------------
// Group W8: fail-closed append import (R7)
// ---------------------------------------------------------------------------------------------

async fn export(fx: &Fx, index: usize) -> UserMemoryExport {
    fx.plain()
        .export_user_data(ExportUserDataInput {
            tenant: tenant(index),
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export")
}

async fn import(
    fx: &Fx,
    index: usize,
    export: UserMemoryExport,
    mode: ImportMode,
) -> MemcoreResult<usize> {
    fx.plain()
        .import_user_data(ImportUserDataInput {
            tenant: tenant(index),
            export,
            mode,
            restore_events: false,
            dry_run: false,
        })
        .await
        .map(|out| out.imported_facts)
}

async fn forget(fx: &Fx, index: usize) {
    fx.plain()
        .forget_user(ForgetUserInput {
            tenant: tenant(index),
        })
        .await
        .expect("forget");
}

#[tokio::test]
async fn w8_a_b_c_partial_append_is_rejected_on_retry_and_replace_recovers() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let x = export(&fx, 0).await;
    assert_eq!(x.facts.len(), 3);
    let episodes_in_x = x
        .episodes
        .as_ref()
        .map(Vec::len)
        .expect("episodes exported");
    forget(&fx, 0).await;
    fx.faults.reset();

    // W8-A: the import's second vector write fails.
    fx.faults.arm("upsert_vector", 2, Mode::Error);
    import(&fx, 0, x.clone(), ImportMode::Append)
        .await
        .expect_err("5xx");
    assert_eq!(fx.faults.fired("upsert_vector"), 1, "X1");
    let partial = fx.facts_of(0, true).await;
    assert_eq!(partial.len(), 2, "2 facts inserted");
    // Protocol amendment v1.1 §2 (S3I): W8-A's "ids as exported" became "ids under t(f)"; TA owns
    // none of X's ids after forget-user, so t(f) = M(TA, f).
    assert!(
        partial.iter().all(|f| x
            .facts
            .iter()
            .any(|e| derived_import_fact_id(&tenant(0), e.id) == f.id)),
        "ids under t(f) = M(TA, f)"
    );
    let points: usize = {
        let mut n = 0;
        for f in &partial {
            n += fx.points(0, f.id).await.len();
        }
        n
    };
    assert_eq!(points, 1, "1 point");

    // W8-B: the same append import.
    fx.faults.reset();
    let before = fx.snapshot(0).await;
    let err = import(&fx, 0, x.clone(), ImportMode::Append)
        .await
        .expect_err("409");
    assert!(err.is_import_already_applied(), "IMPORT_ALREADY_APPLIED");
    assert_eq!(fx.snapshot(0).await, before, "no write of any kind");
    for op in ["insert_fact", "upsert_vector", "write_vector"] {
        assert_eq!(fx.faults.calls(op), 0, "rejected before any write: {op}");
    }
    assert_no_foreign_canary(&err.to_string(), 0);
    for fact in &x.facts {
        assert!(
            !err.to_string().contains(&fact.id.to_string()),
            "no id in the message"
        );
        assert!(
            !err.to_string().contains(&fact.content),
            "no content in the message"
        );
    }

    // W8-C: replace import recovers exactly.
    assert_eq!(
        import(&fx, 0, x.clone(), ImportMode::Replace)
            .await
            .expect("200"),
        3
    );
    let restored = fx.facts_of(0, true).await;
    let mut ids: Vec<Uuid> = restored.iter().map(|f| f.id).collect();
    // Amendment v1.1 §2: W8-C's "ids as exported" became "ids under t(f)" (here M(TA, f)).
    let mut exported: Vec<Uuid> = x
        .facts
        .iter()
        .map(|f| derived_import_fact_id(&tenant(0), f.id))
        .collect();
    ids.sort();
    exported.sort();
    assert_eq!(ids, exported, "exactly X's facts, ids under t(f)");
    for fact in &restored {
        assert!(
            single_matching(&fx.points(0, fact.id).await, fact),
            "1 point per fact"
        );
    }
    let episodes = fx
        .episode_store
        .list_episodes(&tenant(0), 1000)
        .await
        .unwrap()
        .len();
    assert_eq!(episodes, episodes_in_x, "episodes per X");
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn w8_d_live_identical_ids_win_over_deleted_ids_and_d2_reports_the_deleted_conflict() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let x = export(&fx, 0).await;
    let ids: Vec<Uuid> = x.facts.iter().map(|f| f.id).collect();

    // W8-D: F1 deleted, F2 / F3 live and identical.
    delete(&fx, 0, ids[0]).await.expect("delete F1");
    fx.faults.reset();
    let before = fx.snapshot(0).await;
    let err = import(&fx, 0, x.clone(), ImportMode::Append)
        .await
        .expect_err("409");
    assert!(
        err.is_import_already_applied(),
        "IMPORT_ALREADY_APPLIED takes precedence"
    );
    assert_eq!(fx.snapshot(0).await, before);

    // W8-D2: all three deleted.
    for id in &ids[1..] {
        delete(&fx, 0, *id).await.expect("delete");
    }
    fx.faults.reset();
    let before = fx.snapshot(0).await;
    let err = import(&fx, 0, x, ImportMode::Append)
        .await
        .expect_err("409");
    assert!(err.is_import_id_conflict(), "IMPORT_ID_CONFLICT");
    assert_eq!(fx.snapshot(0).await, before, "no write");
    assert_eq!(fx.faults.calls("insert_fact"), 0);
    assert_eq!(fx.others().await, x2, "X2");
}

#[tokio::test]
async fn w8_e_episode_only_export_without_message_ids_duplicates_as_documented() {
    let fx = fx().await;
    // A fourth, fresh tenant, so no fixture data is involved.
    let te = TenantContext::new("org_e", "ue").expect("tenant");
    let messages = (0..3)
        .map(|n| MemoryMessage {
            role: MessageRole::User,
            content: format!("Episode-only message {n} without any message id"),
            ..Default::default()
        })
        .collect();
    fx.engine(MockLlmProvider::new().with_extraction_candidates(Vec::new()))
        .add_memory(AddMemoryInput {
            tenant: te.clone(),
            messages,
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("episodes only");
    let x = fx
        .plain()
        .export_user_data(ExportUserDataInput {
            tenant: te.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export");
    assert!(x.facts.is_empty());
    let n = x.episodes.as_ref().map(Vec::len).unwrap();
    assert_eq!(n, 3);
    fx.plain()
        .forget_user(ForgetUserInput { tenant: te.clone() })
        .await
        .expect("forget");
    for round in 1..=2 {
        fx.plain()
            .import_user_data(ImportUserDataInput {
                tenant: te.clone(),
                export: x.clone(),
                mode: ImportMode::Append,
                restore_events: false,
                dry_run: false,
            })
            .await
            .expect("200");
        let stored = fx
            .episode_store
            .list_episodes(&te, 1000)
            .await
            .unwrap()
            .len();
        assert_eq!(
            stored,
            n * round,
            "duplicates on re-application (documented limitation)"
        );
    }
    let doc = include_str!("../../../docs/operations.md");
    assert!(
        doc.contains("**Limitation (exports without facts):**"),
        "the import contract must document this behaviour"
    );
}

#[tokio::test]
async fn r7_live_id_with_different_content_keeps_the_re_key_behaviour() {
    let fx = fx().await;
    let mut x = export(&fx, 0).await;
    for fact in &mut x.facts {
        fact.content = format!("{} (edited after export)", fact.content);
    }
    assert_eq!(
        import(&fx, 0, x.clone(), ImportMode::Append)
            .await
            .expect("200"),
        3
    );
    let all = fx.facts_of(0, true).await;
    assert_eq!(all.len(), 6, "imported under new ids");
    for fact in &x.facts {
        let copy = all
            .iter()
            .find(|f| f.content == fact.content)
            .expect("imported");
        assert_ne!(copy.id, fact.id, "re-keyed");
    }
}

#[tokio::test]
async fn r7_nonconflicting_import_stores_tenant_derived_ids() {
    // S3I (amendment v1.1 §4): before C+ this asserted "exported id kept". A non-conflicting import
    // still succeeds with no R7 code; TA owns none of the ids after forget-user, so each fact is
    // stored under M(TA, f), never under the exported id.
    let fx = fx().await;
    let x = export(&fx, 0).await;
    forget(&fx, 0).await;
    assert_eq!(
        import(&fx, 0, x.clone(), ImportMode::Append)
            .await
            .expect("200"),
        3
    );
    for fact in &x.facts {
        let derived = derived_import_fact_id(&tenant(0), fact.id);
        let stored = fx.state(0, derived).await.expect("stored under M(TA, f)");
        assert_eq!(stored.fact.content, fact.content);
        assert!(
            fx.state(0, fact.id).await.is_none(),
            "exported id not reused"
        );
    }
}

#[tokio::test]
async fn r7_other_tenants_ids_never_produce_an_r7_code() {
    // A tenant-A export naming tenant B's fact ids (live, and soft-deleted). R7's own reads are
    // tenant-scoped, so neither import gets an IMPORT_* code. Before S3I the insert then collided
    // at the global primary key (the mock answered CONFLICT, SQL a storage error): the cross-tenant
    // signal of the S3I design. Under C+ (amendment v1.1 §4) TA does not own B's ids, so the fact
    // is stored under M(TA, id): the import succeeds and TB is untouched.
    let fx = fx().await;
    let b1 = fx.fact_n(1, 1).await;
    let b2 = fx.fact_n(1, 2).await;
    fx.raw_facts
        .soft_delete_fact(&tenant(1), b2.id)
        .await
        .expect("soft delete");
    let b_before = fx.snapshot(1).await;
    let mut x = export(&fx, 0).await;
    x.facts.truncate(1);
    for target in [b1.id, b2.id] {
        let mut crafted = x.clone();
        crafted.facts[0].id = target;
        assert_eq!(
            import(&fx, 0, crafted, ImportMode::Append)
                .await
                .expect("no R7 code and no collision"),
            1
        );
        let stored = fx
            .state(0, derived_import_fact_id(&tenant(0), target))
            .await
            .expect("stored under M(TA, id)");
        assert_eq!(stored.fact.content, x.facts[0].content);
    }
    assert_eq!(fx.snapshot(1).await, b_before, "TB unchanged");
}
