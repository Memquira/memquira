//! Concurrent equivalent fact additions must not create duplicate facts.
//!
//! Deterministic interleaving: a fact-store wrapper holds the first N fact inserts at a barrier
//! until all N concurrent requests have reached it, i.e. until each has passed every dedup check
//! (exact normalized text, token overlap, embedding similarity, LLM classification) against a store
//! that does not yet hold the other's fact. Before the fix both then inserted (reproduced on the
//! mock and real SQLite: two facts). Across OS processes the same rendezvous uses marker files.
//! Mock, real SQLite (file, pooled connections) and, with `MEMCORE_TEST_POSTGRES_URL` and the
//! `postgres` feature, real PostgreSQL (each case on its own invented tenant). Invented data only.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    DedupInsert, FactFulltextQuery, MemoryEventQuery, MemoryEventStore, OrgUserListQuery,
    OrgUserSummary, RetentionPruneResult, ScoredFact, StoredFactState, VectorPointState,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, ConsolidationConfig, DeleteMemoryInput,
    ExactDedupRule, ExportUserDataInput, Fact, FactSearchQuery, FactStore, ImportMode,
    ImportUserDataInput, MemoryEngine, MemoryEventOperation, MemoryMessage, MemoryType,
    MessageRole, TemporalGranularity, TemporalValue, TenantContext, VectorRecord,
    VectorSearchQuery, VectorSearchResult, VectorStore, VectorWriteIntent, exact_dedup_key,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore, SqliteFactStore};
use serde_json::json;
use tokio::sync::Barrier;
use uuid::Uuid;

const TEXT: &str = "User prefers green tea in the morning.";
/// Equivalent under the exact rule (case, whitespace, trailing punctuation).
const TEXT_VARIANT: &str = "  user prefers GREEN tea in the   morning ";

// ---------------------------------------------------------------------------------------------
// Interleaving control
// ---------------------------------------------------------------------------------------------

enum Rendezvous {
    /// In-process: the first `parties` inserts wait for each other.
    Barrier(Arc<Barrier>, AtomicUsize),
    /// Cross-process: write `ready`, then wait until `go` exists.
    Files { ready: PathBuf, go: PathBuf },
}

/// Holds fact inserts at a rendezvous, after every dedup read of the request and before the write.
struct Gate {
    inner: Arc<dyn FactStore>,
    rendezvous: Rendezvous,
    reached: AtomicUsize,
}

impl Gate {
    fn barrier(inner: Arc<dyn FactStore>, parties: usize) -> Arc<Self> {
        Arc::new(Self {
            inner,
            rendezvous: Rendezvous::Barrier(
                Arc::new(Barrier::new(parties)),
                AtomicUsize::new(parties),
            ),
            reached: AtomicUsize::new(0),
        })
    }
    async fn wait(&self) {
        self.reached.fetch_add(1, Ordering::SeqCst);
        match &self.rendezvous {
            Rendezvous::Barrier(barrier, armed) => {
                if armed
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
                {
                    barrier.wait().await;
                }
            }
            Rendezvous::Files { ready, go } => {
                if !ready.exists() {
                    std::fs::write(ready, b"ready").unwrap();
                    wait_for(std::slice::from_ref(go)).await;
                }
            }
        }
    }
}

/// Waits (event-driven by the other side's file write, polled) for every path to exist.
async fn wait_for(paths: &[PathBuf]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !paths.iter().all(|p| p.exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "rendezvous timed out: {paths:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[async_trait]
impl FactStore for Gate {
    async fn insert_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        self.wait().await;
        self.inner.insert_fact(t, f).await
    }
    async fn insert_fact_deduplicated(
        &self,
        t: &TenantContext,
        f: Fact,
        key: &str,
    ) -> MemcoreResult<DedupInsert> {
        self.wait().await;
        self.inner.insert_fact_deduplicated(t, f, key).await
    }
    async fn update_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        self.inner.update_fact(t, f).await
    }
    async fn get_fact(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<Option<Fact>> {
        self.inner.get_fact(t, id).await
    }
    async fn get_fact_including_deleted(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.inner.get_fact_including_deleted(t, id).await
    }
    async fn list_fact_states(
        &self,
        t: &TenantContext,
        a: Option<Uuid>,
        l: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        self.inner.list_fact_states(t, a, l).await
    }
    async fn fact_id_exists_in_any_tenant(&self, id: Uuid) -> MemcoreResult<bool> {
        self.inner.fact_id_exists_in_any_tenant(id).await
    }
    async fn search_facts(&self, q: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        self.inner.search_facts(q).await
    }
    async fn fulltext_search_facts(&self, q: FactFulltextQuery) -> MemcoreResult<Vec<ScoredFact>> {
        self.inner.fulltext_search_facts(q).await
    }
    async fn soft_delete_fact(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        self.inner.soft_delete_fact(t, id).await
    }
    async fn delete_user_data(&self, t: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_user_data(t).await
    }
    async fn delete_facts_older_than(
        &self,
        t: &TenantContext,
        c: DateTime<Utc>,
        d: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        self.inner.delete_facts_older_than(t, c, d).await
    }
    async fn count_facts_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.inner.count_facts_by_org(o).await
    }
    async fn count_active_facts_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.inner.count_active_facts_by_org(o).await
    }
    async fn count_facts_by_user(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.inner.count_facts_by_user(t).await
    }
    async fn count_users_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.inner.count_users_by_org(o).await
    }
    async fn list_users_by_org(&self, q: OrgUserListQuery) -> MemcoreResult<Vec<OrgUserSummary>> {
        self.inner.list_users_by_org(q).await
    }
}

