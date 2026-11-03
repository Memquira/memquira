//! A committed deletion must not leave the previously cached context
//! of the deleted content servable under the SAME cache key, whatever later cleanup step fails.
//! Cases FUC1-FUC8.
//!
//! One engine per test (the context-cache coordinator belongs to the engine, as in the API), the
//! in-memory context cache enabled, the mocks wrapped by a fault plan that fails the n-th call of a
//! named operation BEFORE the inner call (no effect) or AFTER it (effect applied, error returned),
//! and can HOLD a call after its effect (deterministic interleaving). Every case asserts that the
//! fault fired, reads the stores directly and compares TB / TC snapshots. Invented data only.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    FactFulltextQuery, MemoryEventQuery, OrgMemoryEventQuery, OrgUserListQuery, OrgUserSummary,
    PointScope, RetentionPruneResult, ScoredFact, StoredFactState, VectorIdentityStatus,
    VectorPointState,
};
use memcore_core::retention::{ApplyRetentionInput, RetentionPolicy};
use memcore_core::verify::{
    VerifyMode, VerifyOptions, VerifyStores, confirmation_phrase, verify_vectors,
};
use memcore_core::{
    AddMemoryInput, BuildContextInput, CandidateFact, ContextCacheConfig, Episode,
    EpisodeComponents, EpisodeStore, ExportUserDataInput, Fact, FactSearchQuery, FactStore,
    ForgetUserInput, ImportMode, ImportUserDataInput, InMemoryContextCache, MemoryEngine,
    MemoryEvent, MemoryEventStore, MemoryMessage, MemoryType, MessageRole, SearchEpisodesInput,
    TenantContext, UserMemoryExport, VectorRecord, VectorSearchQuery, VectorSearchResult,
    VectorStore, VectorWriteIntent,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore};
