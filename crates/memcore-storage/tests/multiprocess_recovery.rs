//! closure: ingest-journal recovery across independent OS processes sharing one real
//! PostgreSQL database and one real Qdrant collection. Invented data; no model calls.
//!
//! Each scenario's "instances" are child processes of this test binary (`v21c_child`, re-executed
//! with a role in `V21C_ROLE`): each opens its own connections and engine, exactly as a separate API
//! process would. Crashes are hard kills (`Child::kill`, no destructors, no rollback). Children
//! synchronise with the parent through marker files. Needs `MEMCORE_TEST_POSTGRES_URL` and
//! `MEMCORE_TEST_QDRANT_URL`; skipped otherwise (the child entry point is a no-op without a role).

#![cfg(all(feature = "postgres", feature = "qdrant"))]

#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    DedupInsert, EmbeddingProvider, FactClassificationInput, FactExtractionInput,
    FactFulltextQuery, IngestJournalStore, LlmProvider, OrgUserListQuery, OrgUserSummary,
    PointScope, RetentionPruneResult, ScoredFact, StoredFactState, SummarizationInput,
    VectorPointState, VectorStore,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, Fact, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, IngestJournalComponents,
    MemoryEngine, MemoryMessage, MemoryType, MessageRole, RelatedFactMatcher, TenantContext,
    VectorRecord, VectorSearchQuery, VectorSearchResult,
};
use memcore_storage::{PostgresFactStore, PostgresIngestJournalStore, QdrantVectorStore};
use serde_json::json;
use uuid::Uuid;

const DIM: usize = 16;
const A: &str = "Asha keeps bees on the roof.";
const B: &str = "Asha repairs bicycles on Sundays.";
const C: &str = "Asha sings in a harbour choir.";
const A_PARAPHRASE: &str = "Asha has bees up on her roof.";
const LISBON: &str = "Asha lives in Lisbon near the river.";
const PORTO: &str = "Asha lives in Porto near the river.";

fn tenant() -> TenantContext {
    TenantContext::new("org_v21c_mp", "user_asha").unwrap()
}

fn vector(text: &str) -> Vec<f32> {
    let mut v = [0.0_f32; DIM];
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let h = word
            .bytes()
            .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
        v[(h % DIM as u32) as usize] += 1.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}

// ================================================================== child side

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn marker(name: &str) -> PathBuf {
    Path::new(&env("V21C_DIR").unwrap()).join(name)
}