/// Fails the first `fail` fresh-fact vector writes AFTER nothing was written (the SQL row of the
/// add is already committed then: "SQL write succeeded, vector write failed").
struct FailingVectors {
    inner: Arc<MockVectorStore>,
    fail: AtomicUsize,
    failed: AtomicUsize,
}

#[async_trait]
impl VectorStore for FailingVectors {
    async fn upsert_vector(&self, t: &TenantContext, r: VectorRecord) -> MemcoreResult<()> {
        self.inner.upsert_vector(t, r).await
    }
    async fn write_vector(
        &self,
        t: &TenantContext,
        r: VectorRecord,
        i: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        if i == VectorWriteIntent::FreshFact
            && self
                .fail
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        {
            self.failed.fetch_add(1, Ordering::SeqCst);
            return Err(MemcoreError::Internal(
                "injected vector write failure".into(),
            ));
        }
        self.inner.write_vector(t, r, i).await
    }
    async fn search_vectors(&self, q: VectorSearchQuery) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(q).await
    }
    async fn delete_by_fact_id(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        self.inner.delete_by_fact_id(t, id).await
    }
    async fn delete_by_user(&self, t: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_by_user(t).await
    }
    async fn fact_vector_points(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.inner.fact_vector_points(t, id).await
    }
}

// ---------------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    Mock,
    Sqlite,
    Postgres,
}

/// One backend's shared stores (one database, the shared vector store and event log).
struct Fx {
    backend: Backend,
    facts: Arc<dyn FactStore>,
    vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    /// `Some` runs the engines with consolidation v2, `None` the default legacy path.
    consolidation: std::cell::Cell<Option<ConsolidationConfig>>,
    _dir: Option<tempfile::TempDir>,
}

impl Fx {
    async fn new(backend: Backend) -> Option<Self> {
        let (facts, dir): (Arc<dyn FactStore>, _) = match backend {
            Backend::Mock => (Arc::new(MockFactStore::new()), None),
            Backend::Sqlite => {
                let dir = tempfile::tempdir().unwrap();
                let url = sqlite_url(dir.path());
                (
                    Arc::new(SqliteFactStore::connect(&url).await.unwrap()),
                    Some(dir),
                )
            }
            Backend::Postgres => (postgres_store().await?, None),
        };
        Some(Self {
            backend,
            facts,
            vectors: Arc::new(MockVectorStore::new()),
            events: Arc::new(MockMemoryEventStore::new()),
            consolidation: std::cell::Cell::new(None),
            _dir: dir,
        })
    }

    /// An engine (as one API request handler) that extracts `text` as `kind`.
    fn engine(
        &self,
        facts: Arc<dyn FactStore>,
        vectors: Arc<dyn VectorStore>,
        text: &str,
        kind: MemoryType,
    ) -> MemoryEngine {
        self.engine_for(facts, vectors, candidate(text, kind))
    }

    fn engine_for(
        &self,
        facts: Arc<dyn FactStore>,
        vectors: Arc<dyn VectorStore>,
        candidate: CandidateFact,
    ) -> MemoryEngine {
        engine(
            facts,
            vectors,
            self.events.clone(),
            candidate,
            self.consolidation.get(),
        )
    }

    fn mode(&self) -> &'static str {
        if self.consolidation.get().is_some() {
            "v2"
        } else {
            "legacy"
        }
    }

    async fn race(
        &self,
        adds: &[(&TenantContext, &str, MemoryType)],
    ) -> Vec<MemcoreResult<AddMemoryOutput>> {
        let vectors: Arc<dyn VectorStore> = self.vectors.clone();
        self.race_with(adds, vectors, true).await
    }

    /// Runs every addition concurrently, all held at the insert until each passed its dedup reads.
    async fn race_with(
        &self,
        adds: &[(&TenantContext, &str, MemoryType)],
        vectors: Arc<dyn VectorStore>,
        check_reached: bool,
    ) -> Vec<MemcoreResult<AddMemoryOutput>> {
        let adds: Vec<_> = adds
            .iter()
            .map(|(t, text, kind)| (*t, candidate(text, *kind)))
            .collect();
        self.race_candidates(&adds, vectors, check_reached).await
    }

    /// As [`Self::race_with`], with the extracted candidate of each request given in full.
    async fn race_candidates(
        &self,
        adds: &[(&TenantContext, CandidateFact)],
        vectors: Arc<dyn VectorStore>,
        check_reached: bool,
    ) -> Vec<MemcoreResult<AddMemoryOutput>> {
        let gate = Gate::barrier(self.facts.clone(), adds.len());
        let gated: Arc<dyn FactStore> = gate.clone();
        let handles: Vec<_> = adds
            .iter()
            .map(|(t, candidate)| {
                let engine = self.engine_for(gated.clone(), vectors.clone(), candidate.clone());
                let input = input(t, &candidate.content);
                tokio::spawn(async move { engine.add_memory(input).await })
            })
            .collect();
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        if check_reached {
            assert_eq!(
                gate.reached.load(Ordering::SeqCst),
                adds.len(),
                "every request reached the insert"
            );
        }
        results
    }

    async fn active(&self, t: &TenantContext) -> Vec<Fact> {
        active(self.facts.as_ref(), t).await
    }

    async fn points(&self, fact_id: Uuid, t: &TenantContext) -> usize {
        self.vectors
            .fact_vector_points(t, fact_id)
            .await
            .unwrap()
            .expect("the mock reports points")
            .len()
    }

    async fn events(
        &self,
        t: &TenantContext,
        op: MemoryEventOperation,
    ) -> Vec<memcore_core::MemoryEvent> {
        self.events
            .list_events(MemoryEventQuery {
                operation: Some(op),
                ..MemoryEventQuery::new(t.clone(), 100)
            })
            .await
            .unwrap()
    }

    fn tenant(&self, label: &str) -> TenantContext {
        // A fresh tenant per case keeps cases independent on the shared PostgreSQL database.
        tenant(
            &format!("org_ar086_{label}_{}", Uuid::new_v4().simple()),
            "user_1",
        )
    }
}