use metrics::{
    Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use serde_json::json;
use tokio::sync::Notify;
use uuid::Uuid;

const DIMS: usize = 32;
const TENANTS: [(&str, &str); 3] = [("org_a", "u1"), ("org_a", "u2"), ("org_b", "u1")];

// ---------------------------------------------------------------------------------------------
// Metrics capture
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

fn assert_bounded(captured: &Captured) {
    for key in captured.keys() {
        assert!(
            !key.contains("CANARY") && !key.contains("org_a") && !key.contains("org_b"),
            "{key}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Fault plan
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// Fail before the inner call: no effect.
    Before,
    /// Call the inner store (effect applied), then return an error.
    After,
}

#[derive(Default)]
struct PlanState {
    calls: HashMap<&'static str, usize>,
    armed: HashMap<&'static str, (usize, Mode)>,
    fired: HashMap<&'static str, Vec<usize>>,
    hold: Option<(&'static str, Arc<Notify>, Arc<Notify>)>,
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
    /// Holds the next call of `op` AFTER its effect until `release` is notified.
    fn hold(&self, op: &'static str, reached: Arc<Notify>, release: Arc<Notify>) {
        self.0.lock().unwrap().hold = Some((op, reached, release));
    }
    fn before(&self, op: &'static str) -> MemcoreResult<Option<Mode>> {
        let mut s = self.0.lock().unwrap();
        let n = {
            let c = s.calls.entry(op).or_insert(0);
            *c += 1;
            *c
        };
        let fire = match s.armed.get(op) {
            Some((nth, mode)) if *nth == n => Some(*mode),
            _ => None,
        };
        if fire.is_some() {
            s.fired.entry(op).or_default().push(n);
        }
        if fire == Some(Mode::Before) {
            return Err(MemcoreError::Internal(format!(
                "injected {op} failure (before)"
            )));
        }
        Ok(fire)
    }
    async fn after<T>(
        &self,
        op: &'static str,
        fire: Option<Mode>,
        result: MemcoreResult<T>,
    ) -> MemcoreResult<T> {
        let hold = {
            let mut s = self.0.lock().unwrap();
            match &s.hold {
                Some((h, _, _)) if *h == op => s.hold.take().map(|(_, a, b)| (a, b)),
                _ => None,
            }
        };
        if let Some((reached, release)) = hold {
            reached.notify_one();
            release.notified().await;
        }
        if fire == Some(Mode::After) {
            result?;
            return Err(MemcoreError::Internal(format!(
                "injected {op} failure (after its effect)"
            )));
        }
        result
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
}

struct PFacts(Arc<MockFactStore>, Arc<Plan>);

#[async_trait]
impl FactStore for PFacts {
    async fn insert_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        let m = self.1.before("FactStore.insert_fact")?;
        let r = self.0.insert_fact(t, f).await;
        self.1.after("FactStore.insert_fact", m, r).await
    }
    async fn update_fact(&self, t: &TenantContext, f: Fact) -> MemcoreResult<Fact> {
        self.0.update_fact(t, f).await
    }
    async fn get_fact(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<Option<Fact>> {
        let m = self.1.before("FactStore.get_fact")?;
        let r = self.0.get_fact(t, id).await;
        self.1.after("FactStore.get_fact", m, r).await
    }
    async fn get_fact_including_deleted(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.0.get_fact_including_deleted(t, id).await
    }
    async fn list_fact_states(
        &self,
        t: &TenantContext,
        a: Option<Uuid>,
        l: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        self.0.list_fact_states(t, a, l).await
    }
    async fn fact_id_exists_in_any_tenant(&self, id: Uuid) -> MemcoreResult<bool> {
        self.0.fact_id_exists_in_any_tenant(id).await
    }
    async fn search_facts(&self, q: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        self.0.search_facts(q).await
    }
    async fn fulltext_search_facts(&self, q: FactFulltextQuery) -> MemcoreResult<Vec<ScoredFact>> {
        self.0.fulltext_search_facts(q).await
    }
    async fn soft_delete_fact(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        self.0.soft_delete_fact(t, id).await
    }
    async fn delete_user_data(&self, t: &TenantContext) -> MemcoreResult<()> {
        let m = self.1.before("FactStore.delete_user_data")?;
        let r = self.0.delete_user_data(t).await;
        self.1.after("FactStore.delete_user_data", m, r).await
    }
    async fn delete_facts_older_than(
        &self,
        t: &TenantContext,
        c: DateTime<Utc>,
        d: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        let m = self.1.before("FactStore.delete_facts_older_than")?;
        let r = self.0.delete_facts_older_than(t, c, d).await;
        self.1
            .after("FactStore.delete_facts_older_than", m, r)
            .await
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

struct PVectors(&'static str, Arc<MockVectorStore>, Arc<Plan>);

impl PVectors {
    fn op(&self, op: &'static str) -> &'static str {
        match (self.0, op) {
            ("facts", "delete_by_fact_id") => "VectorStore.delete_by_fact_id",
            ("facts", "delete_by_user") => "VectorStore.delete_by_user",
            ("facts", "upsert_vector") => "VectorStore.upsert_vector",
            ("episodes", "delete_by_fact_id") => "EpisodeVectors.delete_by_fact_id",
            ("episodes", "delete_by_user") => "EpisodeVectors.delete_by_user",
            ("episodes", "upsert_vector") => "EpisodeVectors.upsert_vector",
            _ => "other",
        }
    }
}

#[async_trait]
impl VectorStore for PVectors {
    async fn upsert_vector(&self, t: &TenantContext, r: VectorRecord) -> MemcoreResult<()> {
        let op = self.op("upsert_vector");
        let m = self.2.before(op)?;
        let res = self.1.upsert_vector(t, r).await;
        self.2.after(op, m, res).await
    }
    async fn write_vector(
        &self,
        t: &TenantContext,
        r: VectorRecord,
        i: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        self.1.write_vector(t, r, i).await
    }
    async fn search_vectors(&self, q: VectorSearchQuery) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.1.search_vectors(q).await
    }
    async fn delete_by_fact_id(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        let op = self.op("delete_by_fact_id");
        let m = self.2.before(op)?;
        let res = self.1.delete_by_fact_id(t, id).await;
        self.2.after(op, m, res).await
    }
    async fn delete_by_user(&self, t: &TenantContext) -> MemcoreResult<()> {
        let op = self.op("delete_by_user");
        let m = self.2.before(op)?;
        let res = self.1.delete_by_user(t).await;
        self.2.after(op, m, res).await
    }
    async fn fact_vector_points(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.1.fact_vector_points(t, id).await
    }
}

struct PEpisodes(Arc<MockEpisodeStore>, Arc<Plan>);

#[async_trait]
impl EpisodeStore for PEpisodes {
    async fn insert_episodes(
        &self,
        t: &TenantContext,
        e: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        self.0.insert_episodes(t, e).await
    }
    async fn insert_episodes_checked(
        &self,
        t: &TenantContext,
        e: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        self.0.insert_episodes_checked(t, e).await
    }
    async fn get_episodes(&self, t: &TenantContext, ids: &[Uuid]) -> MemcoreResult<Vec<Episode>> {
        self.0.get_episodes(t, ids).await
    }
    async fn mark_episodes_embedded(&self, t: &TenantContext, ids: &[Uuid]) -> MemcoreResult<()> {
        self.0.mark_episodes_embedded(t, ids).await
    }
    async fn list_episodes(&self, t: &TenantContext, l: usize) -> MemcoreResult<Vec<Episode>> {
        self.0.list_episodes(t, l).await
    }
    async fn list_episodes_after(
        &self,
        t: &TenantContext,
        a: Option<(DateTime<Utc>, Uuid)>,
        l: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        self.0.list_episodes_after(t, a, l).await
    }
    async fn delete_user_episodes(&self, t: &TenantContext) -> MemcoreResult<usize> {
        let m = self.1.before("EpisodeStore.delete_user_episodes")?;
        let r = self.0.delete_user_episodes(t).await;
        self.1
            .after("EpisodeStore.delete_user_episodes", m, r)
            .await
    }
    async fn delete_episodes_recorded_before(
        &self,
        t: &TenantContext,
        c: DateTime<Utc>,
        d: bool,
    ) -> MemcoreResult<Vec<Uuid>> {
        self.0.delete_episodes_recorded_before(t, c, d).await
    }
    async fn count_episodes_by_org(&self, o: &str) -> MemcoreResult<usize> {
        self.0.count_episodes_by_org(o).await
    }
    async fn episode_id_exists_in_any_tenant(&self, id: Uuid) -> MemcoreResult<bool> {
        self.0.episode_id_exists_in_any_tenant(id).await
    }
}

struct PEvents(Arc<MockMemoryEventStore>);

#[async_trait]
impl MemoryEventStore for PEvents {
    async fn record_event(&self, t: &TenantContext, e: MemoryEvent) -> MemcoreResult<MemoryEvent> {
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

// ---------------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------------

fn tenant(n: usize) -> TenantContext {
    TenantContext::new(TENANTS[n].0, TENANTS[n].1).unwrap()
}

fn canary(n: usize, k: usize) -> String {
    format!("CANARY-{}-{}-{k}", TENANTS[n].0, TENANTS[n].1)
}

fn fact_text(n: usize, k: usize) -> String {
    let topic = [
        "kayaking on mountain lakes",
        "baking sourdough bread",
        "restoring vintage radios",
        "growing chilli peppers",
    ];
    format!("Fact {k} {} prefers {}", canary(n, k), topic[k - 1])
}

struct Fx {
    plan: Arc<Plan>,
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    engine: Arc<MemoryEngine>,
}

fn engine_for(
    plan: &Arc<Plan>,
    facts: &Arc<MockFactStore>,
    vectors: &Arc<MockVectorStore>,
    episodes: &Arc<MockEpisodeStore>,
    episode_vectors: &Arc<MockVectorStore>,
    events: &Arc<MockMemoryEventStore>,
    llm: MockLlmProvider,
) -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(PFacts(facts.clone(), plan.clone())),
        Arc::new(PVectors("facts", vectors.clone(), plan.clone())),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(DIMS)),
    )
    .with_event_store(Arc::new(PEvents(events.clone())))
    .with_min_importance(0.0)
    .with_episodes(Some(EpisodeComponents {
        store: Arc::new(PEpisodes(episodes.clone(), plan.clone())),
        vectors: Arc::new(PVectors("episodes", episode_vectors.clone(), plan.clone())),
    }))
    .with_context_cache(
        Arc::new(InMemoryContextCache::new(1000)),
        ContextCacheConfig {
            enabled: true,
            ..ContextCacheConfig::default()
        },
    )
}

async fn fx() -> Fx {
    let plan = Arc::new(Plan::default());
    let facts = Arc::new(MockFactStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let episodes = Arc::new(MockEpisodeStore::new());
    let episode_vectors = Arc::new(MockVectorStore::new());
    let events = Arc::new(MockMemoryEventStore::new());
    for n in 0..3 {
        let candidates = (1..=4)
            .map(|k| {
                CandidateFact::new(
                    fact_text(n, k),
                    MemoryType::Skill,
                    0.9,
                    0.8,
                    None,
                    json!({}),
                )
                .unwrap()
            })
            .collect();
        let mut messages: Vec<MemoryMessage> = (1..=3)
            .map(|k| MemoryMessage {
                role: MessageRole::User,
                content: format!(
                    "Episode {k} message {} about weekend plans",
                    canary(n, 10 + k)
                ),
                message_id: Some(format!("e{k}")),
                ..Default::default()
            })
            .collect();
        messages.push(MemoryMessage {
            role: MessageRole::User,
            content: "short message 4".into(),
            message_id: Some("e4".into()),
            ..Default::default()
        });
        engine_for(
            &plan,
            &facts,
            &vectors,
            &episodes,
            &episode_vectors,
            &events,
            MockLlmProvider::new().with_extraction_candidates(candidates),
        )
        .add_memory(AddMemoryInput {
            tenant: tenant(n),
            messages,
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .unwrap();
    }
    let engine = Arc::new(engine_for(
        &plan,
        &facts,
        &vectors,
        &episodes,
        &episode_vectors,
        &events,
        MockLlmProvider::new().with_extraction_candidates(vec![]),
    ));
    plan.reset();
    Fx {
        plan,
        facts,
        vectors,
        episodes,
        episode_vectors,
        events,
        engine,
    }
}

impl Fx {
    async fn rows(&self, n: usize) -> Vec<StoredFactState> {
        self.facts
            .list_fact_states(&tenant(n), None, 1000)
            .await
            .unwrap()
    }
    async fn fact(&self, n: usize, k: usize) -> Fact {
        let marker = canary(n, k);
        self.rows(n)
            .await
            .into_iter()
            .map(|s| s.fact)
            .find(|f| f.content.contains(&marker))
            .unwrap()
    }
    async fn points(&self, store: &MockVectorStore, n: usize) -> usize {
        store
            .scroll_points(&PointScope::Tenant(tenant(n)), None, 10_000)
            .await
            .unwrap()
            .points
            .len()
    }
    async fn snapshot(&self, n: usize) -> String {
        let t = tenant(n);
        let mut rows: Vec<String> = self
            .rows(n)
            .await
            .iter()
            .map(|s| format!("{}|{}|{}", s.fact.id, s.fact.content, s.deleted))
            .collect();
        rows.sort();
        let mut eps: Vec<String> = self
            .episodes
            .list_episodes(&t, 1000)
            .await
            .unwrap()
            .iter()
            .map(|e| format!("{}|{}|{}", e.id, e.content, e.embedded))
            .collect();
        eps.sort();
        let events = self
            .events
            .list_events(MemoryEventQuery::new(t, 200))
            .await
            .unwrap()
            .len();
        format!(
            "{rows:?}\n{}|{}\n{eps:?}\nevents={events}",
            self.points(&self.vectors, n).await,
            self.points(&self.episode_vectors, n).await
        )
    }
    async fn others(&self) -> (String, String) {
        (self.snapshot(1).await, self.snapshot(2).await)
    }
    /// The context for `query` through the engine's cache (the SAME key each time for one query).
    async fn context(&self, n: usize, query: &str) -> (String, bool) {
        let out = self
            .engine
            .build_context(BuildContextInput {
                tenant: tenant(n),
                query: query.to_string(),
                ..BuildContextInput::default()
            })
            .await
            .unwrap();
        (out.context, out.cache.hit)
    }
    /// Warms the context of `query` and proves it is served from the cache.
    async fn warm(&self, n: usize, query: &str, must_contain: &str) {
        let (first, _) = self.context(n, query).await;
        assert!(
            first.contains(must_contain),
            "the warmed context contains {must_contain}"
        );
        let (second, hit) = self.context(n, query).await;
        assert!(hit, "the second build is a cache hit (same key)");
        assert_eq!(second, first);
    }
}

async fn twice<F, Fut>(case: F) -> String
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let a = case().await;
    let b = case().await;
    assert_eq!(a, b, "two independent fixtures give the same summary");
    a
}

// ---------------------------------------------------------------------------------------------
// FUC1 / FUC1b / FUC2 / FUC3 / FUC4: forget-user
// ---------------------------------------------------------------------------------------------

async fn forget_case(op: &'static str, mode: Mode) -> String {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let q = fact_text(0, 1);
    fx.warm(0, &q, &canary(0, 1)).await;
    fx.plan.arm(op, 1, mode);
    let result = fx
        .engine
        .forget_user(ForgetUserInput { tenant: tenant(0) })
        .await;
    assert_eq!(fx.plan.fired(op), vec![1], "X1 {op} {mode:?}");
    assert!(result.is_err(), "{op}: the failure surfaces as an error");
    let rows_left = fx.rows(0).await.len();
    let fact_points = fx.points(&fx.vectors, 0).await;
    let episode_rows = fx
        .episodes
        .list_episodes(&tenant(0), 100)
        .await
        .unwrap()
        .len();
    let (same_key, hit) = fx.context(0, &q).await;
    let exposes = same_key.contains(&canary(0, 1));
    let committed = rows_left == 0;
    if committed {
        assert!(
            !exposes,
            "{op} {mode:?}: SQL rows deleted, yet the SAME-key cached context still serves them"
        );
    } else {
        assert!(
            exposes,
            "{op} {mode:?}: nothing deleted, the fact is still (correctly) served"
        );
    }
    let episodes_searchable = !fx
        .engine
        .search_episodes(SearchEpisodesInput {
            tenant: tenant(0),
            query: format!("Episode 1 message {}", canary(0, 11)),
            limit: 10,
        })
        .await
        .unwrap()
        .results
        .is_empty();
    assert_eq!(fx.others().await, x2, "X2");
    // The retry completes the erasure; the same key stays clean.
    fx.plan.reset();
    fx.engine
        .forget_user(ForgetUserInput { tenant: tenant(0) })
        .await
        .expect("retry");
    assert!(
        fx.rows(0).await.is_empty()
            && fx.points(&fx.vectors, 0).await == 0
            && fx.points(&fx.episode_vectors, 0).await == 0
    );
    assert_eq!(
        fx.events
            .count_user_audit_content(&tenant(0))
            .await
            .unwrap(),
        0,
        "audit content erased"
    );
    let (after, _) = fx.context(0, &q).await;
    assert!(!after.contains(&canary(0, 1)), "clean after the retry");
    assert_eq!(fx.others().await, x2, "X2 after retry");
    assert_bounded(&captured);
    format!(
        "{op} {mode:?}: err; fact_rows_left={rows_left} fact_points={fact_points} episode_rows={episode_rows} episodes_searchable={episodes_searchable} same_key_hit={hit} exposes={exposes}; failures={:?}",
        captured.failures()
    )
}

#[tokio::test]
async fn fuc1_failed_fact_vector_delete_never_serves_the_cached_context() {
    eprintln!(
        "{}",
        twice(|| forget_case("VectorStore.delete_by_user", Mode::Before)).await
    );
}

#[tokio::test]
async fn fuc1b_a_first_deletion_that_commits_then_errors_never_serves_the_cached_context() {
    eprintln!(
        "{}",
        twice(|| forget_case("FactStore.delete_user_data", Mode::After)).await
    );
}

#[tokio::test]
async fn fuc2_failed_episode_row_or_vector_delete_never_serves_cached_or_episode_content() {
    eprintln!(
        "{}",
        twice(|| forget_case("EpisodeStore.delete_user_episodes", Mode::Before)).await
    );
    eprintln!(
        "{}",
        twice(|| forget_case("EpisodeVectors.delete_by_user", Mode::Before)).await
    );
}

#[tokio::test]
async fn fuc4_a_failure_before_any_mutation_changes_nothing() {
    eprintln!(
        "{}",
        twice(|| forget_case("FactStore.delete_user_data", Mode::Before)).await
    );
}

#[tokio::test]
async fn fuc3_successful_forget_removes_rows_vectors_and_the_cached_context() {
    let fx = fx().await;
    let x2 = fx.others().await;
    let q = fact_text(0, 1);
    fx.warm(0, &q, &canary(0, 1)).await;
    fx.engine
        .forget_user(ForgetUserInput { tenant: tenant(0) })
        .await
        .unwrap();
    assert!(fx.rows(0).await.is_empty());
    assert_eq!(fx.points(&fx.vectors, 0).await, 0);
    assert_eq!(fx.points(&fx.episode_vectors, 0).await, 0);
    assert!(
        !fx.context(0, &q).await.0.contains(&canary(0, 1)),
        "same key clean"
    );
    assert_eq!(fx.others().await, x2, "X2");
}

// ---------------------------------------------------------------------------------------------
// FUC5: retention
// ---------------------------------------------------------------------------------------------

async fn retention_case() -> String {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let t = tenant(0);
    // F1-F3 older than the cutoff; F4 stays recent (an authorised, retained fact).
    for k in 1..=3 {
        let mut f = fx.fact(0, k).await;
        f.updated_at = Utc::now() - Duration::days(400);
        fx.facts.update_fact(&t, f).await.unwrap();
    }
    let q = fact_text(0, 1);
    fx.warm(0, &q, &canary(0, 1)).await;
    fx.warm(0, &fact_text(0, 4), &canary(0, 4)).await;
    fx.plan
        .arm("VectorStore.delete_by_fact_id", 2, Mode::Before);
    let result = fx
        .engine
        .apply_retention(ApplyRetentionInput {
            tenant: t.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(30),
                event_retention_days: None,
            },
            dry_run: false,
        })
        .await;
    assert_eq!(
        fx.plan.fired("VectorStore.delete_by_fact_id"),
        vec![2],
        "X1"
    );
    assert!(result.is_err(), "the cleanup failure surfaces");
    let live: Vec<String> = fx
        .rows(0)
        .await
        .into_iter()
        .filter(|s| !s.deleted)
        .map(|s| s.fact.content)
        .collect();
    assert_eq!(
        live,
        vec![fact_text(0, 4)],
        "F1-F3 pruned in SQL, F4 retained"
    );
    let (same_key, _) = fx.context(0, &q).await;
    for k in 1..=3 {
        assert!(
            !same_key.contains(&canary(0, k)),
            "FUC5: pruned fact {k} served from the SAME-key cached context"
        );
    }
    let (f4_context, _) = fx.context(0, &fact_text(0, 4)).await;
    assert!(
        f4_context.contains(&canary(0, 4)),
        "the retained fact is still served"
    );
    let leftover = fx.points(&fx.vectors, 0).await;
    assert_eq!(
        captured.failures(),
        expected(&[("retention", 1)]),
        "R6 retention"
    );
    // Accepted contract (RT1): the retry has nothing left to prune; verify-vectors removes the
    // leftover points of the pruned rows.
    fx.plan.reset();
    let retry = fx
        .engine
        .apply_retention(ApplyRetentionInput {
            tenant: t.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(30),
                event_retention_days: None,
            },
            dry_run: false,
        })
        .await
        .expect("retry");
    let embedding = MockEmbeddingProvider::new(DIMS);
    let mut options = VerifyOptions::dry_run();
    options.mode = VerifyMode::Repair {
        confirmation: confirmation_phrase(&t),
    };
    let report = verify_vectors(
        &VerifyStores {
            facts: fx.facts.as_ref(),
            episodes: Some(fx.episodes.as_ref()),
            fact_vectors: Some(fx.vectors.as_ref()),
            episode_vectors: Some(fx.episode_vectors.as_ref()),
            embedding: Some(&embedding),
            fact_identity: VectorIdentityStatus::Verified,
            episode_identity: VectorIdentityStatus::Verified,
        },
        &t,
        &options,
    )
    .await;
    assert_eq!(report.remaining_inconsistent, 0);
    assert_eq!(
        fx.points(&fx.vectors, 0).await,
        1,
        "only F4's point remains"
    );
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded(&captured);
    format!(
        "FUC5: err; F1-F3 pruned, F4 kept; leftover points {leftover}; same key clean; retry deleted {}; repair remaining 0; failures={:?}",
        retry.facts_deleted,
        captured.failures()
    )
}

#[tokio::test]
async fn fuc5_failed_retention_cleanup_never_serves_pruned_facts_from_the_cache() {
    eprintln!("{}", twice(retention_case).await);
}

// ---------------------------------------------------------------------------------------------
// FUC6: replace-mode import
// ---------------------------------------------------------------------------------------------

fn replacement(t: &TenantContext) -> UserMemoryExport {
    let now = Utc::now();
    let facts = (1..=2)
        .map(|k| {
            Fact::new(
                Uuid::new_v4(),
                t.org_id.clone(),
                t.user_id.clone(),
                MemoryType::Skill,
                format!("Replacement {k} NEWTEXT prefers rowing on rivers {k}"),
                None,
                memcore_core::MemorySource::ApiImport,
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
    UserMemoryExport::new(&t.org_id, &t.user_id, facts, vec![])
}

async fn replace_case() -> String {
    let captured = Captured::default();
    let recorder = Rec(captured.clone());
    let _g = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let t = tenant(0);
    let q = fact_text(0, 1);
    fx.warm(0, &q, &canary(0, 1)).await;
    let x = replacement(&t);
    // Negative: a rejected replace (DUPLICATE_FACT_ID) and a dry run write nothing, the cache keeps
    // serving the (still existing) old fact.
    let mut dup = x.clone();
    dup.facts.push(dup.facts[0].clone());
    let before = fx.snapshot(0).await;
    assert!(
        fx.engine
            .import_user_data(ImportUserDataInput {
                tenant: t.clone(),
                export: dup,
                mode: ImportMode::Replace,
                restore_events: false,
                dry_run: false
            })
            .await
            .is_err()
    );
    fx.engine
        .import_user_data(ImportUserDataInput {
            tenant: t.clone(),
            export: x.clone(),
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: true,
        })
        .await
        .unwrap();
    assert_eq!(
        fx.snapshot(0).await,
        before,
        "rejected / dry-run replace: no write"
    );
    assert!(
        fx.context(0, &q).await.0.contains(&canary(0, 1)),
        "nothing deleted: the old fact is still (correctly) served"
    );
    // The replace deletes the old rows, then its fact-vector deletion fails.
    fx.plan.arm("VectorStore.delete_by_user", 1, Mode::Before);
    let result = fx
        .engine
        .import_user_data(ImportUserDataInput {
            tenant: t.clone(),
            export: x.clone(),
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await;
    assert_eq!(fx.plan.fired("VectorStore.delete_by_user"), vec![1], "X1");
    assert!(result.is_err());
    assert!(
        fx.rows(0).await.is_empty(),
        "the old rows are deleted (partial replace)"
    );
    let leftover = fx.points(&fx.vectors, 0).await;
    let (same_key, _) = fx.context(0, &q).await;
    assert!(
        !same_key.contains(&canary(0, 1)),
        "FUC6: the deleted old fact served from the SAME-key cached context"
    );
    assert_eq!(captured.failures(), expected(&[("import", 1)]), "R6 import");
    // Documented recovery: the same replace again.
    fx.plan.reset();
    let out = fx
        .engine
        .import_user_data(ImportUserDataInput {
            tenant: t.clone(),
            export: x.clone(),
            mode: ImportMode::Replace,
            restore_events: false,
            dry_run: false,
        })
        .await
        .expect("replace recovers");
    assert_eq!(out.imported_facts, 2);
    let contents: Vec<String> = fx
        .rows(0)
        .await
        .into_iter()
        .map(|s| s.fact.content)
        .collect();
    assert!(contents.iter().all(|c| c.contains("NEWTEXT")) && contents.len() == 2);
    assert!(
        !fx.context(0, &q).await.0.contains(&canary(0, 1)),
        "clean after recovery"
    );
    let exported = fx
        .engine
        .export_user_data(ExportUserDataInput {
            tenant: t.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(exported.facts.len(), 2);
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded(&captured);
    format!(
        "FUC6: rejected/dry-run no write; replace err with old rows deleted, leftover points {leftover}; same key clean; recovery imported 2; failures={:?}",
        captured.failures()
    )
}

#[tokio::test]
async fn fuc6_failed_replace_never_serves_the_deleted_old_context() {
    eprintln!("{}", twice(replace_case).await);
}

// ---------------------------------------------------------------------------------------------
// FUC8: an in-flight context computation must not repopulate the cache after the invalidation
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn fuc8_an_in_flight_computation_cannot_repopulate_a_forgotten_users_cache() {
    let fx = fx().await;
    let q = fact_text(0, 1);
    // The computation is held right AFTER it has read F1's SQL row (its per-hit fetch).
    let (reached, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    fx.plan
        .hold("FactStore.get_fact", reached.clone(), release.clone());
    let engine = fx.engine.clone();
    let query = q.clone();
    let inflight = tokio::spawn(async move {
        engine
            .build_context(BuildContextInput {
                tenant: tenant(0),
                query,
                ..BuildContextInput::default()
            })
            .await
    });
    reached.notified().await;
    // Meanwhile the user is forgotten (successfully) and the cache invalidated.
    fx.engine
        .forget_user(ForgetUserInput { tenant: tenant(0) })
        .await
        .unwrap();
    assert!(fx.rows(0).await.is_empty());
    release.notify_one();
    let old = inflight.await.unwrap().unwrap();
    eprintln!(
        "FUC8: the in-flight request itself returned old content = {} (it read SQL before the deletion)",
        old.context.contains(&canary(0, 1))
    );
    let (after, hit) = fx.context(0, &q).await;
    eprintln!(
        "FUC8: same-key build after the forget: hit={hit} exposes={}",
        after.contains(&canary(0, 1))
    );
    assert!(
        !after.contains(&canary(0, 1)),
        "FUC8: an obsolete entry inserted after the invalidation is served"
    );
}