/// Blocks this process forever after announcing `tag` (the parent kills it).
async fn park(tag: &str) {
    std::fs::write(marker(&format!("{tag}.reached")), b"1").unwrap();
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

struct Embed;

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        Ok(vector(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        DIM
    }
}

/// Extraction returns `V21C_TEXTS`; classification parks on `V21C_HOLD`, updates the first offered
/// fact under the semantic matcher, adds otherwise.
struct Llm {
    texts: Vec<String>,
    hold: Option<String>,
    tag: String,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        Ok(FactExtractionOutput::from_candidates(
            self.texts
                .iter()
                .map(|t| {
                    CandidateFact::new(t, MemoryType::Profile, 0.9, 0.9, None, json!({})).unwrap()
                })
                .collect(),
        ))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        if self.hold.as_deref() == Some(input.candidate_fact.content.as_str()) {
            park(&self.tag).await;
        }
        let target = input.existing_facts.first().map(|f| f.id);
        Ok(match target {
            Some(id) => FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(id),
                reason: None,
                confidence: 0.9,
            },
            None => FactOperationDecision {
                operation: FactOperation::Add,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            },
        })
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

/// Real Qdrant with child-selected faults: `V21C_FAULT=hang_delete` parks on the first point
/// delete, `fail_deletes` fails every point delete, `fail_upsert:<text>` fails that upsert.
struct Vectors {
    inner: QdrantVectorStore,
    fault: Option<String>,
    tag: String,
}

#[async_trait]
impl VectorStore for Vectors {
    async fn upsert_vector(&self, t: &TenantContext, r: VectorRecord) -> MemcoreResult<()> {
        self.inner.upsert_vector(t, r).await
    }
    async fn write_vector(
        &self,
        t: &TenantContext,
        r: VectorRecord,
        intent: memcore_core::ports::VectorWriteIntent,
    ) -> MemcoreResult<()> {
        if let Some(text) = self
            .fault
            .as_deref()
            .and_then(|f| f.strip_prefix("fail_upsert:"))
            && text == r.content
        {
            return Err(MemcoreError::StorageError(
                "qdrant unavailable (injected)".to_string(),
            ));
        }
        self.inner.write_vector(t, r, intent).await
    }
    async fn search_vectors(&self, q: VectorSearchQuery) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(q).await
    }
    async fn delete_by_fact_id(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        match self.fault.as_deref() {
            Some("hang_delete") => {
                park(&self.tag).await;
                unreachable!("parked until killed")
            }
            Some("fail_deletes") => Err(MemcoreError::StorageError(
                "qdrant unavailable (injected)".to_string(),
            )),
            _ => self.inner.delete_by_fact_id(t, id).await,
        }
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
    async fn count_points(&self, scope: &PointScope) -> MemcoreResult<u64> {
        self.inner.count_points(scope).await
    }
}

/// Real PostgreSQL facts; `V21C_FAULT=hang_update:<n>` parks on the n-th `update_fact` (0-based).
struct Facts {
    inner: PostgresFactStore,
    hang_update: Option<usize>,
    updates: AtomicUsize,
    tag: String,
}

#[async_trait]
impl FactStore for Facts {
    async fn insert_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        self.inner.insert_fact(t, f).await
    }
    async fn insert_fact_deduplicated(
        &self,
        t: &TenantContext,
        f: Fact,
        k: &str,
    ) -> MemcoreResult<DedupInsert> {
        self.inner.insert_fact_deduplicated(t, f, k).await
    }
    async fn update_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        let n = self.updates.fetch_add(1, Ordering::SeqCst);
        if self.hang_update == Some(n) {
            park(&self.tag).await;
        }
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

async fn child_engine(tag: &str) -> MemoryEngine {
    let url = env("V21C_DB_URL").unwrap();
    let fault = env("V21C_FAULT");
    let facts = Facts {
        inner: PostgresFactStore::connect(&url).await.unwrap(),
        hang_update: fault
            .as_deref()
            .and_then(|f| f.strip_prefix("hang_update:"))
            .map(|n| n.parse().unwrap()),
        updates: AtomicUsize::new(0),
        tag: tag.to_string(),
    };
    let journal = PostgresIngestJournalStore::new(
        memcore_storage::connect_postgres_pool(&url).await.unwrap(),
    );
    let inner = QdrantVectorStore::connect(
        &env("MEMCORE_TEST_QDRANT_URL").unwrap(),
        &env("V21C_COLLECTION").unwrap(),
        DIM,
    )
    .await
    .unwrap();
    let llm = Llm {
        texts: serde_json::from_str(&env("V21C_TEXTS").unwrap_or_else(|| "[]".to_string()))
            .unwrap(),
        hold: env("V21C_HOLD"),
        tag: tag.to_string(),
    };
    let matcher = if env("V21C_SEMANTIC").is_some() {
        RelatedFactMatcher::Semantic
    } else {
        RelatedFactMatcher::Legacy
    };
    MemoryEngine::new(
        Arc::new(facts),
        Arc::new(Vectors {
            inner,
            fault,
            tag: tag.to_string(),
        }),
        Arc::new(llm),
        Arc::new(Embed),
    )
    .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
        enabled: false,
        ..Default::default()
    })
    .with_related_fact_matcher(matcher)
    .with_ingest_journal(Some(IngestJournalComponents {
        store: Arc::new(journal),
        stale_after: Duration::from_millis(
            env("V21C_STALE_MS")
                .map(|v| v.parse().unwrap())
                .unwrap_or(300_000),
        ),
        inflight_wait: Duration::from_millis(
            env("V21C_WAIT_MS")
                .map(|v| v.parse().unwrap())
                .unwrap_or(10_000),
        ),
        receipt_ttl: memcore_core::DEFAULT_INGEST_RECEIPT_TTL,
    }))
}