fn sqlite_url(dir: &Path) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.join("ar086.db").to_string_lossy().replace('\\', "/")
    )
}

/// Serialises this binary's PostgreSQL connects: the migration runner's first `CREATE TABLE IF
/// NOT EXISTS` on a fresh database is not safe against a concurrent first run (a pre-existing
/// runner property), and the tests of one binary run in parallel.
#[cfg(feature = "postgres")]
static PG_CONNECT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(feature = "postgres")]
async fn connect_postgres(url: &str) -> Arc<dyn FactStore> {
    let _serial = PG_CONNECT.lock().await;
    Arc::new(
        memcore_storage::PostgresFactStore::connect(url)
            .await
            .unwrap(),
    )
}

async fn postgres_store() -> Option<Arc<dyn FactStore>> {
    #[cfg(feature = "postgres")]
    {
        let Ok(url) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
            eprintln!("AR086_POSTGRES_SKIPPED: MEMCORE_TEST_POSTGRES_URL not set");
            return None;
        };
        eprintln!("AR086_POSTGRES_RAN");
        Some(connect_postgres(&url).await)
    }
    #[cfg(not(feature = "postgres"))]
    {
        eprintln!("AR086_POSTGRES_SKIPPED: postgres feature not enabled");
        None
    }
}

async fn open_store(database: &str) -> Arc<dyn FactStore> {
    if database.starts_with("postgres") {
        #[cfg(feature = "postgres")]
        {
            return connect_postgres(database).await;
        }
        #[cfg(not(feature = "postgres"))]
        panic!("postgres feature not enabled");
    }
    Arc::new(SqliteFactStore::connect(database).await.unwrap())
}

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).unwrap()
}

fn candidate(text: &str, kind: MemoryType) -> CandidateFact {
    CandidateFact::new(text, kind, 0.9, 0.8, None, json!({})).unwrap()
}

fn engine(
    facts: Arc<dyn FactStore>,
    vectors: Arc<dyn VectorStore>,
    events: Arc<MockMemoryEventStore>,
    candidate: CandidateFact,
    consolidation: Option<ConsolidationConfig>,
) -> MemoryEngine {
    MemoryEngine::new(
        facts,
        vectors,
        Arc::new(MockLlmProvider::new().with_extraction_candidates(vec![candidate])),
        Arc::new(MockEmbeddingProvider::new(32)),
    )
    .with_event_store(events)
    .with_min_importance(0.0)
    .with_consolidation(consolidation)
}

/// A day-long event time on day `day` of January 2026 (the v2 exact rule compares start and end).
fn event_day(day: u32) -> TemporalValue {
    let start = Utc.with_ymd_and_hms(2026, 1, day, 0, 0, 0).unwrap();
    TemporalValue {
        start,
        end: start + chrono::Duration::days(1),
        granularity: TemporalGranularity::Day,
        anchor: start,
    }
}

fn with_event_day(mut fact: Fact, day: u32) -> Fact {
    fact.structure.event_time = Some(event_day(day));
    fact
}

fn candidate_on_day(text: &str, day: u32) -> CandidateFact {
    let mut candidate = candidate(text, MemoryType::Preference);
    candidate.structure.event_time = Some(event_day(day));
    candidate
}

fn input(t: &TenantContext, text: &str) -> AddMemoryInput {
    AddMemoryInput {
        tenant: t.clone(),
        messages: vec![MemoryMessage {
            role: MessageRole::User,
            content: text.to_string(),
            ..Default::default()
        }],
        metadata: json!({}),
        session: Default::default(),
    }
}

async fn active(store: &dyn FactStore, t: &TenantContext) -> Vec<Fact> {
    store
        .search_facts(FactSearchQuery {
            tenant: t.clone(),
            memory_types: None,
            query_text: None,
            limit: 1000,
            cursor: None,
            include_deleted: false,
        })
        .await
        .unwrap()
}

