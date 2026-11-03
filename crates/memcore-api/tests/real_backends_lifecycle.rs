//! Import, retention, deletion and verification end to end on
//! the REAL PostgreSQL fact / episode / event / entity stores and REAL Qdrant collections, with
//! test-only fault wrappers around those real stores. Every state check reads the backends
//! directly (fresh store handles or the raw Qdrant client), never through the engine under test.
//!
//! Needs `MEMCORE_TEST_POSTGRES_URL` (a disposable server; the runner recreates the databases
//! `ar023t2_e2e`, `ar023t2_w0`, `ar023t2_w1`, `ar023t2_w2` before a run) and
//! `MEMCORE_TEST_QDRANT_URL`; skipped otherwise. Qdrant collections are test-owned.
//! Run with `--test-threads 1`. Invented data only.

#![cfg(all(feature = "postgres", feature = "qdrant"))]

#[path = "../../memcore-storage/tests/support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EntityStore, FactFulltextQuery, MemoryEventQuery, OrgMemoryEventQuery, OrgUserListQuery,
    OrgUserSummary, PointScope, RetentionPruneResult, ScoredFact, StoredFactState,
    VectorPointState,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, BuildContextInput, CandidateFact, ContextCacheConfig,
    DeleteMemoryInput, EmbeddingProvider, Entity, EntityComponents, EntityConfig, EntityExport,
    EntityRelation, EntityRole, EntityType, Episode, EpisodeComponents, EpisodeStore,
    ExportUserDataInput, Fact, FactEntityLink, FactOperation, FactOperationDecision,
    FactSearchQuery, FactStore, ForgetUserInput, ImportMode, ImportUserDataInput,
    ImportUserDataOutput, InMemoryContextCache, MemoryEngine, MemoryEvent, MemoryEventOperation,
    MemoryEventStore, MemoryMessage, MemorySource, MemoryType, MessageRole, RetrievalMode,
    SearchEpisodesInput, SearchMemoryInput, SearchOptions, TenantContext, UserMemoryExport,
    VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore, VectorWriteIntent,
    derived_import_fact_id,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{
    PostgresEntityStore, PostgresEpisodeStore, PostgresFactStore, PostgresMemoryEventStore,
    QdrantVectorStore,
};
use metrics::{
    Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{PointStruct, UpsertPointsBuilder};
use serde_json::{Value, json};
use tokio::sync::Notify;
use uuid::Uuid;

const DIMS: usize = 32;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn pg_url(database: &str) -> Option<String> {
    let url = env("MEMCORE_TEST_POSTGRES_URL")?;
    let base = url.rsplit_once('/').map(|(base, _)| base.to_string())?;
    Some(format!("{base}/{database}"))
}

fn qdrant_url() -> Option<String> {
    env("MEMCORE_TEST_QDRANT_URL")
}

fn raw_qdrant(url: &str) -> Qdrant {
    Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Metrics capture (thread-local recorder)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<BTreeMap<String, u64>>>);

impl Captured {
    fn failures(&self) -> BTreeMap<String, u64> {
        let prefix = "memcore_cross_store_write_failures_total{path=";
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(prefix)
                    .and_then(|r| r.strip_suffix('}'))
                    .map(|p| (p.to_string(), *v))
            })
            .filter(|(_, v)| *v > 0)
            .collect()
    }
    fn keys(&self) -> Vec<String> {
        self.0.lock().unwrap().keys().cloned().collect()
    }
}

struct Handle(String, Captured);
impl CounterFn for Handle {
    fn increment(&self, value: u64) {
        *self.1.0.lock().unwrap().entry(self.0.clone()).or_default() += value;
    }
    fn absolute(&self, value: u64) {
        self.1.0.lock().unwrap().insert(self.0.clone(), value);
    }
}

struct Rec(Captured);
impl Recorder for Rec {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        let mut labels: Vec<String> = key
            .labels()
            .map(|l| format!("{}={}", l.key(), l.value()))
            .collect();
        labels.sort();
        Counter::from_arc(Arc::new(Handle(
            format!("{}{{{}}}", key.name(), labels.join(",")),
            self.0.clone(),
        )))
    }
    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }
    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

fn expected(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(p, v)| (p.to_string(), *v)).collect()
}

// ---------------------------------------------------------------------------------------------
// FaultPlan around the REAL stores
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// Fail before the inner (real) call: no effect.
    Before,
    /// Report success without calling the real store (a lost write), for the R4 verification.
    Swallow,
}

#[derive(Default)]
struct PlanState {
    calls: HashMap<&'static str, usize>,
    armed: HashMap<&'static str, (usize, Mode)>,
    fired: HashMap<&'static str, Vec<usize>>,
    tenants: Vec<(&'static str, String)>,
    gate: Option<(&'static str, Arc<Notify>, Arc<Notify>)>,
}

#[derive(Default)]
struct Plan(Mutex<PlanState>);

impl Plan {
    fn arm(&self, op: &'static str, nth: usize, mode: Mode) {
        let mut s = self.0.lock().unwrap();
        s.calls.insert(op, 0);
        s.armed.insert(op, (nth, mode));
    }
    fn reset(&self) {
        *self.0.lock().unwrap() = PlanState::default();
    }
    fn gate(&self, op: &'static str, reached: Arc<Notify>, release: Arc<Notify>) {
        self.0.lock().unwrap().gate = Some((op, reached, release));
    }
    /// Ok(true) = proceed to the real call; Ok(false) = swallow; Err = injected failure.
    async fn hit(&self, op: &'static str, tenant: Option<&TenantContext>) -> MemcoreResult<bool> {
        let (fire, gate) = {
            let mut s = self.0.lock().unwrap();
            let n = {
                let c = s.calls.entry(op).or_insert(0);
                *c += 1;
                *c
            };
            if let Some(t) = tenant {
                s.tenants.push((op, format!("{}/{}", t.org_id, t.user_id)));
            }
            let fire = match s.armed.get(op) {
                Some((nth, mode)) if *nth == n => Some(*mode),
                _ => None,
            };
            if fire.is_some() {
                s.fired.entry(op).or_default().push(n);
            }
            let gate = match &s.gate {
                Some((g, _, _)) if *g == op => s.gate.take().map(|(_, a, b)| (a, b)),
                _ => None,
            };
            (fire, gate)
        };
        if let Some((reached, release)) = gate {
            reached.notify_one();
            release.notified().await;
        }
        match fire {
            Some(Mode::Before) => Err(MemcoreError::Internal(format!("injected {op} failure"))),
            Some(Mode::Swallow) => Ok(false),
            None => Ok(true),
        }
    }
    fn fired(&self, op: &'static str) -> Vec<usize> {
        self.0
            .lock()
            .unwrap()
            .fired
            .get(op)
            .cloned()
            .unwrap_or_default()
    }
    fn calls(&self, op: &'static str) -> usize {
        self.0.lock().unwrap().calls.get(op).copied().unwrap_or(0)
    }
    fn only_tenant(&self, t: &TenantContext) -> bool {
        let who = format!("{}/{}", t.org_id, t.user_id);
        self.0
            .lock()
            .unwrap()
            .tenants
            .iter()
            .all(|(_, w)| *w == who)
    }
}

struct PFacts(Arc<PostgresFactStore>, Arc<Plan>);

#[async_trait]
impl FactStore for PFacts {
    async fn insert_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        self.1.hit("FactStore.insert_fact", Some(t)).await?;
        self.0.insert_fact(t, f).await
    }
    async fn update_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        self.1.hit("FactStore.update_fact", Some(t)).await?;
        self.0.update_fact(t, f).await
    }
    async fn get_fact(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<Option<Fact>> {
        self.1.hit("FactStore.get_fact", Some(t)).await?;
        self.0.get_fact(t, id).await
    }
    async fn get_fact_including_deleted(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.1
            .hit("FactStore.get_fact_including_deleted", Some(t))
            .await?;
        self.0.get_fact_including_deleted(t, id).await
    }
    async fn list_fact_states(
        &self,
        t: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        self.0.list_fact_states(t, after, limit).await
    }
    async fn fact_id_exists_in_any_tenant(&self, id: Uuid) -> MemcoreResult<bool> {
        self.1
            .hit("FactStore.fact_id_exists_in_any_tenant", None)
            .await?;
        self.0.fact_id_exists_in_any_tenant(id).await
    }
    async fn search_facts(&self, q: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        self.0.search_facts(q).await
    }
    async fn fulltext_search_facts(&self, q: FactFulltextQuery) -> MemcoreResult<Vec<ScoredFact>> {
        self.0.fulltext_search_facts(q).await
    }
    async fn soft_delete_fact(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        self.1.hit("FactStore.soft_delete_fact", Some(t)).await?;
        self.0.soft_delete_fact(t, id).await
    }
    async fn delete_user_data(&self, t: &TenantContext) -> MemcoreResult<()> {
        self.1.hit("FactStore.delete_user_data", Some(t)).await?;
        self.0.delete_user_data(t).await
    }
    async fn delete_facts_older_than(
        &self,
        t: &TenantContext,
        cutoff: DateTime<Utc>,
        dry: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        self.0.delete_facts_older_than(t, cutoff, dry).await
    }
    async fn count_facts_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.0.count_facts_by_org(o).await
    }
    async fn count_active_facts_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.0.count_active_facts_by_org(o).await
    }
    async fn count_facts_by_user(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.0.count_facts_by_user(t).await
    }
    async fn count_users_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.0.count_users_by_org(o).await
    }
    async fn list_users_by_org(&self, q: OrgUserListQuery) -> MemcoreResult<Vec<OrgUserSummary>> {
        self.0.list_users_by_org(q).await
    }
}