/// The child entry point: a no-op unless re-executed with `V21C_ROLE`.
#[test]
fn v21c_child() {
    let Some(role) = env("V21C_ROLE") else {
        return;
    };
    let tag = env("V21C_TAG").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let engine = child_engine(&tag).await;
        match role.as_str() {
            "add" => {
                let ids: Vec<String> = serde_json::from_str(&env("V21C_IDS").unwrap_or_else(|| "[]".to_string())).unwrap();
                let messages = if ids.is_empty() {
                    vec![MemoryMessage { role: MessageRole::User, content: "Asha's week.".to_string(), ..Default::default() }]
                } else {
                    ids.iter().map(|id| MemoryMessage { role: MessageRole::User, content: format!("message {id}"), message_id: Some(id.clone()), ..Default::default() }).collect()
                };
                match engine.add_memory(AddMemoryInput { tenant: tenant(), messages, metadata: json!({}), session: Default::default() }).await {
                    Ok(out) => json!({"ok": true, "added": out.added, "memories": out.memories.iter().map(|f| f.content.clone()).collect::<Vec<_>>()}),
                    Err(error) => json!({"ok": false, "error": error.code(), "message": error.to_string()}),
                }
            }
            "sweep" => match engine.recover_stale_ingest_operations(100).await {
                Ok(report) => json!({"ok": true, "report": report}),
                Err(error) => json!({"ok": false, "error": error.code()}),
            },
            other => panic!("unknown role {other}"),
        }
    });
    std::fs::write(
        marker(&format!("{tag}.json")),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

// ================================================================== parent side

struct Scenario {
    db_url: String,
    db_name: String,
    admin_url: String,
    collection: String,
    qdrant_url: String,
    dir: tempfile::TempDir,
    _guard: qdrant_cleanup::TestCollections,
    rt: tokio::runtime::Runtime,
}

struct Instance {
    tag: String,
    child: Child,
}

impl Scenario {
    fn new(label: &str) -> Option<Self> {
        let (Some(pg), Some(qdrant_url)) = (
            env("MEMCORE_TEST_POSTGRES_URL"),
            env("MEMCORE_TEST_QDRANT_URL"),
        ) else {
            eprintln!(
                "skipping `{label}`: MEMCORE_TEST_POSTGRES_URL / MEMCORE_TEST_QDRANT_URL not set"
            );
            return None;
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let db_name = format!("memcore_v21c_mp_{}", Uuid::new_v4().simple());
        rt.block_on(async {
            let admin = sqlx::PgPool::connect(&pg).await.unwrap();
            sqlx::query(&format!("CREATE DATABASE {db_name}"))
                .execute(&admin)
                .await
                .unwrap();
        });
        let base = pg.rsplit_once('/').unwrap().0;
        let collection = format!("memcore_test_v21c_{}", Uuid::new_v4().simple());
        let guard = qdrant_cleanup::TestCollections::owning(&collection);
        Some(Self {
            db_url: format!("{base}/{db_name}"),
            db_name,
            admin_url: pg,
            collection,
            qdrant_url,
            dir: tempfile::tempdir().unwrap(),
            _guard: guard,
            rt,
        })
    }

    /// Starts an instance (a child process) with `role` and extra environment.
    fn spawn(&self, tag: &str, role: &str, vars: &[(&str, String)]) -> Instance {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "v21c_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("V21C_ROLE", role)
        .env("V21C_TAG", tag)
        .env("V21C_DIR", self.dir.path())
        .env("V21C_DB_URL", &self.db_url)
        .env("V21C_COLLECTION", &self.collection)
        .env("MEMCORE_TEST_QDRANT_URL", &self.qdrant_url)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        for (k, v) in vars {
            cmd.env(k, v);
        }
        Instance {
            tag: tag.to_string(),
            child: cmd.spawn().unwrap(),
        }
    }

    fn wait_marker(&self, name: &str) {
        let path = self.dir.path().join(name);
        let started = Instant::now();
        while !path.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "marker {name} not reached"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits for an instance to exit and returns its JSON result.
    fn finish(&self, mut instance: Instance) -> serde_json::Value {
        let started = Instant::now();
        loop {
            if instance.child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "instance {} did not finish",
                instance.tag
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let text = std::fs::read(self.dir.path().join(format!("{}.json", instance.tag)))
            .expect("result written");
        serde_json::from_slice(&text).unwrap()
    }

    /// Hard-kills an instance once it reached its marker (a crash: no rollback runs).
    fn crash_at_marker(&self, mut instance: Instance) {
        self.wait_marker(&format!("{}.reached", instance.tag));
        instance.child.kill().unwrap();
        let _ = instance.child.wait();
    }

    fn sql(&self, statement: &str) {
        self.rt.block_on(async {
            let pool = memcore_storage::connect_postgres_pool(&self.db_url)
                .await
                .unwrap();
            sqlx::query(statement).execute(&pool).await.unwrap();
            pool.close().await;
        });
    }

    /// Ages every journalled operation past any stale window (as if minutes had passed).
    fn age_journal(&self) {
        self.sql("UPDATE ingest_operations SET updated_at_ms = 0");
    }

    /// (sorted active texts, live rows without exactly one matching point, points of non-live
    /// rows, journal operations, all live texts with multiplicity).
    fn state(&self) -> (Vec<String>, usize, usize, usize, Vec<String>) {
        self.rt.block_on(async {
            let facts = PostgresFactStore::connect(&self.db_url).await.unwrap();
            let journal = PostgresIngestJournalStore::new(
                memcore_storage::connect_postgres_pool(&self.db_url)
                    .await
                    .unwrap(),
            );
            let vectors = QdrantVectorStore::connect(&self.qdrant_url, &self.collection, DIM)
                .await
                .unwrap();
            let t = tenant();
            let query = |include_deleted| FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 1000,
                cursor: None,
                include_deleted,
            };
            let all = facts.search_facts(query(true)).await.unwrap();
            let live = facts.search_facts(query(false)).await.unwrap();
            let live_ids: HashSet<Uuid> = live.iter().map(|f| f.id).collect();
            let mut bad_live = 0;
            for fact in &live {
                let points = vectors
                    .fact_vector_points(&t, fact.id)
                    .await
                    .unwrap()
                    .unwrap_or_default();
                if points.len() != 1 || points[0].content != fact.content {
                    bad_live += 1;
                }
            }
            let mut orphans = 0;
            for fact in all.iter().filter(|f| !live_ids.contains(&f.id)) {
                orphans += vectors
                    .fact_vector_points(&t, fact.id)
                    .await
                    .unwrap()
                    .unwrap_or_default()
                    .len();
            }
            let mut active: Vec<String> = live
                .iter()
                .filter(|f| f.is_active())
                .map(|f| f.content.clone())
                .collect();
            active.sort();
            let mut live_texts: Vec<String> = live.iter().map(|f| f.content.clone()).collect();
            live_texts.sort();
            (
                active,
                bad_live,
                orphans,
                journal.count_operations().await.unwrap(),
                live_texts,
            )
        })
    }

    fn assert_settled(&self, label: &str, active: &[&str]) {
        let (got, bad_live, orphans, journal, _) = self.state();
        let mut want: Vec<String> = active.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want, "{label}: active facts");
        assert_eq!(
            (bad_live, orphans, journal),
            (0, 0, 0),
            "{label}: rows<->points consistent, journal empty"
        );
    }

    /// A crashed request holding A (applied) and B (in flight).
    fn crashed_request(&self) {
        let a = self.spawn(
            "crashed",
            "add",
            &[
                ("V21C_TEXTS", json!([A, B]).to_string()),
                ("V21C_HOLD", B.to_string()),
            ],
        );
        self.crash_at_marker(a);
        assert_eq!(
            self.state().0,
            vec![A.to_string()],
            "the crash left A applied"
        );
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let (admin, name) = (self.admin_url.clone(), self.db_name.clone());
        self.rt.block_on(async move {
            if let Ok(pool) = sqlx::PgPool::connect(&admin).await {
                let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                    .execute(&pool)
                    .await;
            }
        });
    }
}

fn ok(v: &serde_json::Value) -> bool {
    v["ok"].as_bool() == Some(true)
}

/// Results of every scenario, for the closure report (`MEMCORE_V21C_MP_OUT`).
static OUTCOMES: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());

fn record(name: &str, detail: serde_json::Value) {
    let mut all = OUTCOMES.lock().unwrap();
    all.push(json!({"scenario": name, "detail": detail}));
    if let Some(path) = env("MEMCORE_V21C_MP_OUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&*all).unwrap()).unwrap();
    }
}

#[test]
fn mp01_two_instances_race_to_recover_the_same_crashed_operation() {
    let Some(s) = Scenario::new("mp01") else {
        return;
    };
    s.crashed_request();
    s.age_journal();
    let (p1, p2) = (
        s.spawn("sweep1", "sweep", &[]),
        s.spawn("sweep2", "sweep", &[]),
    );
    let (r1, r2) = (s.finish(p1), s.finish(p2));
    let rolled = r1["report"]["rolled_back"].as_u64().unwrap()
        + r2["report"]["rolled_back"].as_u64().unwrap();
    assert_eq!(rolled, 1, "exactly one instance rolled it back: {r1} {r2}");
    s.assert_settled("mp01", &[]);
    record(
        "mp01_concurrent_sweeps",
        json!({"sweep1": r1, "sweep2": r2}),
    );
}

#[test]
fn mp02_a_recovery_that_crashes_is_resumed_by_another_instance() {
    let Some(s) = Scenario::new("mp02") else {
        return;
    };
    s.crashed_request();
    s.age_journal();
    let p1 = s.spawn(
        "sweep_crash",
        "sweep",
        &[("V21C_FAULT", "hang_delete".to_string())],
    );
    s.crash_at_marker(p1);
    let (_, _, _, journal, _) = s.state();
    assert_eq!(journal, 1, "the interrupted rollback is still journalled");
    let early = s.finish(s.spawn("sweep_early", "sweep", &[]));
    assert_eq!(
        early["report"]["rolled_back"], 0,
        "its claim is fresh: not stolen before the stale window"
    );
    s.age_journal();
    let r = s.finish(s.spawn("sweep_resume", "sweep", &[]));
    assert_eq!(r["report"]["rolled_back"], 1, "{r}");
    s.assert_settled("mp02", &[]);
    let again = s.finish(s.spawn("sweep_again", "sweep", &[]));
    assert_eq!(
        again["report"]["examined"], 0,
        "repeated recovery does nothing"
    );
    record(
        "mp02_resume_after_recovery_crash",
        json!({"early": early, "resume": r, "again": again}),
    );
}

#[test]
fn mp03_a_retry_while_another_instance_compensates_keeps_the_retrys_fact() {
    let Some(s) = Scenario::new("mp03") else {
        return;
    };
    s.crashed_request();
    s.age_journal();
    // Instance 1 claims the rollback and stalls inside it: A's row is soft-deleted, its point
    // delete hangs.
    let p1 = s.spawn(
        "sweep_stall",
        "sweep",
        &[("V21C_FAULT", "hang_delete".to_string())],
    );
    s.wait_marker("sweep_stall.reached");
    // Instance 2: the client's retry of A while that rollback is still running.
    let retry = s.finish(s.spawn("retry", "add", &[("V21C_TEXTS", json!([A]).to_string())]));
    assert!(ok(&retry), "{retry}");
    assert_eq!(retry["added"], 1, "the retry stores its own A: {retry}");
    // The stalled rollback crashes and is resumed: it must not delete the retry's committed A.
    s.crash_at_marker(p1);
    s.age_journal();
    let resumed = s.finish(s.spawn("sweep", "sweep", &[]));
    assert_eq!(resumed["report"]["rolled_back"], 1, "{resumed}");
    s.assert_settled("mp03", &[A]);
    assert_eq!(
        s.state().4,
        vec![A.to_string()],
        "exactly one live A, the retry's"
    );
    record(
        "mp03_retry_during_compensation",
        json!({"retry_during": retry, "resumed_rollback": resumed}),
    );
}

#[test]
fn mp04_two_retries_on_different_instances_store_one_fact() {
    let Some(s) = Scenario::new("mp04") else {
        return;
    };
    s.crashed_request();
    s.age_journal();
    let (c1, c2) = (
        s.spawn("retry1", "add", &[("V21C_TEXTS", json!([A]).to_string())]),
        s.spawn("retry2", "add", &[("V21C_TEXTS", json!([A]).to_string())]),
    );
    let (r1, r2) = (s.finish(c1), s.finish(c2));
    assert!(ok(&r1) && ok(&r2), "both retries succeed: {r1} {r2}");
    s.assert_settled("mp04", &[A]);
    assert_eq!(s.state().4, vec![A.to_string()], "exactly one live A");
    record(
        "mp04_two_retries_two_instances",
        json!({"retry1": r1, "retry2": r2}),
    );
}

#[test]
fn mp05_recovery_overlapping_unrelated_ingestion_touches_only_its_operation() {
    let Some(s) = Scenario::new("mp05") else {
        return;
    };
    s.crashed_request();
    s.age_journal();
    let (p, c) = (
        s.spawn("sweep", "sweep", &[]),
        s.spawn("other", "add", &[("V21C_TEXTS", json!([C]).to_string())]),
    );
    let (rp, rc) = (s.finish(p), s.finish(c));
    assert_eq!(rp["report"]["rolled_back"], 1);
    assert!(ok(&rc), "{rc}");
    s.assert_settled("mp05", &[C]);
    record("mp05_overlap_unrelated", json!({"sweep": rp, "add": rc}));
}

#[test]
fn mp06_recovery_survives_a_temporary_vector_store_outage() {
    let Some(s) = Scenario::new("mp06") else {
        return;
    };
    s.crashed_request();
    s.age_journal();
    let down = s.finish(s.spawn(
        "sweep_down",
        "sweep",
        &[("V21C_FAULT", "fail_deletes".to_string())],
    ));
    assert_eq!(down["report"]["failed"], 1, "{down}");
    assert_eq!(s.state().3, 1, "still journalled after the failed attempt");
    s.age_journal();
    let up = s.finish(s.spawn("sweep_up", "sweep", &[]));
    assert_eq!(up["report"]["rolled_back"], 1, "{up}");
    s.assert_settled("mp06", &[]);
    record(
        "mp06_vector_outage",
        json!({"during_outage": down, "after": up}),
    );
}

#[test]
fn mp07_an_active_request_on_another_instance_is_never_recovered() {
    let Some(s) = Scenario::new("mp07") else {
        return;
    };
    let a = s.spawn(
        "active",
        "add",
        &[
            ("V21C_TEXTS", json!([A, B]).to_string()),
            ("V21C_HOLD", B.to_string()),
        ],
    );
    s.wait_marker("active.reached");
    // Another instance sweeps while the request is alive and within its window.
    let r = s.finish(s.spawn("sweep", "sweep", &[]));
    assert_eq!(
        (
            r["report"]["examined"].as_u64(),
            r["report"]["rolled_back"].as_u64()
        ),
        (Some(0), Some(0)),
        "{r}"
    );
    assert_eq!(
        s.state().0,
        vec![A.to_string()],
        "the active request's write is untouched"
    );
    s.crash_at_marker(a);
    s.age_journal();
    assert_eq!(
        s.finish(s.spawn("sweep_after", "sweep", &[]))["report"]["rolled_back"],
        1
    );
    s.assert_settled("mp07", &[]);
    record(
        "mp07_active_not_recovered",
        json!({"sweep_while_active": r}),
    );
}

#[test]
fn mp08_a_crash_during_supersession_is_undone_after_restart() {
    let Some(s) = Scenario::new("mp08") else {
        return;
    };
    let seed = s.finish(s.spawn(
        "seed",
        "add",
        &[
            ("V21C_TEXTS", json!([LISBON]).to_string()),
            ("V21C_SEMANTIC", "1".to_string()),
        ],
    ));
    assert!(ok(&seed), "{seed}");
    // The supersession: replacement added, target invalidated (update 0), linked (update 1): crash
    // before the link.
    let p = s.spawn(
        "supersede",
        "add",
        &[
            ("V21C_TEXTS", json!([PORTO]).to_string()),
            ("V21C_SEMANTIC", "1".to_string()),
            ("V21C_FAULT", "hang_update:1".to_string()),
        ],
    );
    s.crash_at_marker(p);
    let (active, ..) = s.state();
    assert_eq!(
        active,
        vec![PORTO.to_string()],
        "the crash left Lisbon superseded and Porto current"
    );
    s.age_journal();
    assert_eq!(
        s.finish(s.spawn("sweep", "sweep", &[]))["report"]["rolled_back"],
        1
    );
    s.assert_settled("mp08", &[LISBON]);
    let retry = s.finish(s.spawn(
        "retry",
        "add",
        &[
            ("V21C_TEXTS", json!([PORTO]).to_string()),
            ("V21C_SEMANTIC", "1".to_string()),
        ],
    ));
    assert!(ok(&retry), "{retry}");
    s.assert_settled("mp08 retry", &[PORTO]);
    record("mp08_crash_during_supersession", json!({"retry": retry}));
}

#[test]
fn mp09_lost_response_retry_on_another_instance_returns_the_committed_result() {
    let Some(s) = Scenario::new("mp09") else {
        return;
    };
    let ids = json!(["m1", "m2"]).to_string();
    let first = s.finish(s.spawn(
        "first",
        "add",
        &[
            ("V21C_TEXTS", json!([A, B]).to_string()),
            ("V21C_IDS", ids.clone()),
        ],
    ));
    assert_eq!(first["added"], 2);
    // The response is lost; the retry lands on another instance, whose model now paraphrases A.
    let retry = s.finish(s.spawn(
        "retry",
        "add",
        &[
            ("V21C_TEXTS", json!([A_PARAPHRASE]).to_string()),
            ("V21C_IDS", ids),
        ],
    ));
    assert!(ok(&retry), "{retry}");
    assert_eq!(retry["added"], first["added"], "the recorded result");
    assert_eq!(retry["memories"].as_array().map(Vec::len), Some(2));
    s.assert_settled("mp09", &[A, B]);
    record(
        "mp09_lost_response_retry",
        json!({"first": first, "retry": retry}),
    );
}

#[test]
fn mp10_vector_failure_after_the_sql_row_is_rolled_back_by_the_request() {
    let Some(s) = Scenario::new("mp10") else {
        return;
    };
    let r = s.finish(s.spawn(
        "fail",
        "add",
        &[
            ("V21C_TEXTS", json!([A, B, C]).to_string()),
            ("V21C_FAULT", format!("fail_upsert:{C}")),
        ],
    ));
    assert_eq!(r["ok"], false);
    s.assert_settled("mp10", &[]);
    record("mp10_sql_then_vector_failure", json!({"request": r}));
}