fn new_fact(t: &TenantContext, content: &str) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        t.org_id.clone(),
        t.user_id.clone(),
        MemoryType::Preference,
        content,
        None,
        memcore_core::MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        now,
        now,
        json!({}),
    )
    .unwrap()
}

fn added(results: &[MemcoreResult<AddMemoryOutput>]) -> usize {
    results.iter().map(|r| r.as_ref().unwrap().added).sum()
}

// ---------------------------------------------------------------------------------------------
// Cases (each runs on one backend)
// ---------------------------------------------------------------------------------------------

/// Acceptance 1 / 4: N concurrent equivalent additions in one tenant store exactly one fact with
/// one vector; every request succeeds; one add event and N-1 duplicate NOOP events naming it.
async fn case_same_tenant(fx: &Fx, n: usize) {
    let t = fx.tenant("same");
    let texts = [
        TEXT,
        TEXT_VARIANT,
        "User prefers green tea in the morning!",
        TEXT,
    ];
    let adds: Vec<_> = (0..n)
        .map(|i| (&t, texts[i % texts.len()], MemoryType::Preference))
        .collect();
    let results = fx.race(&adds).await;
    let facts = fx.active(&t).await;
    assert_eq!(
        facts.len(),
        1,
        "{:?}: {n} concurrent equivalent additions stored {} facts",
        fx.backend,
        facts.len()
    );
    assert_eq!(added(&results), 1, "{:?}", fx.backend);
    let winner = &facts[0];
    assert_eq!(
        fx.points(winner.id, &t).await,
        1,
        "{:?}: one vector point",
        fx.backend
    );
    assert_eq!(fx.events(&t, MemoryEventOperation::Add).await.len(), 1);
    let noops = fx.events(&t, MemoryEventOperation::NoOp).await;
    assert_eq!(noops.len(), n - 1, "{:?}", fx.backend);
    assert!(
        noops.iter().all(|e| e.fact_id == Some(winner.id)),
        "{:?}: NOOPs name the stored fact",
        fx.backend
    );
    let discarded: usize = results
        .iter()
        .map(|r| r.as_ref().unwrap().discarded.duplicate_content)
        .sum();
    assert_eq!(discarded, n - 1, "{:?}", fx.backend);
}

/// Acceptance 2: the same text for different tenants (other user of the org, other org) at once.
async fn case_cross_tenant(fx: &Fx) {
    let a = fx.tenant("xa");
    let b = tenant(&a.org_id, "user_2");
    let c = fx.tenant("xc");
    let results = fx
        .race(&[
            (&a, TEXT, MemoryType::Preference),
            (&b, TEXT, MemoryType::Preference),
            (&c, TEXT, MemoryType::Preference),
        ])
        .await;
    assert_eq!(added(&results), 3, "{:?}", fx.backend);
    for t in [&a, &b, &c] {
        let facts = fx.active(t).await;
        assert_eq!(facts.len(), 1, "{:?} {t:?}", fx.backend);
        assert_eq!(
            (facts[0].org_id.as_str(), facts[0].user_id.as_str()),
            (t.org_id.as_str(), t.user_id.as_str())
        );
        assert_eq!(fx.points(facts[0].id, t).await, 1);
    }
}

/// Acceptance 3: distinct facts are not merged: other content (memory type, subject and event
/// time are mode-specific, see `case_exact_rules`).
async fn case_distinct(fx: &Fx) {
    let t = fx.tenant("distinct");
    let results = fx
        .race(&[
            (&t, TEXT, MemoryType::Preference),
            (
                &t,
                "User prefers black coffee in the evening.",
                MemoryType::Preference,
            ),
        ])
        .await;
    assert_eq!(added(&results), 2, "{:?} {}", fx.backend, fx.mode());
    assert_eq!(fx.active(&t).await.len(), 2, "{:?}", fx.backend);
}

/// Acceptance 5: the winning request's SQL insert commits and its vector write fails; the losing
/// request is a NOOP (no vector heal racing the winner). A retry heals the vector (R4): one fact,
/// one point, and the retry adds nothing.
async fn case_vector_failure_retry(fx: &Fx) {
    let t = fx.tenant("vfail");
    let failing = Arc::new(FailingVectors {
        inner: fx.vectors.clone(),
        fail: AtomicUsize::new(1),
        failed: AtomicUsize::new(0),
    });
    let results = fx
        .race_with(
            &[
                (&t, TEXT, MemoryType::Preference),
                (&t, TEXT_VARIANT, MemoryType::Preference),
            ],
            failing.clone(),
            true,
        )
        .await;
    assert_eq!(failing.failed.load(Ordering::SeqCst), 1, "the fault fired");
    let (errs, oks): (Vec<_>, Vec<_>) = results.into_iter().partition(|r| r.is_err());
    assert_eq!(
        (errs.len(), oks.len()),
        (1, 1),
        "{:?}: the winner fails, the loser is a NOOP",
        fx.backend
    );
    assert_eq!(oks[0].as_ref().unwrap().added, 0);
    let facts = fx.active(&t).await;
    assert_eq!(facts.len(), 1, "{:?}", fx.backend);
    assert_eq!(
        fx.points(facts[0].id, &t).await,
        0,
        "the failed write left no point"
    );

    let vectors: Arc<dyn VectorStore> = fx.vectors.clone();
    let retry = fx
        .engine(fx.facts.clone(), vectors, TEXT, MemoryType::Preference)
        .add_memory(input(&t, TEXT))
        .await
        .unwrap();
    assert_eq!(
        retry.added,
        0,
        "{:?} {}: the retry adds nothing",
        fx.backend,
        fx.mode()
    );
    let facts = fx.active(&t).await;
    assert_eq!(facts.len(), 1, "{:?} {}", fx.backend, fx.mode());
    let points = fx.points(facts[0].id, &t).await;
    if fx.consolidation.get().is_none() {
        assert_eq!(
            points, 1,
            "{:?}: the retry healed the vector (R4)",
            fx.backend
        );
    } else {
        // Consolidation v2 has no R4 heal (unchanged); verify-vectors (R5) reports it.
        eprintln!("AR086_V2_RETRY backend={:?} points={points}", fx.backend);
    }
}