struct PVectors(&'static str, Arc<QdrantVectorStore>, Arc<Plan>);

impl PVectors {
    fn op(&self, op: &'static str) -> &'static str {
        match (self.0, op) {
            ("facts", "upsert_vector") => "VectorStore.upsert_vector",
            ("facts", "write_vector") => "VectorStore.write_vector",
            ("facts", "delete_by_fact_id") => "VectorStore.delete_by_fact_id",
            ("facts", "delete_by_user") => "VectorStore.delete_by_user",
            ("facts", "search_vectors") => "VectorStore.search_vectors",
            ("episodes", "upsert_vector") => "EpisodeVectors.upsert_vector",
            ("episodes", "write_vector") => "EpisodeVectors.write_vector",
            ("episodes", "delete_by_fact_id") => "EpisodeVectors.delete_by_fact_id",
            ("episodes", "delete_by_user") => "EpisodeVectors.delete_by_user",
            ("episodes", "search_vectors") => "EpisodeVectors.search_vectors",
            _ => "other",
        }
    }
}

#[async_trait]
impl VectorStore for PVectors {
    async fn upsert_vector(&self, t: &TenantContext, r: VectorRecord) -> MemcoreResult<()> {
        if !self.2.hit(self.op("upsert_vector"), Some(t)).await? {
            return Ok(());
        }
        self.1.upsert_vector(t, r).await
    }
    async fn write_vector(
        &self,
        t: &TenantContext,
        r: VectorRecord,
        i: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        if !self.2.hit(self.op("write_vector"), Some(t)).await? {
            return Ok(());
        }
        self.1.write_vector(t, r, i).await
    }
    async fn search_vectors(&self, q: VectorSearchQuery) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.2
            .hit(self.op("search_vectors"), Some(&q.tenant))
            .await?;
        self.1.search_vectors(q).await
    }
    async fn delete_by_fact_id(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        self.2.hit(self.op("delete_by_fact_id"), Some(t)).await?;
        self.1.delete_by_fact_id(t, id).await
    }
    async fn delete_by_user(&self, t: &TenantContext) -> MemcoreResult<()> {
        self.2.hit(self.op("delete_by_user"), Some(t)).await?;
        self.1.delete_by_user(t).await
    }
    async fn fact_vector_points(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.1.fact_vector_points(t, id).await
    }
}

struct PEpisodes(Arc<PostgresEpisodeStore>, Arc<Plan>);

#[async_trait]
impl EpisodeStore for PEpisodes {
    async fn insert_episodes(
        &self,
        t: &TenantContext,
        e: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        self.1.hit("EpisodeStore.insert_episodes", Some(t)).await?;
        self.0.insert_episodes(t, e).await
    }
    async fn insert_episodes_checked(
        &self,
        t: &TenantContext,
        e: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        self.1
            .hit("EpisodeStore.insert_episodes_checked", Some(t))
            .await?;
        self.0.insert_episodes_checked(t, e).await
    }
    async fn get_episodes(&self, t: &TenantContext, ids: &[Uuid]) -> MemcoreResult<Vec<Episode>> {
        self.0.get_episodes(t, ids).await
    }
    async fn mark_episodes_embedded(&self, t: &TenantContext, ids: &[Uuid]) -> MemcoreResult<()> {
        self.1
            .hit("EpisodeStore.mark_episodes_embedded", Some(t))
            .await?;
        self.0.mark_episodes_embedded(t, ids).await
    }
    async fn list_episodes(&self, t: &TenantContext, limit: usize) -> MemcoreResult<Vec<Episode>> {
        self.0.list_episodes(t, limit).await
    }
    async fn list_episodes_after(
        &self,
        t: &TenantContext,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        self.0.list_episodes_after(t, after, limit).await
    }
    async fn delete_user_episodes(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.1
            .hit("EpisodeStore.delete_user_episodes", Some(t))
            .await?;
        self.0.delete_user_episodes(t).await
    }
    async fn delete_episodes_recorded_before(
        &self,
        t: &TenantContext,
        cutoff: DateTime<Utc>,
        dry: bool,
    ) -> MemcoreResult<Vec<Uuid>> {
        self.0.delete_episodes_recorded_before(t, cutoff, dry).await
    }
    async fn count_episodes_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.0.count_episodes_by_org(o).await
    }
    async fn episode_id_exists_in_any_tenant(&self, id: Uuid) -> MemcoreResult<bool> {
        self.0.episode_id_exists_in_any_tenant(id).await
    }
}

struct PEvents(Arc<PostgresMemoryEventStore>, Arc<Plan>);

#[async_trait]
impl MemoryEventStore for PEvents {
    async fn record_event(&self, t: &TenantContext, e: MemoryEvent) -> MemcoreResult<MemoryEvent> {
        self.1.hit("MemoryEventStore.record_event", Some(t)).await?;
        self.0.record_event(t, e).await
    }
    async fn list_events(&self, q: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        self.0.list_events(q).await
    }
    async fn list_events_by_org(&self, q: OrgMemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        self.0.list_events_by_org(q).await
    }
    async fn delete_events_older_than(
        &self,
        t: &TenantContext,
        c: DateTime<Utc>,
        d: bool,
    ) -> MemcoreResult<usize> {
        self.0.delete_events_older_than(t, c, d).await
    }
    async fn count_events_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.0.count_events_by_org(o).await
    }
    async fn erase_user_audit_content(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.0.erase_user_audit_content(t).await
    }
    async fn count_user_audit_content(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.0.count_user_audit_content(t).await
    }
}

struct PEmbedding(MockEmbeddingProvider, Arc<Plan>);

#[async_trait]
impl EmbeddingProvider for PEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        self.1.hit("EmbeddingProvider.embed_text", None).await?;
        self.0.embed_text(text).await
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        self.1.hit("EmbeddingProvider.embed_batch", None).await?;
        self.0.embed_batch(texts).await
    }
    fn dimensions(&self) -> usize {
        self.0.dimensions()
    }
}

// ---------------------------------------------------------------------------------------------
// Real-backend world
// ---------------------------------------------------------------------------------------------

struct World {
    _guard: qdrant_cleanup::TestCollections,
    qurl: String,
    fact_collection: String,
    episode_collection: String,
    facts: Arc<PostgresFactStore>,
    episodes: Arc<PostgresEpisodeStore>,
    events: Arc<PostgresMemoryEventStore>,
    entities: Arc<PostgresEntityStore>,
    vectors: Arc<QdrantVectorStore>,
    episode_vectors: Arc<QdrantVectorStore>,
    plan: Arc<Plan>,
    cache: Arc<InMemoryContextCache>,
    tag: String,
}

impl World {
    /// A world on the PostgreSQL database `database` (fresh, recreated by the runner) and fresh
    /// test-owned Qdrant collections. `tag` makes this test's tenants unique in a shared database.
    async fn open(database: &str, tag: &str) -> Option<Self> {
        let (Some(url), Some(qurl)) = (pg_url(database), qdrant_url()) else {
            eprintln!(
                "skipping T2 real-backend test: MEMCORE_TEST_POSTGRES_URL / MEMCORE_TEST_QDRANT_URL not set"
            );
            return None;
        };
        let facts = Arc::new(
            PostgresFactStore::connect(&url)
                .await
                .expect("postgres (migrations applied)"),
        );
        let pool = facts.pool();
        let episodes = Arc::new(PostgresEpisodeStore::new(pool.clone()));
        let events = Arc::new(PostgresMemoryEventStore::new(pool.clone()));
        let entities = Arc::new(PostgresEntityStore::new(pool));
        let guard = qdrant_cleanup::TestCollections::new(qurl.clone());
        let fact_collection = guard.name(&format!("t2{tag}"));
        let episode_collection = format!("{fact_collection}_episodes");
        let vectors = Arc::new(
            QdrantVectorStore::connect(&qurl, &fact_collection, DIMS)
                .await
                .expect("qdrant facts"),
        );
        let episode_vectors = Arc::new(
            QdrantVectorStore::connect(&qurl, &episode_collection, DIMS)
                .await
                .expect("qdrant episodes"),
        );
        Some(Self {
            _guard: guard,
            qurl,
            fact_collection,
            episode_collection,
            facts,
            episodes,
            events,
            entities,
            vectors,
            episode_vectors,
            plan: Arc::new(Plan::default()),
            cache: Arc::new(InMemoryContextCache::new(1000)),
            tag: tag.to_string(),
        })
    }

    fn t(&self, n: usize) -> TenantContext {
        match n {
            0 => TenantContext::new(format!("org_{}_a", self.tag), "u1").unwrap(),
            1 => TenantContext::new(format!("org_{}_a", self.tag), "u2").unwrap(),
            _ => TenantContext::new(format!("org_{}_b", self.tag), "u1").unwrap(),
        }
    }

    fn canary(&self, n: usize, k: usize) -> String {
        let t = self.t(n);
        format!("CANARY-{}-{}-{k}", t.org_id, t.user_id)
    }