/// Each ingestion mode's own exact rule under concurrency.
/// Legacy (`detect_duplicate`): same memory type and normalized text is one fact, whatever the
/// subject or event time. Consolidation v2 (`fast_path`, related facts of every type): same
/// normalized text, subject and event time is one fact, whatever the memory type; another subject
/// or event time is a distinct fact.
async fn case_exact_rules(fx: &Fx) {
    let v2 = fx.consolidation.get().is_some();
    let race = |adds: Vec<(TenantContext, CandidateFact)>| async move {
        let adds: Vec<_> = adds.iter().map(|(t, c)| (t, c.clone())).collect();
        fx.race_candidates(&adds, fx.vectors.clone(), true).await
    };
    // Same type and text; other event days and a subject.
    let t = fx.tenant("rule_structure");
    let mut with_subject = candidate(TEXT_VARIANT, MemoryType::Preference);
    with_subject.structure.subject = Some("Alice".into());
    let results = race(vec![
        (t.clone(), candidate_on_day(TEXT, 5)),
        (t.clone(), candidate_on_day(TEXT, 6)),
        (t.clone(), with_subject),
    ])
    .await;
    let expected = if v2 { 3 } else { 1 };
    assert_eq!(
        (added(&results), fx.active(&t).await.len()),
        (expected, expected),
        "{:?} {}: same type and text, other subject / event time",
        fx.backend,
        fx.mode()
    );
    // Same text, subject and event time; another memory type.
    let t = fx.tenant("rule_type");
    let mut skill = candidate_on_day(TEXT_VARIANT, 5);
    skill.memory_type = MemoryType::Skill;
    let results = race(vec![
        (t.clone(), candidate_on_day(TEXT, 5)),
        (t.clone(), skill),
    ])
    .await;
    let expected = if v2 { 1 } else { 2 };
    assert_eq!(
        (added(&results), fx.active(&t).await.len()),
        (expected, expected),
        "{:?} {}: same text, subject and event time, other memory type",
        fx.backend,
        fx.mode()
    );
    // Equal under both rules: one fact in both modes.
    let t = fx.tenant("rule_same");
    let results = race(vec![
        (t.clone(), candidate_on_day(TEXT, 5)),
        (t.clone(), candidate_on_day(TEXT_VARIANT, 5)),
    ])
    .await;
    assert_eq!(
        (added(&results), fx.active(&t).await.len()),
        (1, 1),
        "{:?} {}",
        fx.backend,
        fx.mode()
    );
}

/// Switching ingestion modes on one store: keys are tagged by rule, so a fact
/// keyed by the default path never blocks a fact that consolidation v2 calls distinct, v2 still
/// enforces its own key on that tenant, and back on the default path its sequential exact rule
/// still sees every stored fact.
async fn case_mode_switch(fx: &Fx) {
    let previous = fx.consolidation.get();
    let t = fx.tenant("mode_switch");
    fx.consolidation.set(None);
    let results = fx.race(&[(&t, TEXT, MemoryType::Preference)]).await;
    assert_eq!(added(&results), 1, "{:?}", fx.backend);

    // v2: another subject is a distinct fact; two concurrent requests store exactly one.
    fx.consolidation.set(Some(ConsolidationConfig::default()));
    let mut alice = candidate(TEXT, MemoryType::Preference);
    alice.structure.subject = Some("Alice".into());
    let mut alice_variant = candidate(TEXT_VARIANT, MemoryType::Preference);
    alice_variant.structure.subject = Some(" alice".into());
    let results = fx
        .race_candidates(
            &[(&t, alice), (&t, alice_variant)],
            fx.vectors.clone(),
            true,
        )
        .await;
    assert_eq!(
        (added(&results), fx.active(&t).await.len()),
        (1, 2),
        "{:?}: v2 after the default path",
        fx.backend
    );

    // Back on the default path: same type and text is a duplicate of the stored facts, whatever
    // the subject, so concurrent requests add nothing (caught before the insert).
    fx.consolidation.set(None);
    let mut bob = candidate(TEXT, MemoryType::Preference);
    bob.structure.subject = Some("Bob".into());
    let results = fx
        .race_candidates(&[(&t, bob.clone()), (&t, bob)], fx.vectors.clone(), false)
        .await;
    assert_eq!(
        (added(&results), fx.active(&t).await.len()),
        (0, 2),
        "{:?}: the default path after v2",
        fx.backend
    );
    fx.consolidation.set(previous);
}

/// Acceptance 6 (deletion): a deleted fact frees its key; concurrent re-additions after the
/// delete store exactly one new fact, and the deleted row stays deleted.
async fn case_delete_then_readd(fx: &Fx) {
    let t = fx.tenant("delete");
    fx.race(&[(&t, TEXT, MemoryType::Preference)]).await;
    let first = fx.active(&t).await.remove(0);
    let vectors: Arc<dyn VectorStore> = fx.vectors.clone();
    let deleted = fx
        .engine(fx.facts.clone(), vectors, TEXT, MemoryType::Preference)
        .delete_memory(DeleteMemoryInput {
            tenant: t.clone(),
            memory_id: first.id,
        })
        .await
        .unwrap();
    assert!(deleted.deleted);
    let results = fx
        .race(&[
            (&t, TEXT, MemoryType::Preference),
            (&t, TEXT_VARIANT, MemoryType::Preference),
        ])
        .await;
    assert_eq!(added(&results), 1, "{:?}", fx.backend);
    let facts = fx.active(&t).await;
    assert_eq!(facts.len(), 1, "{:?}", fx.backend);
    assert_ne!(facts[0].id, first.id);
    let state = fx
        .facts
        .get_fact_including_deleted(&t, first.id)
        .await
        .unwrap()
        .unwrap();
    assert!(state.deleted);
}