    fn engine(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            Arc::new(PFacts(self.facts.clone(), self.plan.clone())),
            Arc::new(PVectors("facts", self.vectors.clone(), self.plan.clone())),
            Arc::new(llm),
            Arc::new(PEmbedding(
                MockEmbeddingProvider::new(DIMS),
                self.plan.clone(),
            )),
        )
        .with_event_store(Arc::new(PEvents(self.events.clone(), self.plan.clone())))
        .with_min_importance(0.0)
        .with_episodes(Some(EpisodeComponents {
            store: Arc::new(PEpisodes(self.episodes.clone(), self.plan.clone())),
            vectors: Arc::new(PVectors(
                "episodes",
                self.episode_vectors.clone(),
                self.plan.clone(),
            )),
        }))
        .with_entities(Some(EntityComponents {
            store: self.entities.clone(),
            config: EntityConfig {
                name_similarity_threshold: 0.9,
                llm_confirmation: false,
                embedding_model: "mock/mock-embedding".to_string(),
            },
        }))
        .with_context_cache(
            self.cache.clone(),
            ContextCacheConfig {
                enabled: true,
                ..ContextCacheConfig::default()
            },
        )
        .with_search_max_limit(100)
        .expect("search limit")
    }

    fn plain(&self) -> MemoryEngine {
        self.engine(MockLlmProvider::new().with_extraction_candidates(vec![]))
    }

    async fn states(&self, n: usize) -> Vec<StoredFactState> {
        self.facts
            .list_fact_states(&self.t(n), None, 10_000)
            .await
            .unwrap()
    }

    async fn fact_with(&self, n: usize, marker: &str) -> Fact {
        self.states(n)
            .await
            .into_iter()
            .map(|s| s.fact)
            .find(|f| f.content.contains(marker))
            .expect("fixture fact")
    }

    /// Fact points of a tenant read from Qdrant directly: (point id, fact id, content, type).
    async fn points(&self, collection: &str, n: usize) -> Vec<(String, String, String, String)> {
        let t = self.t(n);
        let mut out = Vec::new();
        let mut cursor = None;
        let store = QdrantVectorStore::connect_existing(&self.qurl, collection, DIMS)
            .await
            .unwrap()
            .unwrap();
        loop {
            let page = store
                .scroll_points(&PointScope::Tenant(t.clone()), cursor, 256)
                .await
                .unwrap();
            for p in page.points {
                out.push((
                    p.point_id,
                    p.entity_id.unwrap_or_default(),
                    p.content.unwrap_or_default(),
                    format!("{:?}", p.memory_type),
                ));
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        out.sort();
        out
    }

    async fn fact_points(&self, n: usize) -> Vec<(String, String, String, String)> {
        self.points(&self.fact_collection, n).await
    }

    async fn points_of(&self, n: usize, fact: Uuid) -> Vec<(String, String, String, String)> {
        self.fact_points(n)
            .await
            .into_iter()
            .filter(|p| p.1 == fact.to_string())
            .collect()
    }

    async fn episode_rows(&self, n: usize) -> Vec<Episode> {
        let mut e = self
            .episodes
            .list_episodes(&self.t(n), 10_000)
            .await
            .unwrap();
        e.sort_by_key(|e| e.id);
        e
    }

    async fn event_rows(&self, n: usize) -> Vec<MemoryEvent> {
        self.events
            .list_events(MemoryEventQuery::new(self.t(n), 200))
            .await
            .unwrap()
    }

    /// `S(tenant)` read from PostgreSQL and Qdrant directly.
    async fn snapshot(&self, n: usize) -> String {
        let mut rows: Vec<String> = self
            .states(n)
            .await
            .iter()
            .map(|s| format!("{}|{}|{}", s.fact.id, s.fact.content, s.deleted))
            .collect();
        rows.sort();
        let eps: Vec<String> = self
            .episode_rows(n)
            .await
            .iter()
            .map(|e| format!("{}|{}|{}", e.id, e.content, e.embedded))
            .collect();
        let entities = self
            .entities
            .list_all_entities(&self.t(n))
            .await
            .unwrap()
            .len();
        format!(
            "{rows:?}\n{:?}\n{eps:?}\n{:?}\nevents={} entities={entities}",
            self.fact_points(n).await,
            self.points(&self.episode_collection, n).await,
            self.event_rows(n).await.len()
        )
    }

    async fn others(&self) -> (String, String) {
        (self.snapshot(1).await, self.snapshot(2).await)
    }

    /// consistent(T): every live fact has exactly one point with the row's text, no point lacks a
    /// live row, every embedded embeddable episode has exactly one point.
    async fn consistent(&self, n: usize) -> Result<(), String> {
        let live: HashMap<String, String> = self
            .states(n)
            .await
            .into_iter()
            .filter(|s| !s.deleted)
            .map(|s| (s.fact.id.to_string(), s.fact.content))
            .collect();
        let points = self.fact_points(n).await;
        for (id, content) in &live {
            let mine: Vec<_> = points.iter().filter(|p| &p.1 == id).collect();
            if mine.len() != 1 || mine[0].2 != *content {
                return Err(format!("fact {id}: {mine:?}"));
            }
        }
        if let Some(p) = points.iter().find(|p| !live.contains_key(&p.1)) {
            return Err(format!("point without live row: {p:?}"));
        }
        let eps = self.points(&self.episode_collection, n).await;
        for e in self.episode_rows(n).await {
            if e.embedded
                && e.is_embeddable()
                && eps.iter().filter(|p| p.0 == e.id.to_string()).count() != 1
            {
                return Err(format!("episode {}", e.id));
            }
        }
        Ok(())
    }

    /// Plants a point directly in Qdrant (a divergent state), with an explicit point id.
    async fn plant_point(
        &self,
        n: usize,
        point: Uuid,
        fact: Uuid,
        content: &str,
        memory_type: &str,
    ) {
        let t = self.t(n);
        raw_qdrant(&self.qurl)
            .upsert_points(
                UpsertPointsBuilder::new(
                    &self.fact_collection,
                    vec![PointStruct::new(
                        point.to_string(),
                        deterministic_embedding(content, DIMS).unwrap(),
                        qdrant_client::Payload::try_from(json!({
                            "id": point.to_string(), "fact_id": fact.to_string(), "org_id": t.org_id, "user_id": t.user_id,
                            "content": content, "memory_type": memory_type, "metadata": "{}"
                        }))
                        .unwrap(),
                    )],
                )
                .wait(true),
            )
            .await
            .unwrap();
    }

    /// FX (protocol §2) for the three tenants, through the engine, no fault.
    async fn seed(&self) {
        for n in 0..3 {
            let candidates = (1..=3)
                .map(|k| candidate(&self.fact_text(n, k), &format!("F{k}")))
                .collect();
            let out = self
                .engine(MockLlmProvider::new().with_extraction_candidates(candidates))
                .add_memory(AddMemoryInput {
                    tenant: self.t(n),
                    messages: self.episode_messages(n),
                    metadata: json!({}),
                    session: Default::default(),
                })
                .await
                .expect("seed");
            assert_eq!(out.added, 3, "seed tenant {n}");
        }
        self.plan.reset();
    }

    fn fact_text(&self, n: usize, k: usize) -> String {
        let topic = [
            "kayaking on mountain lakes",
            "baking sourdough bread",
            "restoring vintage radios",
        ];
        format!("Fact {k} {} prefers {}", self.canary(n, k), topic[k - 1])
    }

    fn episode_messages(&self, n: usize) -> Vec<MemoryMessage> {
        let mut m: Vec<MemoryMessage> = (1..=3)
            .map(|k| MemoryMessage {
                role: MessageRole::User,
                content: format!(
                    "Episode {k} message {} about weekend plans",
                    self.canary(n, 10 + k)
                ),
                message_id: Some(format!("e{k}")),
                ..Default::default()
            })
            .collect();
        m.push(MemoryMessage {
            role: MessageRole::User,
            content: "short message 4".to_string(),
            message_id: Some("e4".to_string()),
            ..Default::default()
        });
        m
    }

    fn request(&self, n: usize, id: &str) -> AddMemoryInput {
        AddMemoryInput {
            tenant: self.t(n),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: format!("Message {id} for {} with enough text", self.canary(n, 20)),
                message_id: Some(id.to_string()),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        }
    }

    async fn search(
        &self,
        n: usize,
        q: &str,
        mode: RetrievalMode,
    ) -> Vec<(Uuid, String, Value, MemoryType)> {
        self.plain()
            .search_memory_with_options(
                SearchMemoryInput {
                    tenant: self.t(n),
                    query: q.to_string(),
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
            .map(|r| (r.fact_id, r.content, r.metadata, r.memory_type))
            .collect()
    }

    async fn context(&self, n: usize, q: &str) -> String {
        self.plain()
            .build_context(BuildContextInput {
                tenant: self.t(n),
                query: q.to_string(),
                ..BuildContextInput::default()
            })
            .await
            .expect("context")
            .context
    }
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

fn extracting(text: &str) -> MockLlmProvider {
    MockLlmProvider::new().with_extraction_candidates(vec![candidate(text, "new")])
}

fn updating(text: &str, target: Uuid) -> MockLlmProvider {
    extracting(text).with_classification_decision(FactOperationDecision {
        operation: FactOperation::Update,
        target_fact_id: Some(target),
        reason: Some("t2 update".to_string()),
        confidence: 0.9,
    })
}

async fn ingest(
    w: &World,
    llm: MockLlmProvider,
    input: AddMemoryInput,
) -> MemcoreResult<AddMemoryOutput> {
    w.engine(llm).add_memory(input).await
}

macro_rules! world {
    ($db:expr, $tag:expr) => {
        match World::open($db, $tag).await {
            Some(w) => w,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------------------------------
// G3: R1 / E1 read gates
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn g3_r1_stale_orphan_and_foreign_points_are_never_served_on_real_backends() {
    let w = world!("ar023t2_e2e", "r1");
    w.seed().await;
    let x2 = w.others().await;
    let (f1, f2, f3) = (
        w.fact_with(0, &w.canary(0, 1)).await,
        w.fact_with(0, &w.canary(0, 2)).await,
        w.fact_with(0, &w.canary(0, 3)).await,
    );
    let consistent_before = w.search(0, &f3.content, RetrievalMode::Semantic).await;
    // R1-a stale text: the row is updated directly in PostgreSQL; the point keeps the old text.
    let mut updated = f1.clone();
    updated.content = format!("Fact 1 {} now prefers ocean rowing", w.canary(0, 1));
    w.facts.update_fact(&w.t(0), updated.clone()).await.unwrap();
    // R1-b stale memory type: F2's point is rewritten with another memory type.
    let f2_point = w.points_of(0, f2.id).await[0].0.clone();
    w.plant_point(
        0,
        Uuid::parse_str(&f2_point).unwrap(),
        f2.id,
        &f2.content,
        "event",
    )
    .await;
    // R1-c soft-deleted orphan: F3 soft-deleted in PostgreSQL directly, point kept.
    w.facts.soft_delete_fact(&w.t(0), f3.id).await.unwrap();
    // R1-d hard-deleted orphan: a point for a fact id with no row.
    let absent = Uuid::new_v4();
    w.plant_point(
        0,
        Uuid::new_v4(),
        absent,
        &format!("ORPHAN-TEXT {} no row", w.canary(0, 5)),
        "skill",
    )
    .await;
    // R1-e foreign: a TA-attributed point naming TB's live fact id.
    let b1 = w.fact_with(1, &w.canary(1, 1)).await;
    w.plant_point(
        0,
        Uuid::new_v4(),
        b1.id,
        &format!("FOREIGN-TEXT {} pointing at TB", w.canary(0, 6)),
        "skill",
    )
    .await;

    let mut served = Vec::new();
    for mode in [
        RetrievalMode::Semantic,
        RetrievalMode::Hybrid,
        RetrievalMode::Rrf,
    ] {
        for q in [
            f1.content.as_str(),
            f2.content.as_str(),
            f3.content.as_str(),
            "ORPHAN-TEXT",
            "FOREIGN-TEXT",
            updated.content.as_str(),
        ] {
            for (id, content, metadata, memory_type) in w.search(0, q, mode).await {
                assert!(
                    !content.contains("mountain lakes"),
                    "R1-a: stale text never served"
                );
                assert!(
                    !content.contains("ORPHAN-TEXT") && !content.contains("FOREIGN-TEXT"),
                    "R1-d/e: orphan or foreign payload served"
                );
                assert_ne!(id, f3.id, "R1-c: soft-deleted fact served");
                assert_ne!(id, b1.id, "R1-e: TB's fact served to TA");
                assert_ne!(id, absent);
                let row = w
                    .facts
                    .get_fact(&w.t(0), id)
                    .await
                    .unwrap()
                    .expect("a served hit has a live TA row");
                assert_eq!(content, row.content, "X5: content from SQL");
                assert_eq!(metadata, row.metadata, "metadata from SQL");
                assert_eq!(
                    memory_type, row.memory_type,
                    "memory type from SQL, never the stale payload"
                );
                served.push(id);
            }
        }
    }
    // R1-b: F2 is served (if at all) with SQL's memory type, already asserted. Consistent hits:
    let f3_gone = w.search(0, &f3.content, RetrievalMode::Semantic).await;
    assert!(
        consistent_before.iter().any(|h| h.0 == f3.id),
        "the consistent fact was found before the change"
    );
    assert!(f3_gone.iter().all(|h| h.0 != f3.id));
    let context = w.context(0, &f1.content).await;
    assert!(
        !context.contains("mountain lakes")
            && !context.contains("ORPHAN-TEXT")
            && !context.contains("FOREIGN-TEXT"),
        "R1-D: renderer gets SQL text only"
    );
    assert_eq!(w.others().await, x2, "X2");
    eprintln!("G3 R1: {} served hits, all SQL-confirmed", served.len());
}

#[tokio::test]
async fn g3_e1_a_failing_sql_fetch_fails_the_search_without_payload_text() {
    let w = world!("ar023t2_e2e", "e1");
    w.seed().await;
    let f1 = w.fact_with(0, &w.canary(0, 1)).await;
    w.plan.arm("FactStore.get_fact", 1, Mode::Before);
    let result = w
        .plain()
        .search_memory_with_options(
            SearchMemoryInput {
                tenant: w.t(0),
                query: f1.content.clone(),
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
        .await;
    let fired = w.plan.fired("FactStore.get_fact");
    match result {
        Err(error) => {
            assert_eq!(fired, vec![1], "X1");
            assert!(
                !error.to_string().contains("mountain lakes"),
                "no payload text in the error"
            );
        }
        Ok(out) => {
            // The engine may fetch the hit rows in one batch call other than get_fact.
            assert!(
                out.results
                    .iter()
                    .all(|r| !r.content.contains("mountain lakes")
                        || w.plan.calls("FactStore.get_fact") == 0)
            );
            panic!(
                "E1: expected an error when the per-hit SQL fetch fails (fired {fired:?}, results {})",
                out.results.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// G3: R2 dedup gates
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn g3_r2_dedup_never_suppresses_on_an_orphan_stale_or_foreign_hit() {
    let w = world!("ar023t2_e2e", "r2");
    w.seed().await;
    let x2 = w.others().await;
    let f1 = w.fact_with(0, &w.canary(0, 1)).await;
    let f2 = w.fact_with(0, &w.canary(0, 2)).await;
    // R2-a soft-deleted orphan: F2 soft-deleted directly, its point kept; ingest F2's exact text.
    w.facts.soft_delete_fact(&w.t(0), f2.id).await.unwrap();
    let out = ingest(&w, extracting(&f2.content), w.request(0, "r2a"))
        .await
        .expect("200");
    assert_eq!(
        out.added, 1,
        "R2-a: a new live fact; the deleted orphan did not suppress"
    );
    // R2-b hard-deleted orphan with text Y.
    let y = format!("Orphan text Y {} sailing dinghies", w.canary(0, 7));
    w.plant_point(0, Uuid::new_v4(), Uuid::new_v4(), &y, "skill")
        .await;
    let out = ingest(&w, extracting(&y), w.request(0, "r2b"))
        .await
        .expect("200");
    assert_eq!(out.added, 1, "R2-b: new fact despite the orphan point");
    // R2-e foreign: a TA-attributed point naming TB's fact with text Z.
    let b2 = w.fact_with(1, &w.canary(1, 2)).await;
    let z = format!("Foreign text Z {} pottery classes", w.canary(0, 8));
    w.plant_point(0, Uuid::new_v4(), b2.id, &z, "skill").await;
    let out = ingest(&w, extracting(&z), w.request(0, "r2e"))
        .await
        .expect("200");
    assert_eq!(out.added, 1, "R2-e: new fact despite the foreign point");
    // R2-c stale point: F1's row updated directly; ingest F1's OLD text -> never suppressed by the stale point.
    let mut updated = f1.clone();
    updated.content = format!("Fact 1 {} now prefers canyoning", w.canary(0, 1));
    w.facts.update_fact(&w.t(0), updated).await.unwrap();
    let out = ingest(&w, extracting(&f1.content), w.request(0, "r2c"))
        .await
        .expect("200");
    assert_eq!(
        out.noop, 0,
        "R2-c: the stale point did not make the old text a duplicate"
    );
    // R2-d genuine duplicate: F3's current text again -> NOOP, no new row.
    let f3 = w.fact_with(0, &w.canary(0, 3)).await;
    let rows = w.states(0).await.len();
    let out = ingest(&w, extracting(&f3.content), w.request(0, "r2d"))
        .await
        .expect("200");
    assert_eq!(
        (out.added, out.noop),
        (0, 1),
        "R2-d: genuine duplicate is a NOOP"
    );
    assert_eq!(w.states(0).await.len(), rows, "no new row");
    assert_eq!(w.others().await, x2, "X2");
}

// ---------------------------------------------------------------------------------------------
// G3: R3 delete + cache, R4 heal
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn g3_d1_d2_d3_failed_vector_cleanup_is_redriven_and_the_cache_is_invalidated() {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "d1");
    w.seed().await;
    let x2 = w.others().await;
    let f1 = w.fact_with(0, &w.canary(0, 1)).await;
    let cached = w.context(0, &f1.content).await;
    assert!(
        cached.contains(&w.canary(0, 1)),
        "the context (now cached) contains F1"
    );
    w.plan.arm("VectorStore.delete_by_fact_id", 1, Mode::Before);
    let d1 = w
        .plain()
        .delete_memory(DeleteMemoryInput {
            tenant: w.t(0),
            memory_id: f1.id,
        })
        .await;
    assert!(d1.is_err(), "D1: 5xx");
    assert_eq!(w.plan.fired("VectorStore.delete_by_fact_id"), vec![1], "X1");
    let state = w
        .facts
        .get_fact_including_deleted(&w.t(0), f1.id)
        .await
        .unwrap()
        .expect("row");
    assert!(state.deleted, "D1: soft-deleted in PostgreSQL");
    assert_eq!(
        w.points_of(0, f1.id).await.len(),
        1,
        "D1: the point remains in Qdrant"
    );
    assert!(
        !w.context(0, &f1.content).await.contains(&w.canary(0, 1)),
        "D-CACHE: the same context key no longer serves F1"
    );
    for mode in [RetrievalMode::Semantic, RetrievalMode::Hybrid] {
        assert!(
            w.search(0, &f1.content, mode)
                .await
                .iter()
                .all(|h| h.0 != f1.id),
            "invisible to search"
        );
    }
    assert_eq!(
        captured.failures(),
        expected(&[("fact_delete", 1)]),
        "R6 fact_delete"
    );
    w.plan.reset();
    let d2 = w
        .plain()
        .delete_memory(DeleteMemoryInput {
            tenant: w.t(0),
            memory_id: f1.id,
        })
        .await
        .expect("D2: 200");
    assert!(d2.deleted);
    assert!(w.points_of(0, f1.id).await.is_empty(), "D2: no F1 point");
    let d3 = w
        .plain()
        .delete_memory(DeleteMemoryInput {
            tenant: w.t(0),
            memory_id: f1.id,
        })
        .await
        .expect("D3: 200");
    assert!(d3.deleted);
    let mut q = MemoryEventQuery::new(w.t(0), 100);
    q.fact_id = Some(f1.id);
    q.operation = Some(MemoryEventOperation::Delete);
    assert_eq!(
        w.events.list_events(q).await.unwrap().len(),
        1,
        "exactly one delete event in PostgreSQL"
    );
    w.consistent(0).await.expect("consistent(TA)");
    // D4 (engine): unknown, TB live and TB deleted ids are the same NotFound.
    let b1 = w.fact_with(1, &w.canary(1, 1)).await;
    let b2 = w.fact_with(1, &w.canary(1, 2)).await;
    w.facts.soft_delete_fact(&w.t(1), b2.id).await.unwrap();
    let x2b = w.others().await;
    let mut bodies = Vec::new();
    for id in [Uuid::new_v4(), b1.id, b2.id] {
        let e = w
            .plain()
            .delete_memory(DeleteMemoryInput {
                tenant: w.t(0),
                memory_id: id,
            })
            .await
            .expect_err("404");
        bodies.push(format!("{e:?}").replace(&id.to_string(), "<id>"));
    }
    assert!(
        bodies.windows(2).all(|p| p[0] == p[1]),
        "D4: indistinguishable: {bodies:?}"
    );
    assert!(matches!(
        w.plain()
            .delete_memory(DeleteMemoryInput {
                tenant: w.t(0),
                memory_id: b1.id
            })
            .await,
        Err(MemcoreError::NotFound(_))
    ));
    assert_eq!(w.others().await, x2b, "X2: no cleanup of TB");
    let _ = x2;
}

#[tokio::test]
async fn g3_w1_w2_r4_heal_on_real_qdrant_keeps_point_ids_and_detects_lost_writes() {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "r4");
    w.seed().await;
    let x2 = w.others().await;

    // W1: the new fact's vector write fails.
    let text = format!("Fact 8 {} prefers carving wooden spoons", w.canary(0, 8));
    w.plan.arm("VectorStore.write_vector", 1, Mode::Before);
    assert!(
        ingest(&w, extracting(&text), w.request(0, "w1"))
            .await
            .is_err(),
        "W1: 5xx"
    );
    assert_eq!(w.plan.fired("VectorStore.write_vector"), vec![1], "X1");
    let row = w.fact_with(0, &w.canary(0, 8)).await;
    assert!(
        w.points_of(0, row.id).await.is_empty(),
        "W1: live row, no point (read from Qdrant)"
    );
    assert!(
        w.search(0, &text, RetrievalMode::Semantic)
            .await
            .iter()
            .all(|h| h.0 != row.id),
        "W1: semantic absent"
    );
    assert_eq!(captured.failures(), expected(&[("fact_add", 1)]));
    // W1-R: the identical retry heals.
    w.plan.reset();
    let out = ingest(&w, extracting(&text), w.request(0, "w1"))
        .await
        .expect("W1-R: 200");
    assert_eq!((out.added, out.noop), (0, 1));
    let pts = w.points_of(0, row.id).await;
    assert_eq!(pts.len(), 1, "W1-R: one point");
    assert_eq!(pts[0].2, text);
    assert_eq!(w.states(0).await.len(), 4, "no new fact row");

    // W2 / W2-R with a LEGACY random point id: the replacement keeps that id on real Qdrant.
    let f1 = w.fact_with(0, &w.canary(0, 1)).await;
    let legacy_point = w.points_of(0, f1.id).await[0].0.clone();
    let new_text = format!(
        "Fact 1 {} now prefers sea kayaking at night",
        w.canary(0, 1)
    );
    w.plan.arm("VectorStore.write_vector", 1, Mode::Before);
    assert!(
        ingest(&w, updating(&new_text, f1.id), w.request(0, "w2"))
            .await
            .is_err(),
        "W2: 5xx"
    );
    assert_eq!(w.plan.fired("VectorStore.write_vector"), vec![1]);
    assert_eq!(
        w.points_of(0, f1.id).await[0].2,
        f1.content,
        "W2: the point keeps the old text"
    );
    assert_eq!(
        captured.failures(),
        expected(&[("fact_add", 1), ("fact_update", 1)])
    );
    w.plan.reset();
    let out = ingest(&w, updating(&new_text, f1.id), w.request(0, "w2"))
        .await
        .expect("W2-R: 200");
    assert_eq!(out.noop, 1);
    let pts = w.points_of(0, f1.id).await;
    assert_eq!(pts.len(), 1);
    assert_eq!(pts[0].2, new_text, "W2-R: the point carries the new text");
    assert_eq!(
        pts[0].0, legacy_point,
        "R4 on real Qdrant: the existing (random, legacy) point id is kept"
    );

    // R4-M: three points on F2 converge to one.
    let f2 = w.fact_with(0, &w.canary(0, 2)).await;
    for k in 0..2 {
        w.plant_point(
            0,
            Uuid::new_v4(),
            f2.id,
            &format!("extra point {k} for F2"),
            "skill",
        )
        .await;
    }
    assert_eq!(w.points_of(0, f2.id).await.len(), 3);
    let out = ingest(&w, extracting(&f2.content), w.request(0, "r4m"))
        .await
        .expect("R4-M: 200");
    assert_eq!(out.noop, 1);
    let pts = w.points_of(0, f2.id).await;
    assert_eq!(pts.len(), 1, "R4-M: exactly one point");
    assert_eq!(pts[0].2, f2.content);

    // R4-S: a swallowed repair write (reports success, writes nothing) is detected.
    let f3 = w.fact_with(0, &w.canary(0, 3)).await;
    w.vectors.delete_by_fact_id(&w.t(0), f3.id).await.unwrap();
    w.plan.arm("VectorStore.write_vector", 1, Mode::Swallow);
    let swallowed = ingest(&w, extracting(&f3.content), w.request(0, "r4s")).await;
    assert_eq!(
        w.plan.fired("VectorStore.write_vector"),
        vec![1],
        "X1 (swallow)"
    );
    assert!(
        swallowed.is_err(),
        "R4-S: the post-write verification reports the lost write"
    );
    assert!(w.points_of(0, f3.id).await.is_empty());
    assert_eq!(
        captured.failures(),
        expected(&[("fact_add", 1), ("fact_update", 1), ("fact_heal", 1)]),
        "R6 fact_heal"
    );
    w.plan.reset();
    ingest(&w, extracting(&f3.content), w.request(0, "r4s"))
        .await
        .expect("heal on the next retry");

    // R4-H: a healthy NOOP makes no repair embedding and no vector write.
    w.consistent(0)
        .await
        .expect("consistent before the healthy NOOP");
    let before = w.fact_points(0).await;
    w.plan.reset();
    let out = ingest(&w, extracting(&f3.content), w.request(0, "r4h"))
        .await
        .expect("200");
    assert_eq!(out.noop, 1);
    assert_eq!(
        w.plan.calls("VectorStore.write_vector") + w.plan.calls("VectorStore.upsert_vector"),
        0,
        "R4-H: no fact vector write"
    );
    assert_eq!(w.fact_points(0).await, before, "R4-H: points unchanged");
    w.consistent(0).await.expect("consistent(TA)");
    assert!(w.plan.only_tenant(&w.t(0)), "X2 (log)");
    assert_eq!(w.others().await, x2, "X2");
}

// ---------------------------------------------------------------------------------------------
// G3: W6 / W6-E / W7 / FU1 on real backends
// ---------------------------------------------------------------------------------------------

fn w6_request(w: &World) -> AddMemoryInput {
    AddMemoryInput {
        tenant: w.t(0),
        messages: vec![
            MemoryMessage {
                role: MessageRole::User,
                content: format!(
                    "New episode A {} about the harbour festival",
                    w.canary(0, 21)
                ),
                message_id: Some("w6a".into()),
                ..Default::default()
            },
            MemoryMessage {
                role: MessageRole::User,
                content: format!("New episode B {} about the night market", w.canary(0, 22)),
                message_id: Some("w6b".into()),
                ..Default::default()
            },
            MemoryMessage {
                role: MessageRole::User,
                content: "tiny w6 message".into(),
                message_id: Some("w6c".into()),
                ..Default::default()
            },
        ],
        metadata: json!({}),
        session: Default::default(),
    }
}

async fn w6_case(op: &'static str, tag: &str) {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", tag);
    w.seed().await;
    let x2 = w.others().await;
    let rows_before = w.episode_rows(0).await.len();
    let points_before = w.points(&w.episode_collection, 0).await.len();
    w.plan.arm(op, 1, Mode::Before);
    assert!(w.plain().add_memory(w6_request(&w)).await.is_err(), "5xx");
    assert_eq!(w.plan.fired(op), vec![1], "X1 {op}");
    let new_rows: Vec<Episode> = w
        .episode_rows(0)
        .await
        .into_iter()
        .filter(|e| e.message_id.as_deref().is_some_and(|m| m.starts_with("w6")))
        .collect();
    assert_eq!(new_rows.len(), 3, "3 rows in PostgreSQL");
    assert!(new_rows.iter().all(|e| !e.embedded), "embedded = 0");
    let new_points = w.points(&w.episode_collection, 0).await.len() - points_before;
    assert_eq!(
        new_points,
        if op == "EmbeddingProvider.embed_batch" {
            0
        } else {
            2
        },
        "{op}: new episode points in Qdrant"
    );
    assert_eq!(
        captured.failures(),
        expected(&[("episode_index", 1)]),
        "R6 episode_index"
    );
    w.plan.reset();
    w.plain()
        .add_memory(w6_request(&w))
        .await
        .expect("stable-id retry 200");
    let mut flags: Vec<(String, bool)> = w
        .episode_rows(0)
        .await
        .into_iter()
        .filter_map(|e| {
            e.message_id
                .clone()
                .filter(|m| m.starts_with("w6"))
                .map(|m| (m, e.embedded))
        })
        .collect();
    flags.sort();
    assert_eq!(
        flags,
        vec![
            ("w6a".into(), true),
            ("w6b".into(), true),
            ("w6c".into(), false)
        ],
        "flags 1, 1, 0"
    );
    assert_eq!(
        w.episode_rows(0).await.len(),
        rows_before + 3,
        "no duplicate row"
    );
    assert_eq!(
        w.points(&w.episode_collection, 0).await.len(),
        points_before + 2,
        "2 points, no third"
    );
    w.consistent(0).await.expect("consistent(TA)");
    assert_eq!(w.others().await, x2, "X2");
}

#[tokio::test]
async fn g3_w6_failed_embedded_flag_on_postgres_is_completed_by_the_retry() {
    w6_case("EpisodeStore.mark_episodes_embedded", "w6").await;
}

#[tokio::test]
async fn g3_w6_e_failed_episode_embedding_is_completed_by_the_retry() {
    w6_case("EmbeddingProvider.embed_batch", "w6e").await;
}

#[tokio::test]
async fn g3_w7_a_failed_postgres_audit_event_keeps_the_200_and_is_counted() {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "w7");
    w.seed().await;
    let x2 = w.others().await;
    let text = format!("Fact 7 {} prefers mapping hiking trails", w.canary(0, 7));
    w.plan.arm("MemoryEventStore.record_event", 1, Mode::Before);
    let out = ingest(&w, extracting(&text), w.request(0, "w7"))
        .await
        .expect("W7: 200");
    assert_eq!(w.plan.fired("MemoryEventStore.record_event"), vec![1], "X1");
    assert_eq!(out.added, 1);
    let row = w.fact_with(0, &w.canary(0, 7)).await;
    assert_eq!(w.points_of(0, row.id).await.len(), 1, "1 point");
    let mut q = MemoryEventQuery::new(w.t(0), 100);
    q.fact_id = Some(row.id);
    q.operation = Some(MemoryEventOperation::Add);
    assert_eq!(
        w.events.list_events(q).await.unwrap().len(),
        0,
        "the ADD event is missing in PostgreSQL (best-effort)"
    );
    assert_eq!(captured.failures(), expected(&[("event", 1)]), "R6 event");
    w.consistent(0).await.expect("consistent(TA)");
    assert_eq!(w.others().await, x2, "X2");
}

#[tokio::test]
async fn g3_fu1_forget_user_with_a_failing_qdrant_delete_leaves_nothing_readable_even_from_the_cache()
 {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "fu1");
    w.seed().await;
    let x2 = w.others().await;
    let f1 = w.fact_with(0, &w.canary(0, 1)).await;
    // An entity row for TA, so the entity deletion is observable.
    w.entities
        .upsert_entity(
            &w.t(0),
            Entity {
                id: Uuid::new_v4(),
                org_id: w.t(0).org_id,
                user_id: w.t(0).user_id,
                canonical_name: "Avery".into(),
                entity_type: EntityType::Person,
                aliases: vec![],
                summary: None,
                is_speaker: false,
                name_embedding: None,
                name_embedding_model: None,
                name_embedding_dimensions: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let cached = w.context(0, &f1.content).await;
    assert!(
        cached.contains(&w.canary(0, 1)),
        "a context with F1 is cached before the forget"
    );
    let fact_points = w.fact_points(0).await.len();
    w.plan.arm("VectorStore.delete_by_user", 1, Mode::Before);
    assert!(
        w.plain()
            .forget_user(ForgetUserInput { tenant: w.t(0) })
            .await
            .is_err(),
        "FU1: error"
    );
    assert_eq!(w.plan.fired("VectorStore.delete_by_user"), vec![1], "X1");
    assert!(
        w.states(0).await.is_empty(),
        "(1) fact rows removed (PostgreSQL)"
    );
    assert!(
        w.episode_rows(0).await.is_empty(),
        "(2) episode rows removed (PostgreSQL)"
    );
    assert_eq!(
        w.entities.list_all_entities(&w.t(0)).await.unwrap().len(),
        0,
        "(3) entity rows removed"
    );
    assert_eq!(
        w.fact_points(0).await.len(),
        fact_points,
        "(4) the fact points remain in Qdrant"
    );
    for k in 1..=3 {
        for mode in [
            RetrievalMode::Semantic,
            RetrievalMode::Hybrid,
            RetrievalMode::Fulltext,
        ] {
            assert!(
                w.search(0, &w.fact_text(0, k), mode).await.is_empty(),
                "(5) no fact searchable"
            );
        }
    }
    for k in 1..=3 {
        let eps = w
            .plain()
            .search_episodes(SearchEpisodesInput {
                tenant: w.t(0),
                query: format!("Episode {k} message {}", w.canary(0, 10 + k)),
                limit: 10,
            })
            .await
            .unwrap();
        assert!(eps.results.is_empty(), "(6) no episode searchable");
    }
    let after = w.context(0, &f1.content).await;
    assert!(
        !after.contains(&w.canary(0, 1)),
        "(7) the cached context (same key) does not expose forgotten content: {after}"
    );
    assert_eq!(
        captured.failures(),
        expected(&[("forget_user", 1)]),
        "R6 forget_user"
    );
    assert_eq!(w.others().await, x2, "(8) TB / TC unchanged");
    w.plan.reset();
    w.plain()
        .forget_user(ForgetUserInput { tenant: w.t(0) })
        .await
        .expect("(9) retry");
    assert!(
        w.states(0).await.is_empty()
            && w.fact_points(0).await.is_empty()
            && w.points(&w.episode_collection, 0).await.is_empty(),
        "(10) complete erasure"
    );
    assert_eq!(
        w.events.count_user_audit_content(&w.t(0)).await.unwrap(),
        0,
        "(10) audit content erased"
    );
    assert_eq!(w.others().await, x2, "X2");
}

/// Evidence for FU1 items (1)-(6) and (8)-(10) on real backends. Item (7), the cached context, is
/// asserted by the FU1 test above and is NOT asserted here: this test only records what the same
/// cache key returns, so the other items are not hidden behind that failure.
#[tokio::test]
async fn g3_fu1_evidence_erasure_order_fresh_reads_and_retry() {
    let w = world!("ar023t2_e2e", "fu1e");
    w.seed().await;
    let x2 = w.others().await;
    let f1 = w.fact_with(0, &w.canary(0, 1)).await;
    let cached = w.context(0, &f1.content).await;
    assert!(cached.contains(&w.canary(0, 1)));
    w.plan.arm("VectorStore.delete_by_user", 1, Mode::Before);
    assert!(
        w.plain()
            .forget_user(ForgetUserInput { tenant: w.t(0) })
            .await
            .is_err()
    );
    assert_eq!(w.plan.fired("VectorStore.delete_by_user"), vec![1], "X1");
    assert!(w.states(0).await.is_empty(), "(1)");
    assert!(w.episode_rows(0).await.is_empty(), "(2)");
    assert!(!w.fact_points(0).await.is_empty(), "(4) points remain");
    for k in 1..=3 {
        assert!(
            w.search(0, &w.fact_text(0, k), RetrievalMode::Hybrid)
                .await
                .is_empty(),
            "(5)"
        );
    }
    let same_key = w.context(0, &f1.content).await;
    let fresh_key = w.context(0, &format!("{} weekend", f1.content)).await;
    eprintln!(
        "G3 FU1 evidence: same-key cached context exposes forgotten text = {}; fresh-key context exposes = {}",
        same_key.contains(&w.canary(0, 1)),
        fresh_key.contains(&w.canary(0, 1))
    );
    assert!(
        !fresh_key.contains(&w.canary(0, 1)),
        "a fresh context key reads SQL and is clean"
    );
    assert_eq!(w.others().await, x2, "(8)");
    w.plan.reset();
    w.plain()
        .forget_user(ForgetUserInput { tenant: w.t(0) })
        .await
        .expect("(9) retry");
    assert!(
        w.states(0).await.is_empty()
            && w.fact_points(0).await.is_empty()
            && w.points(&w.episode_collection, 0).await.is_empty(),
        "(10)"
    );
    assert_eq!(
        w.events.count_user_audit_content(&w.t(0)).await.unwrap(),
        0,
        "(10) audit"
    );
    let after_retry = w.context(0, &f1.content).await;
    eprintln!(
        "G3 FU1 evidence: same-key context after the successful retry exposes = {}",
        after_retry.contains(&w.canary(0, 1))
    );
    assert!(
        !after_retry.contains(&w.canary(0, 1)),
        "the successful retry invalidates the cache"
    );
    assert_eq!(w.others().await, x2, "X2");
}

/// A retention whose vector cleanup fails after the
/// PostgreSQL prune must not leave the pruned facts servable from the SAME-key cached context.
#[tokio::test]
async fn g3_fuc5_failed_retention_cleanup_never_serves_pruned_facts_from_the_cache() {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "fuc5");
    w.seed().await;
    let x2 = w.others().await;
    let t = w.t(0);
    for k in 1..=3 {
        let mut f = w.fact_with(0, &w.canary(0, k)).await;
        f.updated_at = Utc::now() - chrono::Duration::days(400);
        w.facts.update_fact(&t, f).await.unwrap();
    }
    let q = w.fact_text(0, 1);
    assert!(w.context(0, &q).await.contains(&w.canary(0, 1)), "warmed");
    w.plan.arm("VectorStore.delete_by_fact_id", 2, Mode::Before);
    let result = w
        .plain()
        .apply_retention(memcore_core::retention::ApplyRetentionInput {
            tenant: t.clone(),
            policy: memcore_core::retention::RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(30),
                event_retention_days: None,
            },
            dry_run: false,
        })
        .await;
    assert_eq!(w.plan.fired("VectorStore.delete_by_fact_id"), vec![2], "X1");
    assert!(result.is_err(), "the cleanup failure surfaces");
    assert!(
        w.states(0).await.iter().all(|s| s.deleted),
        "F1-F3 pruned in PostgreSQL"
    );
    let leftover = w.fact_points(0).await.len();
    let same_key = w.context(0, &q).await;
    for k in 1..=3 {
        assert!(
            !same_key.contains(&w.canary(0, k)),
            "pruned fact {k} served from the SAME-key cached context"
        );
    }
    assert_eq!(captured.failures(), expected(&[("retention", 1)]));
    assert_eq!(w.others().await, x2, "X2");
    eprintln!(
        "G3 FUC5 (real backends): err; F1-F3 pruned; {leftover} leftover Qdrant points; same key clean"
    );
}

/// A replace import whose vector deletion fails after
/// the old PostgreSQL rows were deleted must not leave the old context servable under the same key.
#[tokio::test]
async fn g3_fuc6_failed_replace_never_serves_the_deleted_old_context() {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "fuc6");
    w.seed().await;
    let x2 = w.others().await;
    let t = w.t(0);
    let q = w.fact_text(0, 1);
    assert!(w.context(0, &q).await.contains(&w.canary(0, 1)), "warmed");
    let now = Utc::now();
    let new_facts = (1..=2)
        .map(|k| {
            Fact::new(
                Uuid::new_v4(),
                t.org_id.clone(),
                t.user_id.clone(),
                MemoryType::Skill,
                format!("Replacement {k} NEWTEXT prefers rowing on rivers {k}"),
                None,
                MemorySource::ApiImport,
                0.9,
                0.8,
                None,
                None,
                now,
                now,
                json!({}),
            )
            .unwrap()
        })
        .collect();
    let x = UserMemoryExport::new(&t.org_id, &t.user_id, new_facts, vec![]);
    w.plan.arm("VectorStore.delete_by_user", 1, Mode::Before);
    let result = import(&w, &x, ImportMode::Replace).await;
    assert_eq!(w.plan.fired("VectorStore.delete_by_user"), vec![1], "X1");
    assert!(result.is_err());
    assert!(
        w.states(0).await.is_empty(),
        "the old rows are deleted in PostgreSQL (partial replace)"
    );
    let leftover = w.fact_points(0).await.len();
    assert!(
        !w.context(0, &q).await.contains(&w.canary(0, 1)),
        "the deleted old fact served from the SAME-key cached context"
    );
    assert_eq!(captured.failures(), expected(&[("import", 1)]));
    w.plan.reset();
    let out = import(&w, &x, ImportMode::Replace)
        .await
        .expect("the documented recovery: replace again");
    assert_eq!(out.imported_facts, 2);
    w.consistent(0).await.expect("consistent after recovery");
    assert_eq!(w.others().await, x2, "X2");
    eprintln!(
        "G3 FUC6 (real backends): err; old rows deleted; {leftover} leftover Qdrant points; same key clean; recovery imported 2"
    );
}

#[tokio::test]
async fn g3_r6_keys_carry_only_the_fixed_path_label() {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let w = world!("ar023t2_e2e", "r6");
    w.seed().await;
    w.plan.arm("VectorStore.write_vector", 1, Mode::Before);
    let _ = ingest(
        &w,
        extracting(&format!("Fact 9 {} paints murals", w.canary(0, 9))),
        w.request(0, "r6"),
    )
    .await;
    assert_eq!(captured.failures(), expected(&[("fact_add", 1)]));
    for key in captured.keys() {
        assert!(
            !key.contains("CANARY") && !key.contains("org_r6") && !key.contains("u1"),
            "{key}"
        );
        if key.starts_with("memcore_cross_store_write_failures_total") {
            assert!(
                key.starts_with("memcore_cross_store_write_failures_total{path="),
                "{key}"
            );
            assert_eq!(key.matches('=').count(), 1, "one label only: {key}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// G4: R7 / C+ on PostgreSQL (engine level, three isolated databases)
// ---------------------------------------------------------------------------------------------

fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 9, minute, 0).unwrap()
}

fn src_fact(t: &TenantContext, id: Uuid, content: &str, minute: u32) -> Fact {
    Fact::new(
        id,
        t.org_id.clone(),
        t.user_id.clone(),
        MemoryType::Preference,
        content,
        None,
        MemorySource::ApiImport,
        0.9,
        0.7,
        None,
        None,
        at(minute),
        at(minute),
        json!({"label": "x"}),
    )
    .unwrap()
}

fn src_event(
    t: &TenantContext,
    fact: Option<Uuid>,
    op: MemoryEventOperation,
    minute: u32,
) -> MemoryEvent {
    MemoryEvent {
        id: Uuid::new_v4(),
        org_id: t.org_id.clone(),
        user_id: t.user_id.clone(),
        fact_id: fact,
        operation: op,
        input_text: None,
        previous_content: None,
        new_content: Some(format!("event at {minute}")),
        provider_name: None,
        model_name: None,
        metadata: json!({}),
        created_at: at(minute),
    }
}

fn fid(n: u128) -> Uuid {
    Uuid::from_u128(0x5b0c_6f1e_2a4d_4e8f_9c31_0000_0000_0000 | n)
}

/// Export X of TA: f superseded by g, h; restored events; entity layer with links / relation.
fn export_x(t: &TenantContext) -> UserMemoryExport {
    let mut f = src_fact(
        t,
        fid(0xf),
        "Avery leads the Tuesday trail running club.",
        10,
    );
    f.superseded_by = Some(fid(0xa));
    f.superseded_at = Some(at(12));
    let facts = vec![
        f,
        src_fact(
            t,
            fid(0xa),
            "Avery moved the trail running club to Thursdays.",
            11,
        ),
        src_fact(t, fid(0xb), "Avery prefers oat milk in coffee.", 13),
    ];
    let events = vec![
        src_event(t, Some(fid(0xf)), MemoryEventOperation::Add, 20),
        src_event(t, Some(fid(0xa)), MemoryEventOperation::Add, 21),
        src_event(t, None, MemoryEventOperation::NoOp, 23),
    ];
    let avery = Uuid::from_u128(0x9a00_0000_0000_4000_8000_0000_0000_0001);
    let club = Uuid::from_u128(0x9a00_0000_0000_4000_8000_0000_0000_0002);
    let ent = |id: Uuid, name: &str| Entity {
        id,
        org_id: t.org_id.clone(),
        user_id: t.user_id.clone(),
        canonical_name: name.into(),
        entity_type: EntityType::Person,
        aliases: vec![],
        summary: None,
        is_speaker: false,
        name_embedding: None,
        name_embedding_model: None,
        name_embedding_dimensions: None,
        created_at: at(1),
        updated_at: at(1),
    };
    let link = |fact: Uuid| FactEntityLink {
        org_id: t.org_id.clone(),
        user_id: t.user_id.clone(),
        fact_id: fact,
        entity_id: avery,
        role: EntityRole::Subject,
        mention: "Avery".into(),
        created_at: at(2),
    };
    UserMemoryExport::new(&t.org_id, &t.user_id, facts, events).with_entities(EntityExport {
        entities: vec![ent(avery, "Avery"), ent(club, "Trail Club")],
        links: vec![link(fid(0xf)), link(fid(0xa))],
        relations: vec![EntityRelation {
            id: Uuid::from_u128(0x9a00_0000_0000_4000_8000_0000_0000_00a1),
            org_id: t.org_id.clone(),
            user_id: t.user_id.clone(),
            source_entity_id: avery,
            relation: "leads".into(),
            target_entity_id: club,
            valid_at: None,
            invalid_at: None,
            source_fact_id: Some(fid(0xf)),
            created_at: at(2),
        }],
    })
}

async fn import(
    w: &World,
    x: &UserMemoryExport,
    mode: ImportMode,
) -> MemcoreResult<ImportUserDataOutput> {
    w.plain()
        .import_user_data(ImportUserDataInput {
            tenant: w.t(0),
            export: x.clone(),
            mode,
            restore_events: true,
            dry_run: false,
        })
        .await
}

/// TA's observable state on PostgreSQL + Qdrant, fact ids NEVER normalised.
async fn obs(w: &World) -> Value {
    let x = w
        .plain()
        .export_user_data(ExportUserDataInput {
            tenant: w.t(0),
            include_events: true,
            include_deleted: true,
        })
        .await
        .unwrap();
    let mut facts: Vec<Value> = x
        .facts
        .iter()
        .map(|f| json!([f.id, f.content, f.superseded_by]))
        .collect();
    facts.sort_by_key(|v| v.to_string());
    let mut events: Vec<Value> = x
        .memory_events
        .iter()
        .map(|e| json!([e.fact_id, format!("{:?}", e.operation), e.created_at]))
        .collect();
    events.sort_by_key(|v| v.to_string());
    let layer = x.entities.unwrap_or(EntityExport {
        entities: vec![],
        links: vec![],
        relations: vec![],
    });
    let mut links: Vec<Value> = layer.links.iter().map(|l| json!(l.fact_id)).collect();
    links.sort_by_key(|v| v.to_string());
    let relations: Vec<Value> = layer
        .relations
        .iter()
        .map(|r| json!(r.source_fact_id))
        .collect();
    // Point ids are memcore-minted random ids (amendment v1.1 §8): left out, and the points sorted
    // by their (never normalised) fact id and content.
    let mut points: Vec<Value> = w
        .fact_points(0)
        .await
        .into_iter()
        .map(|p| json!([p.1, p.2]))
        .collect();
    points.sort_by_key(|v| v.to_string());
    json!({"facts": facts, "events": events, "links": links, "relations": relations, "points": points})
}

#[derive(Clone, Copy, Debug)]
enum Foreign {
    Absent,
    Live,
    Deleted,
}

async fn plant_foreign(w: &World, foreign: Foreign, ids: &[Uuid]) {
    let b = w.t(1);
    w.facts
        .insert_fact(
            &b,
            src_fact(&b, Uuid::new_v4(), "TB unrelated canary fact.", 5),
        )
        .await
        .unwrap();
    for (k, id) in ids.iter().enumerate() {
        match foreign {
            Foreign::Absent => {}
            Foreign::Live | Foreign::Deleted => {
                w.facts
                    .insert_fact(&b, src_fact(&b, *id, &format!("TB private fact {k}."), 5))
                    .await
                    .unwrap();
                if matches!(foreign, Foreign::Deleted) {
                    w.facts.soft_delete_fact(&b, *id).await.unwrap();
                }
            }
        }
    }
}

#[tokio::test]
async fn g4_is1_is3_non_owned_ids_are_noninterfering_on_postgres() {
    let mut traces = Vec::new();
    for (db, foreign) in [
        ("ar023t2_w0", Foreign::Absent),
        ("ar023t2_w1", Foreign::Live),
        ("ar023t2_w2", Foreign::Deleted),
    ] {
        let w = world!(db, "is");
        let x = export_x(&w.t(0));
        let ids: Vec<Uuid> = x.facts.iter().map(|f| f.id).collect();
        plant_foreign(&w, foreign, &ids).await;
        let tb_before = w.snapshot(1).await;
        let first = import(&w, &x, ImportMode::Append).await;
        let out = first.as_ref().expect("200 in every world");
        assert_eq!(out.imported_facts, 3);
        let o = obs(&w).await;
        let m = |s: Uuid| derived_import_fact_id(&w.t(0), s);
        let stored: Vec<Uuid> = w.states(0).await.iter().map(|s| s.fact.id).collect();
        for s in &ids {
            assert!(
                stored.contains(&m(*s)),
                "{foreign:?}: stored under M(TA, f) on PostgreSQL"
            );
        }
        let f_row = w
            .facts
            .get_fact(&w.t(0), m(fid(0xf)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            f_row.superseded_by,
            Some(m(fid(0xa))),
            "superseded_by mapped"
        );
        assert!(
            o["links"]
                .as_array()
                .unwrap()
                .iter()
                .all(|l| *l == json!(m(fid(0xf))) || *l == json!(m(fid(0xa)))),
            "links mapped"
        );
        assert_eq!(o["relations"][0], json!(m(fid(0xf))), "relation mapped");
        assert!(
            o["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e[0] == json!(m(fid(0xf)))),
            "restored event mapped"
        );
        assert!(
            o["points"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| ids.iter().any(|s| p[0] == json!(m(*s).to_string()))),
            "Qdrant payload fact_id = stored id"
        );
        let second = import(&w, &x, ImportMode::Append).await.expect_err("409");
        assert!(
            second.is_import_already_applied(),
            "{foreign:?}: {second:?}"
        );
        assert_eq!(w.snapshot(1).await, tb_before, "TB unchanged");
        traces.push((
            format!("{foreign:?}"),
            json!([serde_json::to_value(out).unwrap(), o, format!("{second:?}")]),
        ));
    }
    for (label, trace) in &traces[1..] {
        assert_eq!(trace, &traces[0].1, "{label} differs from W0 on PostgreSQL");
    }
    eprintln!("G4 IS1-3: identical traces across W0 / W1 / W2 on PostgreSQL + Qdrant");
}

#[tokio::test]
async fn g4_owned_ids_duplicates_partial_retry_and_replace_on_postgres() {
    let w = world!("ar023t2_e2e", "own");
    let a = w.t(0);
    let x = export_x(&a);
    // IS7: duplicate source ids -> 400 DUPLICATE_FACT_ID, nothing written, in append and replace.
    w.facts
        .insert_fact(&a, src_fact(&a, Uuid::new_v4(), "TA prior fact kept.", 4))
        .await
        .unwrap();
    let before = w.snapshot(0).await;
    let mut dup = x.clone();
    let mut twin = dup.facts[0].clone();
    twin.content = "A second record reusing the first id.".into();
    dup.facts.push(twin);
    for mode in [ImportMode::Append, ImportMode::Replace] {
        let e = import(&w, &dup, mode).await.expect_err("400");
        assert!(
            matches!(&e, MemcoreError::ValidationError(m) if m.starts_with("DUPLICATE_FACT_ID")),
            "{e:?}"
        );
        assert_eq!(
            w.snapshot(0).await,
            before,
            "IS7 {mode:?}: nothing written (replace deleted nothing)"
        );
    }
    // IS9: partial append (upsert_vector#2/before), retry 409, replace restores X under t().
    w.plan.arm("VectorStore.upsert_vector", 2, Mode::Before);
    assert!(import(&w, &x, ImportMode::Append).await.is_err());
    assert_eq!(w.plan.fired("VectorStore.upsert_vector"), vec![2], "X1");
    w.plan.reset();
    let r = import(&w, &x, ImportMode::Append).await.expect_err("409");
    assert!(r.is_import_already_applied(), "IS9 retry: {r:?}");
    let out = import(&w, &x, ImportMode::Replace).await.expect("replace");
    assert_eq!(out.imported_facts, 3);
    let mut stored: Vec<Uuid> = w.states(0).await.iter().map(|s| s.fact.id).collect();
    stored.sort();
    let mut want: Vec<Uuid> = x
        .facts
        .iter()
        .map(|f| derived_import_fact_id(&a, f.id))
        .collect();
    want.sort();
    assert_eq!(stored, want, "IS9: exactly X under t()");
    w.consistent(0).await.expect("consistent after replace");
    // IS4 / IS6 / IS5 on owned ids (TA now owns M(TA, ·)): export TA and re-import.
    let owned = w
        .plain()
        .export_user_data(ExportUserDataInput {
            tenant: a.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    let e = import(&w, &owned, ImportMode::Append)
        .await
        .expect_err("IS4 409");
    assert!(e.is_import_already_applied());
    w.facts
        .soft_delete_fact(&a, owned.facts[0].id)
        .await
        .unwrap();
    let mut only_deleted = owned.clone();
    only_deleted.facts.truncate(1);
    let e = import(&w, &only_deleted, ImportMode::Append)
        .await
        .expect_err("IS6 409");
    assert!(e.is_import_id_conflict(), "{e:?}");
    let mut different = owned.clone();
    different.facts = vec![owned.facts[1].clone()];
    different.facts[0].content = "Different content under an owned id.".into();
    different.memory_events.clear();
    let out = import(&w, &different, ImportMode::Append)
        .await
        .expect("IS5 re-key 200");
    assert_eq!(out.imported_facts, 1);
    // IS14: replace keeps owned ids.
    let snapshot_ids: Vec<Uuid> = owned.facts.iter().map(|f| f.id).collect();
    let out = import(&w, &owned, ImportMode::Replace)
        .await
        .expect("IS14 replace");
    assert_eq!(out.imported_facts, 3);
    let mut now: Vec<Uuid> = w.states(0).await.iter().map(|s| s.fact.id).collect();
    now.sort();
    let mut kept = snapshot_ids.clone();
    kept.sort();
    assert_eq!(now, kept, "IS14: owned ids kept by replace on PostgreSQL");
}

#[tokio::test]
async fn g4_is18_concurrent_identical_imports_and_is19_legacy_collision_on_postgres() {
    // IS18: import 1 held at its first fact insert; import 2 completes; import 1 resumes.
    let w = world!("ar023t2_e2e", "con");
    let a = w.t(0);
    let x = export_x(&a);
    let (reached, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    w.plan
        .gate("FactStore.insert_fact", reached.clone(), release.clone());
    let w = Arc::new(w);
    let first = {
        let w = w.clone();
        let x = x.clone();
        tokio::spawn(async move { import(&w, &x, ImportMode::Append).await })
    };
    reached.notified().await;
    let second = import(&w, &x, ImportMode::Append).await;
    release.notify_one();
    let first = first.await.unwrap();
    eprintln!(
        "G4 IS18 on PostgreSQL: import 2 = {:?}; import 1 = {:?}",
        second.as_ref().map(|o| o.imported_facts),
        first.as_ref().err()
    );
    assert!(second.is_ok(), "import 2 completes");
    assert!(first.is_err(), "import 1 fails at the global key");
    let ids: Vec<Uuid> = w.states(0).await.iter().map(|s| s.fact.id).collect();
    assert_eq!(
        ids.len(),
        3,
        "one fact per M(TA, ·), no duplicate logical fact"
    );

    // IS19: TB holds a legacy row under M(TA, f); the signal is recorded, never PASS.
    let w2 = world!("ar023t2_e2e", "leg");
    let a2 = w2.t(0);
    let b2 = w2.t(1);
    let x2 = export_x(&a2);
    let legacy = derived_import_fact_id(&a2, fid(0xa));
    w2.facts
        .insert_fact(&b2, src_fact(&b2, legacy, "TB legacy imported row.", 5))
        .await
        .unwrap();
    let tb_before = w2.snapshot(1).await;
    let collided = import(&w2, &x2, ImportMode::Append).await;
    eprintln!(
        "G4 IS19 KNOWN RESIDUAL (PostgreSQL): {:?}; TA rows after = {}",
        collided.as_ref().err(),
        w2.states(0).await.len()
    );
    assert!(
        collided.is_err(),
        "the legacy row still collides on PostgreSQL"
    );
    assert_eq!(w2.snapshot(1).await, tb_before, "TB's legacy row unchanged");
    let text = format!("{:?}", collided.err().unwrap());
    assert!(
        !text.contains("TB legacy"),
        "no foreign content in the error"
    );
}