/// Acceptance 6 (imports, identifiers): imports write no key, so an export holding equal facts
/// (as a store before this change may) restores in full with its ids (Replace into the same
/// tenant, the only import target); a later addition of that text is the usual exact-duplicate
/// NOOP; and a concurrent race on the restored tenant still stores no further fact.
async fn case_import(fx: &Fx) {
    let t = fx.tenant("import");
    // Two equal facts, written as legacy rows (no key), e.g. earlier data.
    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(
            fx.facts
                .insert_fact(&t, new_fact(&t, TEXT))
                .await
                .unwrap()
                .id,
        );
    }
    ids.sort();
    let vectors: Arc<dyn VectorStore> = fx.vectors.clone();
    let engine = fx.engine(fx.facts.clone(), vectors, TEXT, MemoryType::Preference);
    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant: t.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(export.facts.len(), 2);
    let imported = engine
        .import_user_data(ImportUserDataInput {
            tenant: t.clone(),
            export,
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(imported.imported_facts, 2, "{:?}: {imported:?}", fx.backend);
    let mut restored: Vec<Uuid> = fx.active(&t).await.iter().map(|f| f.id).collect();
    restored.sort();
    assert_eq!(restored, ids, "{:?}: the import keeps both ids", fx.backend);
    let again = engine.add_memory(input(&t, TEXT)).await.unwrap();
    assert_eq!(again.added, 0, "{:?}", fx.backend);
    let results = fx
        .race_with(
            &[
                (&t, TEXT, MemoryType::Preference),
                (&t, TEXT_VARIANT, MemoryType::Preference),
            ],
            fx.vectors.clone(),
            false,
        )
        .await;
    assert_eq!(added(&results), 0, "{:?}", fx.backend);
    assert_eq!(fx.active(&t).await.len(), 2, "{:?}", fx.backend);
}

/// The store contract directly: one key holder per tenant, per-tenant scope, update clears the
/// key, soft and user deletion free it, plain inserts never conflict.
async fn case_store_contract(fx: &Fx) {
    let t = fx.tenant("store");
    let other = tenant(&t.org_id, "user_2");
    use ExactDedupRule::{ConsolidationV2, Legacy};
    let key_of = |rule, f: &Fact| exact_dedup_key(rule, f);
    let base = new_fact(&t, TEXT);
    let mut skill = new_fact(&t, TEXT);
    skill.memory_type = MemoryType::Skill;
    let mut subject = new_fact(&t, TEXT);
    subject.structure.subject = Some("Alice".into());
    let mut same_subject = new_fact(&t, TEXT_VARIANT);
    same_subject.structure.subject = Some("  alice ".into());
    let day1 = with_event_day(new_fact(&t, TEXT), 1);
    let day2 = with_event_day(new_fact(&t, TEXT), 2);
    for rule in [Legacy, ConsolidationV2] {
        // Case, whitespace and trailing punctuation never matter.
        assert_eq!(
            key_of(rule, &base),
            key_of(rule, &new_fact(&t, TEXT_VARIANT))
        );
        assert_eq!(key_of(rule, &subject), key_of(rule, &same_subject));
    }
    // Legacy: the memory type matters; subject and event time do not.
    assert_ne!(key_of(Legacy, &base), key_of(Legacy, &skill));
    assert_eq!(key_of(Legacy, &base), key_of(Legacy, &subject));
    assert_eq!(key_of(Legacy, &day1), key_of(Legacy, &day2));
    // Consolidation v2: subject and event time matter; the memory type does not.
    assert_eq!(
        key_of(ConsolidationV2, &base),
        key_of(ConsolidationV2, &skill)
    );
    assert_ne!(
        key_of(ConsolidationV2, &base),
        key_of(ConsolidationV2, &subject)
    );
    assert_ne!(
        key_of(ConsolidationV2, &day1),
        key_of(ConsolidationV2, &day2)
    );
    assert_ne!(
        key_of(ConsolidationV2, &base),
        key_of(ConsolidationV2, &day1)
    );
    // The two rules never share a key, even for one fact.
    assert_ne!(key_of(Legacy, &base), key_of(ConsolidationV2, &base));
    let key = key_of(Legacy, &base);
    let fact = new_fact;
    let s = fx.facts.as_ref();
    let DedupInsert::Inserted(first) = s
        .insert_fact_deduplicated(&t, fact(&t, TEXT), &key)
        .await
        .unwrap()
    else {
        panic!("first insert")
    };
    let dup = s
        .insert_fact_deduplicated(&t, fact(&t, TEXT_VARIANT), &key)
        .await
        .unwrap();
    assert_eq!(
        dup,
        DedupInsert::Duplicate {
            existing_fact_id: first.id
        },
        "{:?}",
        fx.backend
    );
    assert!(matches!(
        s.insert_fact_deduplicated(&other, fact(&other, TEXT), &key)
            .await
            .unwrap(),
        DedupInsert::Inserted(_)
    ));
    // A plain (import) insert of equal content never conflicts.
    s.insert_fact(&t, fact(&t, TEXT)).await.unwrap();
    // An update clears the key: the next keyed insert wins.
    let mut changed = first.clone();
    changed.content = "User prefers oolong tea.".into();
    s.update_fact(&t, changed).await.unwrap();
    let DedupInsert::Inserted(second) = s
        .insert_fact_deduplicated(&t, fact(&t, TEXT), &key)
        .await
        .unwrap()
    else {
        panic!("{:?}: update must clear the key", fx.backend)
    };
    // A soft delete frees it.
    s.soft_delete_fact(&t, second.id).await.unwrap();
    let DedupInsert::Inserted(third) = s
        .insert_fact_deduplicated(&t, fact(&t, TEXT), &key)
        .await
        .unwrap()
    else {
        panic!("{:?}: soft delete must free the key", fx.backend)
    };
    assert_eq!(
        s.insert_fact_deduplicated(&t, fact(&t, TEXT), &key)
            .await
            .unwrap(),
        DedupInsert::Duplicate {
            existing_fact_id: third.id
        }
    );
    // Deleting the user's data frees it.
    s.delete_user_data(&t).await.unwrap();
    assert!(matches!(
        s.insert_fact_deduplicated(&t, fact(&t, TEXT), &key)
            .await
            .unwrap(),
        DedupInsert::Inserted(_)
    ));
}

async fn all_cases(backend: Backend) {
    let Some(fx) = Fx::new(backend).await else {
        return;
    };
    case_store_contract(&fx).await;
    case_import(&fx).await;
    for consolidation in [None, Some(ConsolidationConfig::default())] {
        fx.consolidation.set(consolidation);
        case_exact_rules(&fx).await;
        case_same_tenant(&fx, 2).await;
        case_same_tenant(&fx, 4).await;
        case_cross_tenant(&fx).await;
        case_distinct(&fx).await;
        case_vector_failure_retry(&fx).await;
        case_delete_then_readd(&fx).await;
        eprintln!("AR086_CASES_PASSED backend={backend:?} mode={}", fx.mode());
    }
    case_mode_switch(&fx).await;
    eprintln!("AR086_MODE_SWITCH_PASSED backend={backend:?}");
}

#[tokio::test]
async fn ar086_mock() {
    all_cases(Backend::Mock).await;
}

#[tokio::test]
async fn ar086_sqlite() {
    all_cases(Backend::Sqlite).await;
}

#[tokio::test]
async fn ar086_postgres() {
    all_cases(Backend::Postgres).await;
}

// ---------------------------------------------------------------------------------------------
// Acceptance 4: separate OS processes (separate pools, no shared memory) on one database.
// ---------------------------------------------------------------------------------------------

/// Worker entry: does nothing unless started by a multi-process case.
#[tokio::test]
async fn ar086_worker_process_entry() {
    let Ok(role) = std::env::var("MEMCORE_AR086_WORKER") else {
        return;
    };
    let var = |name: &str| std::env::var(name).unwrap();
    let database = var("MEMCORE_AR086_DATABASE");
    let dir = PathBuf::from(var("MEMCORE_AR086_DIR"));
    let t = tenant(&var("MEMCORE_AR086_ORG"), "user_1");
    let text = var("MEMCORE_AR086_TEXT");
    let mut extracted = candidate(&text, var("MEMCORE_AR086_TYPE").parse().unwrap());
    extracted.structure.subject = std::env::var("MEMCORE_AR086_SUBJECT").ok();
    let consolidation = (var("MEMCORE_AR086_MODE") == "v2").then(ConsolidationConfig::default);
    let gate = Arc::new(Gate {
        inner: open_store(&database).await,
        rendezvous: Rendezvous::Files {
            ready: dir.join(format!("ready-{role}")),
            go: dir.join("go"),
        },
        reached: AtomicUsize::new(0),
    });
    let out = engine(
        gate,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockMemoryEventStore::new()),
        extracted,
        consolidation,
    )
    .add_memory(input(&t, &text))
    .await
    .unwrap();
    println!(
        "AR086_WORKER_RESULT role={role} added={} duplicate={}",
        out.added, out.discarded.duplicate_content
    );
}

/// One worker process: the text, memory type and subject its extraction returns.
struct Worker(&'static str, MemoryType, Option<&'static str>);

/// `workers` separate OS processes (own pools, no shared memory) add to one tenant of one
/// database, all released together at the insert; `expected` facts must be stored.
async fn multi_process_case(
    database: &str,
    label: &str,
    mode: &str,
    workers: &[Worker],
    expected: usize,
) {
    let store = open_store(database).await;
    let org = format!("org_ar086_mp_{label}_{}", Uuid::new_v4().simple());
    let t = tenant(&org, "user_1");
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = workers
        .iter()
        .enumerate()
        .map(|(i, Worker(text, kind, subject))| {
            let mut command = std::process::Command::new(&exe);
            command
                .args([
                    "ar086_worker_process_entry",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("MEMCORE_AR086_WORKER", format!("p{i}"))
                .env("MEMCORE_AR086_DATABASE", database)
                .env("MEMCORE_AR086_DIR", dir.path())
                .env("MEMCORE_AR086_ORG", &org)
                .env("MEMCORE_AR086_TEXT", text)
                .env("MEMCORE_AR086_TYPE", kind.as_str())
                .env("MEMCORE_AR086_MODE", mode)
                .env_remove("MEMCORE_AR086_SUBJECT")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(subject) = subject {
                command.env("MEMCORE_AR086_SUBJECT", subject);
            }
            command.spawn().unwrap()
        })
        .collect();
    // Every worker passed its dedup reads and waits at the insert; release them together.
    let ready: Vec<_> = (0..workers.len())
        .map(|i| dir.path().join(format!("ready-p{i}")))
        .collect();
    wait_for(&ready).await;
    std::fs::write(dir.path().join("go"), b"go").unwrap();
    let (mut added, mut duplicate) = (0, 0);
    for (i, child) in children.into_iter().enumerate() {
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "worker {i}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let line = stdout
            .lines()
            .find_map(|l| l.find("AR086_WORKER_RESULT").map(|at| &l[at..]))
            .expect("worker result");
        let field = |name: &str| -> usize {
            line.split_whitespace()
                .find_map(|kv| kv.strip_prefix(name))
                .unwrap()
                .parse()
                .unwrap()
        };
        added += field("added=");
        duplicate += field("duplicate=");
    }
    let facts = active(store.as_ref(), &t).await;
    let processes = workers.len();
    eprintln!(
        "AR086_MULTI_PROCESS database={} case={label} mode={mode} processes={processes} added={added} duplicate={duplicate} facts={}",
        if database.starts_with("postgres") {
            "postgres"
        } else {
            "sqlite"
        },
        facts.len()
    );
    assert_eq!(
        (facts.len(), added, duplicate),
        (expected, expected, processes - expected),
        "{label} ({mode}): {processes} processes"
    );
}

/// The multi-process cases, per backend: equivalent text (both modes' rules), the default rule
/// across subjects, v2 across memory types, and v2's distinct subjects.
async fn multi_process_cases(database: &str) {
    use MemoryType::{Preference, Skill};
    let equivalent = [
        Worker(TEXT, Preference, None),
        Worker(TEXT_VARIANT, Preference, None),
        Worker("User prefers green tea in the morning!", Preference, None),
    ];
    let subjects = [
        Worker(TEXT, Preference, None),
        Worker(TEXT_VARIANT, Preference, Some("Alice")),
        Worker(TEXT, Preference, Some("Bob")),
    ];
    let types = [
        Worker(TEXT, Preference, Some("Alice")),
        Worker(TEXT_VARIANT, Skill, Some("alice")),
        Worker(TEXT, Skill, Some(" Alice ")),
    ];
    multi_process_case(database, "equivalent", "legacy", &equivalent, 1).await;
    multi_process_case(database, "equivalent", "v2", &equivalent, 1).await;
    multi_process_case(database, "subjects", "legacy", &subjects, 1).await;
    multi_process_case(database, "subjects", "v2", &subjects, 3).await;
    multi_process_case(database, "types", "legacy", &types, 2).await;
    multi_process_case(database, "types", "v2", &types, 1).await;
}

#[tokio::test]
async fn ar086_multi_process_sqlite() {
    if std::env::var("MEMCORE_AR086_WORKER").is_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let database = sqlite_url(dir.path());
    drop(open_store(&database).await); // migrate once before the workers start
    multi_process_cases(&database).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn ar086_multi_process_postgres() {
    if std::env::var("MEMCORE_AR086_WORKER").is_ok() {
        return;
    }
    let Ok(database) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
        eprintln!("AR086_POSTGRES_SKIPPED: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    multi_process_cases(&database).await;
}
