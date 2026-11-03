//! `verify-vectors` (R5) and its repair counters
//! (R6) on the in-process mocks.
//!
//! Case ids:
//! V1-V9, RT1, RT2, E4. Every fault is injected by a wrapper that logs whether it fired (X1);
//! other tenants are compared before and after every tenant-A case (X2); reports and metric keys
//! are scanned for every tenant's canaries (X3).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    FactFulltextQuery, OrgUserListQuery, OrgUserSummary, PointPage, PointScope,
    RetentionPruneResult, ScoredFact, StoredFactState, VectorIdentityStatus, VectorPointState,
};
use memcore_core::retention::{ApplyRetentionInput, RetentionPolicy};
use memcore_core::verify::{
    CATEGORIES, COLLECTIONS, ITEM_RESULTS, VERIFY_ITEMS_METRIC, VERIFY_SCANS_METRIC, VerifyOptions,
    VerifyReport, VerifyStores, confirmation_phrase, verify_vectors,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingProvider, Episode, EpisodeComponents, EpisodeStore,
    Fact, FactSearchQuery, FactStore, MemoryEngine, MemoryMessage, MemoryType, MessageRole,
    TenantContext, VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore,
    VectorWriteIntent,
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
// Metrics capture
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<BTreeMap<String, u64>>>);

fn key_string(key: &Key) -> String {
    let mut labels: Vec<String> = key
        .labels()
        .map(|label| format!("{}={}", label.key(), label.value()))
        .collect();
    labels.sort();
    format!("{}{{{}}}", key.name(), labels.join(","))
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

struct TestRecorder(Captured);

impl Recorder for TestRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(Arc::new(Handle(key_string(key), self.0.clone())))
    }
    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        self.0.0.lock().unwrap().entry(key_string(key)).or_default();
        Gauge::noop()
    }
    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        self.0.0.lock().unwrap().entry(key_string(key)).or_default();
        Histogram::noop()
    }
}

// ---------------------------------------------------------------------------------------------
// Fault plan (X1)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Fail before the inner call.
    Error,
    /// Run the inner call, then report an error (the effect is applied).
    After,
    /// Scroll only, from call n on: a cursor that never advances.
    Stuck,
    /// Scroll only: return the first point and no next page (a page-limit bug).
    Truncate,
    /// `get_fact` only: a concurrent writer changes the row just before this read.
    ConcurrentEdit,
}

/// Per operation: calls, fail at call n (0 = off), times fired, mode.
type OpState = (usize, usize, usize, Option<Mode>);

#[derive(Default)]
struct Faults {
    ops: Mutex<HashMap<&'static str, OpState>>,
}

impl Faults {
    fn arm(&self, op: &'static str, nth: usize, mode: Mode) {
        self.ops.lock().unwrap().insert(op, (0, nth, 0, Some(mode)));
    }
    fn reset(&self) {
        self.ops.lock().unwrap().clear();
    }
    fn hit(&self, op: &'static str) -> Option<Mode> {
        let mut ops = self.ops.lock().unwrap();
        let entry = ops.entry(op).or_insert((0, 0, 0, None));
        entry.0 += 1;
        let sticky = entry.3 == Some(Mode::Stuck) && entry.1 != 0 && entry.0 > entry.1;
        if (entry.1 != 0 && entry.0 == entry.1) || sticky {
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
    fn writes(&self) -> usize {
        [
            "write_vector",
            "upsert_vector",
            "delete_points",
            "delete_by_fact_id",
            "delete_by_user",
        ]
        .iter()
        .map(|op| self.calls(op))
        .sum()
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
        self.inner.insert_fact(tenant, fact).await
    }
    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.inner.update_fact(tenant, fact).await
    }
    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
        match self.faults.hit("get_fact") {
            Some(Mode::Error) => return Err(injected("get_fact")),
            Some(Mode::ConcurrentEdit) => {
                if let Some(mut fact) = self.inner.get_fact(tenant, fact_id).await? {
                    fact.content = format!("{} (edited concurrently)", fact.content);
                    self.inner.update_fact(tenant, fact).await?;
                }
            }
            _ => {}
        }
        self.inner.get_fact(tenant, fact_id).await
    }
    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        if self.faults.hit("get_fact_including_deleted") == Some(Mode::Error) {
            return Err(injected("get_fact_including_deleted"));
        }
        self.inner.get_fact_including_deleted(tenant, fact_id).await
    }
    async fn list_fact_states(
        &self,
        tenant: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        if self.faults.hit("list_fact_states") == Some(Mode::Error) {
            return Err(injected("list_fact_states"));
        }
        self.inner.list_fact_states(tenant, after, limit).await
    }
    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool> {
        if self.faults.hit("fact_id_exists_in_any_tenant") == Some(Mode::Error) {
            return Err(injected("fact_id_exists_in_any_tenant"));
        }
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

/// Wraps one collection; op names are prefixed with `name` (`facts.` / `episodes.`).
struct FaultVectorStore {
    inner: Arc<MockVectorStore>,
    faults: Arc<Faults>,
    episodes: bool,
}

impl FaultVectorStore {
    fn op(&self, base: &'static str) -> &'static str {
        match (self.episodes, base) {
            (false, op) => op,
            (true, "write_vector") => "ep.write_vector",
            (true, "upsert_vector") => "ep.upsert_vector",
            (true, "delete_points") => "ep.delete_points",
            (true, "delete_by_fact_id") => "ep.delete_by_fact_id",
            (true, "scroll_points") => "ep.scroll_points",
            (true, "count_points") => "ep.count_points",
            (true, op) => op,
        }
    }
}

#[async_trait]
impl VectorStore for FaultVectorStore {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        match self.faults.hit(self.op("upsert_vector")) {
            Some(Mode::Error) => return Err(injected("upsert_vector")),
            Some(Mode::After) => {
                self.inner.upsert_vector(tenant, record).await?;
                return Err(injected("upsert_vector"));
            }
            _ => {}
        }
        self.inner.upsert_vector(tenant, record).await
    }

    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        let fault = self.faults.hit(self.op("write_vector"));
        if fault == Some(Mode::Error) {
            return Err(injected("write_vector"));
        }
        // The trait-default semantics (as the mock store uses them), through the inner store.
        if intent == VectorWriteIntent::Replace {
            match self.inner.delete_by_fact_id(tenant, record.fact_id).await {
                Ok(()) | Err(MemcoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.inner.upsert_vector(tenant, record).await?;
        if fault == Some(Mode::After) {
            return Err(injected("write_vector"));
        }
        Ok(())
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(query).await
    }

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        if self.faults.hit(self.op("delete_by_fact_id")) == Some(Mode::Error) {
            return Err(injected("delete_by_fact_id"));
        }
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.faults.hit("delete_by_user");
        self.inner.delete_by_user(tenant).await
    }

    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.inner.fact_vector_points(tenant, fact_id).await
    }

    async fn scroll_points(
        &self,
        scope: &PointScope,
        cursor: Option<String>,
        limit: usize,
    ) -> MemcoreResult<PointPage> {
        match self.faults.hit(self.op("scroll_points")) {
            Some(Mode::Error) => return Err(injected("scroll_points")),
            Some(Mode::Stuck) => {
                let mut page = self
                    .inner
                    .scroll_points(scope, cursor.clone(), limit)
                    .await?;
                let first = page.points.first().map(|p| p.point_id.clone());
                page.next_cursor = cursor.or(first).or_else(|| Some("0".to_string()));
                return Ok(page);
            }
            Some(Mode::Truncate) => {
                let mut page = self.inner.scroll_points(scope, cursor, limit).await?;
                page.points.truncate(1);
                page.next_cursor = None;
                return Ok(page);
            }
            _ => {}
        }
        self.inner.scroll_points(scope, cursor, limit).await
    }

    async fn count_points(&self, scope: &PointScope) -> MemcoreResult<u64> {
        if self.faults.hit(self.op("count_points")) == Some(Mode::Error) {
            return Err(injected("count_points"));
        }
        self.inner.count_points(scope).await
    }

    async fn delete_points(
        &self,
        tenant: &TenantContext,
        point_ids: &[String],
    ) -> MemcoreResult<()> {
        if self.faults.hit(self.op("delete_points")) == Some(Mode::Error) {
            return Err(injected("delete_points"));
        }
        self.inner.delete_points(tenant, point_ids).await
    }
}

struct FaultEmbedding {
    inner: MockEmbeddingProvider,
    faults: Arc<Faults>,
    always_fail: AtomicBool,
}

#[async_trait]
impl EmbeddingProvider for FaultEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let fault = self.faults.hit("embed_text");
        if self.always_fail.load(Ordering::SeqCst) || fault == Some(Mode::Error) {
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
// Standard fixture FX and divergence fixture DV
// ---------------------------------------------------------------------------------------------

fn tenant(index: usize) -> TenantContext {
    TenantContext::new(TENANTS[index].0, TENANTS[index].1).expect("tenant")
}

fn canary(index: usize, n: usize) -> String {
    format!("CANARY-{}-{}-{n}", TENANTS[index].0, TENANTS[index].1)
}

fn all_canaries() -> Vec<String> {
    (0..TENANTS.len())
        .flat_map(|t| (1..=30).map(move |n| canary(t, n)))
        .collect()
}

fn fact_text(index: usize, n: usize) -> String {
    let topic = [
        "kayaking on mountain lakes",
        "baking sourdough bread",
        "restoring vintage radios",
        "growing tomatoes on the balcony",
    ];
    format!("Fact {n} {} prefers {}", canary(index, n), topic[n - 1])
}

struct Fx {
    faults: Arc<Faults>,
    facts: Arc<FaultFactStore>,
    raw_facts: Arc<MockFactStore>,
    vectors: Arc<FaultVectorStore>,
    raw_vectors: Arc<MockVectorStore>,
    episode_store: Arc<MockEpisodeStore>,
    episode_vectors: Arc<FaultVectorStore>,
    raw_episode_vectors: Arc<MockVectorStore>,
    embedding: Arc<FaultEmbedding>,
}

impl Fx {
    fn engine(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            Arc::new(llm),
            self.embedding.clone(),
        )
        .with_episodes(Some(EpisodeComponents {
            store: self.episode_store.clone(),
            vectors: self.episode_vectors.clone(),
        }))
    }

    fn stores(&self, identity: VectorIdentityStatus) -> VerifyStores<'_> {
        VerifyStores {
            facts: self.facts.as_ref(),
            episodes: Some(self.episode_store.as_ref()),
            fact_vectors: Some(self.vectors.as_ref()),
            episode_vectors: Some(self.episode_vectors.as_ref()),
            embedding: Some(self.embedding.as_ref()),
            fact_identity: identity,
            episode_identity: identity,
        }
    }

    async fn dry(&self, index: usize) -> VerifyReport {
        verify_vectors(
            &self.stores(VectorIdentityStatus::Verified),
            &tenant(index),
            &VerifyOptions::dry_run(),
        )
        .await
    }

    async fn repair(&self, index: usize) -> VerifyReport {
        self.repair_with(index, VectorIdentityStatus::Verified)
            .await
    }

    async fn repair_with(&self, index: usize, identity: VectorIdentityStatus) -> VerifyReport {
        let options = VerifyOptions::repair(confirmation_phrase(&tenant(index)));
        verify_vectors(&self.stores(identity), &tenant(index), &options).await
    }

    async fn fact_n(&self, index: usize, n: usize) -> Fact {
        let marker = canary(index, n);
        let mut query = FactSearchQuery::new(tenant(index), 100);
        query.include_deleted = true;
        self.raw_facts
            .search_facts(query)
            .await
            .unwrap()
            .into_iter()
            .find(|fact| fact.content.contains(&format!("{marker} ")))
            .expect("fixture fact")
    }

    async fn fact_points(&self, index: usize, id: Uuid) -> Vec<VectorPointState> {
        self.raw_vectors
            .fact_vector_points(&tenant(index), id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn episode_points(&self, index: usize, id: Uuid) -> Vec<VectorPointState> {
        self.raw_episode_vectors
            .fact_vector_points(&tenant(index), id)
            .await
            .unwrap()
            .unwrap()
    }

    /// Fact rows (id, text, deleted), fact and episode points (point id, entity, text), episode
    /// rows (id, text, embedded) of one tenant.
    async fn snapshot(&self, index: usize) -> String {
        let t = tenant(index);
        let mut rows: Vec<String> = self
            .raw_facts
            .list_fact_states(&t, None, 10_000)
            .await
            .unwrap()
            .iter()
            .map(|s| format!("{}|{}|{}", s.fact.id, s.fact.content, s.deleted))
            .collect();
        rows.sort();
        let mut points = Vec::new();
        for store in [&self.raw_vectors, &self.raw_episode_vectors] {
            let page = store
                .scroll_points(&PointScope::Tenant(t.clone()), None, 10_000)
                .await
                .unwrap();
            for p in page.points {
                points.push(format!("{}|{:?}|{:?}", p.point_id, p.entity_id, p.content));
            }
        }
        points.sort();
        let mut episodes: Vec<String> = self
            .episode_store
            .list_episodes(&t, 10_000)
            .await
            .unwrap()
            .iter()
            .map(|e| format!("{}|{}|{}", e.id, e.content, e.embedded))
            .collect();
        episodes.sort();
        format!("{rows:?}\n{points:?}\n{episodes:?}")
    }

    async fn others(&self, index: usize) -> Vec<String> {
        let mut out = Vec::new();
        for other in 0..TENANTS.len() {
            if other != index {
                out.push(self.snapshot(other).await);
            }
        }
        out
    }

    async fn global_points(&self) -> u64 {
        let mut n = 0;
        for store in [&self.raw_vectors, &self.raw_episode_vectors] {
            n += store.count_points(&PointScope::Unattributed).await.unwrap();
        }
        n
    }

    async fn point(&self, episodes: bool, index: usize, fact_id: Uuid, content: &str) -> String {
        let id = Uuid::new_v4();
        let store = if episodes {
            &self.raw_episode_vectors
        } else {
            &self.raw_vectors
        };
        store
            .upsert_vector(
                &tenant(index),
                VectorRecord {
                    id,
                    fact_id,
                    org_id: TENANTS[index].0.to_string(),
                    user_id: TENANTS[index].1.to_string(),
                    embedding: deterministic_embedding(content, DIMS).unwrap(),
                    content: content.to_string(),
                    memory_type: if episodes {
                        MemoryType::Conversation
                    } else {
                        MemoryType::Skill
                    },
                    metadata: json!({}),
                },
            )
            .await
            .unwrap();
        id.to_string()
    }
}

async fn fx() -> Fx {
    let faults = Arc::new(Faults::default());
    let raw_facts = Arc::new(MockFactStore::new());
    let raw_vectors = Arc::new(MockVectorStore::new());
    let raw_episode_vectors = Arc::new(MockVectorStore::new());
    let fx = Fx {
        facts: Arc::new(FaultFactStore {
            inner: raw_facts.clone(),
            faults: faults.clone(),
        }),
        vectors: Arc::new(FaultVectorStore {
            inner: raw_vectors.clone(),
            faults: faults.clone(),
            episodes: false,
        }),
        episode_vectors: Arc::new(FaultVectorStore {
            inner: raw_episode_vectors.clone(),
            faults: faults.clone(),
            episodes: true,
        }),
        embedding: Arc::new(FaultEmbedding {
            inner: MockEmbeddingProvider::new(DIMS),
            faults: faults.clone(),
            always_fail: AtomicBool::new(false),
        }),
        faults,
        raw_facts,
        raw_vectors,
        episode_store: Arc::new(MockEpisodeStore::new()),
        raw_episode_vectors,
    };
    for index in 0..TENANTS.len() {
        let candidates = (1..=4)
            .map(|n| {
                CandidateFact::new(
                    fact_text(index, n),
                    MemoryType::Skill,
                    0.9,
                    0.8,
                    None,
                    json!({ "label": format!("F{n}") }),
                )
                .unwrap()
            })
            .collect();
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
            content: "short message 4".to_string(), // Stored, not embedded
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
            .unwrap();
        assert_eq!(out.added, 4);
    }
    fx.faults.reset();
    fx
}

/// Ids of the divergence fixture DV of one tenant.
#[allow(dead_code)]
struct Dv {
    missing: Uuid,
    stale: Uuid,
    stale_old_text: String,
    soft_deleted: Uuid,
    duplicated: Uuid,
    duplicate_points: Vec<String>,
    pending_episode: Uuid,
    short_episode: Uuid,
    fact_orphan_point: String,
    episode_orphan_point: String,
    unattributable_point: String,
    conflict_point: String,
}

/// Protocol §5 group V, divergence fixture DV: one of each category for `index`; the attribution
/// conflict names `other`'s live F1.
async fn dv(fx: &Fx, index: usize, other: usize) -> Dv {
    let t = tenant(index);
    let f1 = fx.fact_n(index, 1).await;
    fx.raw_vectors.delete_by_fact_id(&t, f1.id).await.unwrap(); // missing point
    let f2 = fx.fact_n(index, 2).await;
    let mut updated = f2.clone();
    updated.content = format!("Fact 2 {} now prefers rye bread", canary(index, 2));
    fx.raw_facts.update_fact(&t, updated).await.unwrap(); // stale point
    let f3 = fx.fact_n(index, 3).await;
    fx.raw_facts.soft_delete_fact(&t, f3.id).await.unwrap(); // soft-deleted orphan
    let f4 = fx.fact_n(index, 4).await;
    let extra = fx.point(false, index, f4.id, &f4.content).await; // 2 points
    let mut duplicate_points: Vec<String> = fx
        .fact_points(index, f4.id)
        .await
        .into_iter()
        .map(|p| p.point_id)
        .collect();
    duplicate_points.sort();
    assert!(duplicate_points.contains(&extra));
    // An embeddable episode with embedded = 0 and no point.
    let pending = Episode {
        id: Uuid::new_v4(),
        org_id: t.org_id.clone(),
        user_id: t.user_id.clone(),
        session_id: None,
        message_id: Some("p1".to_string()),
        speaker: None,
        role: MessageRole::User,
        content: format!(
            "Pending episode {} that was never embedded",
            canary(index, 20)
        ),
        observed_at: None,
        recorded_at: Utc::now(),
        embedded: false,
        metadata: json!({}),
    };
    fx.episode_store
        .insert_episodes(&t, vec![pending.clone()])
        .await
        .unwrap();
    let short = fx
        .episode_store
        .list_episodes(&t, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.content == "short message 4")
        .unwrap();
    let fact_orphan_point = fx
        .point(
            false,
            index,
            Uuid::new_v4(),
            &format!("Orphan fact text {}", canary(index, 21)),
        )
        .await;
    let episode_orphan_point = fx
        .point(
            true,
            index,
            Uuid::new_v4(),
            &format!("Orphan episode text {}", canary(index, 22)),
        )
        .await;
    // A point without tenant payload.
    let unattributable = Uuid::new_v4();
    fx.raw_vectors.insert_unchecked_for_test(VectorRecord {
        id: unattributable,
        fact_id: Uuid::new_v4(),
        org_id: String::new(),
        user_id: String::new(),
        embedding: deterministic_embedding("no tenant", DIMS).unwrap(),
        content: format!("No tenant payload {}", canary(index, 23)),
        memory_type: MemoryType::Skill,
        metadata: json!({}),
    });
    // A point saying `index` whose fact id is `other`'s live row.
    let foreign = fx.fact_n(other, 1).await;
    let conflict = Uuid::new_v4();
    fx.raw_vectors.insert_unchecked_for_test(VectorRecord {
        id: conflict,
        fact_id: foreign.id,
        org_id: t.org_id.clone(),
        user_id: t.user_id.clone(),
        embedding: deterministic_embedding(&foreign.content, DIMS).unwrap(),
        content: foreign.content.clone(),
        memory_type: MemoryType::Skill,
        metadata: json!({}),
    });
    fx.faults.reset();
    Dv {
        missing: f1.id,
        stale: f2.id,
        stale_old_text: f2.content,
        soft_deleted: f3.id,
        duplicated: f4.id,
        duplicate_points,
        pending_episode: pending.id,
        short_episode: short.id,
        fact_orphan_point,
        episode_orphan_point,
        unattributable_point: unattributable.to_string(),
        conflict_point: conflict.to_string(),
    }
}

fn cat(map: &BTreeMap<&'static str, u64>, category: &str) -> u64 {
    map.get(category).copied().unwrap_or(0)
}

fn json_of(report: &VerifyReport) -> String {
    serde_json::to_string(report).unwrap()
}

fn assert_no_canary(text: &str) {
    for c in all_canaries() {
        assert!(!text.contains(&c), "a canary leaked into the report");
    }
}

// ---------------------------------------------------------------------------------------------
// Group V
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn v1_dry_run_classifies_each_category_once_and_writes_nothing() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    let x2 = fx.others(0).await;
    let before = fx.snapshot(0).await;
    let report = fx.dry(0).await;

    assert_eq!(report.mode, "dry_run");
    assert!(report.scan.complete, "{:?}", report.scan.errors);
    for category in [
        "missing_vector",
        "stale_content",
        "duplicate_points",
        "soft_deleted_orphan",
        "hard_deleted_orphan",
    ] {
        assert_eq!(cat(&report.facts, category), 1, "facts.{category}");
    }
    assert_eq!(cat(&report.facts, "verification_error"), 0);
    assert_eq!(cat(&report.episodes, "episode_pending"), 1);
    assert_eq!(cat(&report.episodes, "hard_deleted_orphan"), 1);
    assert_eq!(
        cat(&report.episodes, "episode_short_exempt"),
        1,
        "short episode reported as exempt"
    );
    assert_eq!(cat(&report.episodes, "consistent"), 3);
    for category in [
        "missing_vector",
        "stale_content",
        "duplicate_points",
        "verification_error",
    ] {
        assert_eq!(cat(&report.episodes, category), 0, "episodes.{category}");
    }
    assert_eq!(cat(&report.global, "unattributable_point"), 1);
    assert_eq!(cat(&report.global, "tenant_attribution_conflict"), 1);
    // The ambiguous items are in the global section only.
    for item in &report.items {
        if item.point_ids.contains(&dv.unattributable_point)
            || item.point_ids.contains(&dv.conflict_point)
        {
            assert_eq!(item.collection, "global");
            assert_eq!(item.action, "report_only");
        }
        assert_ne!(
            item.entity_id,
            Some(dv.short_episode.to_string()),
            "short episode not pending"
        );
    }
    assert_eq!(report.outcome, "repair_needed");
    assert_eq!(report.exit_code, 1);
    assert_eq!(report.remaining_inconsistent, 7);
    // No write of any kind, no embedding call.
    assert_eq!(fx.faults.writes(), 0);
    assert_eq!(
        fx.faults.calls("ep.write_vector") + fx.faults.calls("ep.delete_points"),
        0
    );
    assert_eq!(fx.faults.calls("embed_text"), 0);
    assert_eq!(fx.snapshot(0).await, before);
    assert_eq!(fx.others(0).await, x2, "X2");
    assert_no_canary(&json_of(&report));
}

#[tokio::test]
async fn v2_v3_repair_fixes_the_tenant_never_touches_ambiguous_points_and_is_idempotent() {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    let x2 = fx.others(0).await;
    let global_before = fx.global_points().await;
    let report = {
        let _guard = metrics::set_default_local_recorder(&recorder);
        fx.repair(0).await
    };

    assert_eq!(report.mode, "repair");
    let after = report.after.as_ref().expect("re-verification");
    assert!(after.scan.complete);
    assert_eq!(after.remaining_inconsistent, 0, "{:?}", report.items);
    assert_eq!(report.repairs.failed, 0);
    assert_eq!(report.repairs.skipped_unsafe, 0);
    assert_eq!(report.repairs.succeeded, 7);
    // Facts: missing -> 1 point; stale -> overwritten; orphans -> gone; 2 points -> the smallest.
    let missing = fx.fact_n(0, 1).await;
    assert!(
        matches!(fx.fact_points(0, missing.id).await.as_slice(), [p] if p.content == missing.content)
    );
    let stale = fx
        .raw_facts
        .get_fact(&tenant(0), dv.stale)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(fx.fact_points(0, dv.stale).await.as_slice(), [p] if p.content == stale.content)
    );
    assert!(fx.fact_points(0, dv.soft_deleted).await.is_empty());
    let kept = fx.fact_points(0, dv.duplicated).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].point_id, dv.duplicate_points[0], "smallest-id rule");
    // Episode: embedded with a point under its own id, flag set.
    let episode = fx
        .episode_store
        .get_episodes(&tenant(0), &[dv.pending_episode])
        .await
        .unwrap();
    assert!(episode[0].embedded);
    let points = fx.episode_points(0, dv.pending_episode).await;
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].point_id, dv.pending_episode.to_string());
    // Never touched: the short episode, the unattributable and conflict points, other tenants.
    assert!(fx.episode_points(0, dv.short_episode).await.is_empty());
    let short = fx
        .episode_store
        .get_episodes(&tenant(0), &[dv.short_episode])
        .await
        .unwrap();
    assert!(!short[0].embedded);
    assert_eq!(fx.global_points().await, global_before);
    let ta_points = fx
        .raw_vectors
        .scroll_points(&PointScope::Tenant(tenant(0)), None, 1000)
        .await
        .unwrap();
    assert!(
        ta_points
            .points
            .iter()
            .any(|p| p.point_id == dv.conflict_point),
        "conflict point kept"
    );
    assert!(
        !ta_points
            .points
            .iter()
            .any(|p| p.point_id == dv.fact_orphan_point)
    );
    assert!(
        fx.raw_episode_vectors
            .scroll_points(&PointScope::Tenant(tenant(0)), None, 1000)
            .await
            .unwrap()
            .points
            .iter()
            .all(|p| p.point_id != dv.episode_orphan_point)
    );
    assert_eq!(fx.others(0).await, x2, "X2");
    // Only items not attributable to the tenant remain.
    assert_eq!(report.outcome, "attention_required");
    assert_eq!(report.exit_code, 0);
    assert_no_canary(&json_of(&report));

    // R6: exact counter values, fixed labels.
    let counters = captured.0.lock().unwrap().clone();
    let get = |c: &str, cat: &str, r: &str| {
        counters
            .get(&format!(
                "{VERIFY_ITEMS_METRIC}{{category={cat},collection={c},result={r}}}"
            ))
            .copied()
            .unwrap_or(0)
    };
    for category in [
        "missing_vector",
        "stale_content",
        "duplicate_points",
        "soft_deleted_orphan",
        "hard_deleted_orphan",
    ] {
        assert_eq!(get("facts", category, "detected"), 1, "{category}");
        assert_eq!(get("facts", category, "repaired"), 1, "{category}");
    }
    assert_eq!(get("episodes", "episode_pending", "repaired"), 1);
    assert_eq!(get("episodes", "hard_deleted_orphan", "repaired"), 1);
    assert_eq!(get("global", "unattributable_point", "detected"), 1);
    assert_eq!(get("global", "tenant_attribution_conflict", "detected"), 1);
    assert_eq!(
        counters.get(&format!(
            "{VERIFY_SCANS_METRIC}{{mode=repair,result=complete}}"
        )),
        Some(&1)
    );
    for key in counters
        .keys()
        .filter(|k| k.starts_with("memcore_vector_verify"))
    {
        let ok = COLLECTIONS.iter().any(|c| {
            CATEGORIES.iter().any(|cat| {
                ITEM_RESULTS.iter().any(|r| {
                    *key == format!(
                        "{VERIFY_ITEMS_METRIC}{{category={cat},collection={c},result={r}}}"
                    )
                })
            })
        }) || key.starts_with(VERIFY_SCANS_METRIC);
        assert!(ok, "unexpected series {key}");
        assert_no_canary(key);
    }

    // V3: again -> nothing to do, no write call.
    fx.faults.reset();
    let again = fx.repair(0).await;
    assert_eq!(
        fx.faults.writes()
            + fx.faults.calls("ep.write_vector")
            + fx.faults.calls("ep.delete_points"),
        0
    );
    assert_eq!(fx.faults.calls("embed_text"), 0);
    assert_eq!(again.remaining_inconsistent, 0);
    assert_eq!(again.repairs.attempted, 0);
    let divergent: u64 = again
        .facts
        .iter()
        .chain(again.episodes.iter())
        .filter(|(k, _)| **k != "consistent" && **k != "episode_short_exempt")
        .map(|(_, v)| *v)
        .sum();
    assert_eq!(divergent, 0, "all tenant counts 0");
    assert_eq!(again.exit_code, 0);
}

#[tokio::test]
async fn v4_interrupted_repair_is_reported_and_a_rerun_completes_without_duplicates() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    fx.faults.arm("write_vector", 2, Mode::After);
    let first = fx.repair(0).await;
    assert_eq!(fx.faults.fired("write_vector"), 1, "X1");
    assert_ne!(first.exit_code, 0);
    assert_eq!(first.outcome, "repair_incomplete");
    assert_eq!(first.repairs.failed, 1);
    assert_eq!(
        first.items.iter().filter(|i| i.action == "failed").count(),
        1,
        "the failed item is listed"
    );
    fx.faults.reset();
    let second = fx.repair(0).await;
    assert_eq!(second.after.as_ref().unwrap().remaining_inconsistent, 0);
    assert_eq!(second.exit_code, 0);
    for id in [dv.missing, dv.stale, dv.duplicated] {
        assert_eq!(fx.fact_points(0, id).await.len(), 1, "no duplicate points");
    }
}

#[tokio::test]
async fn v5_permanent_embedding_failure_removes_the_stale_point_and_reports_failure() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    fx.embedding.always_fail.store(true, Ordering::SeqCst);
    let report = fx.repair(0).await;
    assert!(fx.faults.calls("embed_text") >= 3, "fired on every attempt");
    assert!(report.repairs.failed > 0);
    assert_ne!(report.exit_code, 0);
    let stale_item = report
        .items
        .iter()
        .find(|i| i.entity_id == Some(dv.stale.to_string()))
        .unwrap();
    assert_eq!(stale_item.action, "failed_stale_removed");
    assert!(
        fx.fact_points(0, dv.stale).await.is_empty(),
        "privacy first: the stale point is deleted"
    );
    let after = report.after.as_ref().unwrap();
    assert_eq!(
        cat(&after.facts, "missing_vector"),
        2,
        "the stale fact is now reported missing (with F1)"
    );
    // No obsolete text remains anywhere in the tenant's points.
    let points = fx
        .raw_vectors
        .scroll_points(&PointScope::Tenant(tenant(0)), None, 1000)
        .await
        .unwrap();
    assert!(
        points
            .points
            .iter()
            .all(|p| p.content.as_deref() != Some(dv.stale_old_text.as_str()))
    );
    // Categories that need no embedding were repaired.
    assert!(fx.fact_points(0, dv.soft_deleted).await.is_empty());
    assert_eq!(fx.fact_points(0, dv.duplicated).await.len(), 1);
    assert_eq!(cat(&after.facts, "hard_deleted_orphan"), 0);
}

#[tokio::test]
async fn v6_repair_without_the_exact_confirmation_is_refused_and_writes_nothing() {
    let fx = fx().await;
    dv(&fx, 0, 1).await;
    let before = fx.snapshot(0).await;
    for phrase in [
        "",
        "verify-vectors:org_a:u2",
        "verify-vectors:org_b:u1",
        "forget-user:org_a:u1",
        "verify-vectors:all",
    ] {
        let report = verify_vectors(
            &fx.stores(VectorIdentityStatus::Verified),
            &tenant(0),
            &VerifyOptions::repair(phrase),
        )
        .await;
        assert_eq!(report.outcome, "refused", "{phrase}");
        assert_eq!(report.exit_code, 2);
    }
    assert_eq!(fx.faults.writes(), 0);
    assert_eq!(
        fx.faults.calls("scroll_points") + fx.faults.calls("list_fact_states"),
        0,
        "nothing read"
    );
    assert_eq!(fx.snapshot(0).await, before);
}

#[tokio::test]
async fn v7_unverified_identity_refuses_re_embedding_but_removes_attributable_orphans() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    let report = fx.repair_with(0, VectorIdentityStatus::Unrecorded).await;
    assert_eq!(report.identity.facts, VectorIdentityStatus::Unrecorded);
    assert_eq!(
        fx.faults.calls("write_vector") + fx.faults.calls("ep.write_vector"),
        0,
        "no vector written"
    );
    assert_eq!(fx.faults.calls("embed_text"), 0);
    let skipped: Vec<_> = report
        .items
        .iter()
        .filter(|i| i.action == "skipped_identity_unverified")
        .collect();
    assert_eq!(skipped.len(), 3, "missing, stale and pending episode");
    assert!(
        fx.fact_points(0, dv.soft_deleted).await.is_empty(),
        "attributable orphan deleted"
    );
    assert_eq!(report.outcome, "repair_incomplete");
    assert_eq!(report.exit_code, 3);
    let json = json_of(&report);
    assert!(
        json.contains("\"unrecorded\""),
        "identity_unverified reported"
    );
}

#[tokio::test]
async fn v8_another_tenants_divergence_is_untouched_and_absent_from_the_report() {
    let fx = fx().await;
    dv(&fx, 0, 1).await;
    dv(&fx, 1, 2).await;
    let tb_before = fx.snapshot(1).await;
    let tb_ids: Vec<String> = fx
        .raw_facts
        .list_fact_states(&tenant(1), None, 1000)
        .await
        .unwrap()
        .iter()
        .map(|s| s.fact.id.to_string())
        .collect();
    for report in [fx.dry(0).await, fx.repair(0).await] {
        let json = json_of(&report);
        assert!(!json.contains("\"u2\""), "TB not in the report");
        for id in &tb_ids {
            assert!(!json.contains(id), "no TB fact id in the report");
        }
        assert_no_canary(&json);
    }
    assert_eq!(fx.snapshot(1).await, tb_before, "X2");
}

#[tokio::test]
async fn v9_identical_live_facts_are_reported_never_merged() {
    let fx = fx().await;
    let f1 = fx.fact_n(0, 1).await;
    let mut twin = f1.clone();
    twin.id = Uuid::new_v4();
    fx.raw_facts
        .insert_fact(&tenant(0), twin.clone())
        .await
        .unwrap();
    fx.point(false, 0, twin.id, &twin.content).await;
    let report = fx.dry(0).await;
    assert_eq!(report.duplicate_content, 1);
    assert_eq!(report.outcome, "clean", "informational only");
    fx.repair(0).await;
    assert!(
        fx.raw_facts
            .get_fact(&tenant(0), f1.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fx.raw_facts
            .get_fact(&tenant(0), twin.id)
            .await
            .unwrap()
            .is_some(),
        "never merged"
    );
}

// ---------------------------------------------------------------------------------------------
// Group D: retention residues (RT1, RT2)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn rt1_retention_cleanup_failure_is_completed_by_the_repair() {
    let fx = fx().await;
    let t = tenant(0);
    let old = Utc::now() - Duration::days(400);
    let mut ids = Vec::new();
    for n in 1..=3 {
        let mut fact = fx.fact_n(0, n).await;
        fact.updated_at = old;
        fx.raw_facts.update_fact(&t, fact.clone()).await.unwrap();
        ids.push(fact.id);
    }
    let x2 = fx.others(0).await;
    fx.faults.arm("delete_by_fact_id", 2, Mode::Error);
    let err = fx
        .engine(MockLlmProvider::new())
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
    assert!(err.is_err());
    assert_eq!(fx.faults.fired("delete_by_fact_id"), 1, "X1");
    let mut remaining = 0;
    for id in &ids {
        assert!(
            fx.raw_facts
                .get_fact_including_deleted(&t, *id)
                .await
                .unwrap()
                .unwrap()
                .deleted
        );
        remaining += fx.fact_points(0, *id).await.len();
    }
    assert_eq!(remaining, 2, "one point gone, two remain");
    fx.faults.reset();
    let report = fx.repair(0).await;
    assert_eq!(cat(&report.facts, "soft_deleted_orphan"), 2);
    assert_eq!(report.after.as_ref().unwrap().remaining_inconsistent, 0);
    for id in &ids {
        assert!(fx.fact_points(0, *id).await.is_empty());
    }
    assert_eq!(report.outcome, "repair_completed");
    assert_eq!(fx.others(0).await, x2, "X2");
}

#[tokio::test]
async fn rt2_hard_deleted_episode_orphans_are_found_by_scroll_and_removed() {
    let fx = fx().await;
    let t = tenant(0);
    let old = Utc::now() - Duration::days(400);
    let mut ids = Vec::new();
    for n in 1..=3 {
        let episode = Episode {
            id: Uuid::new_v4(),
            org_id: t.org_id.clone(),
            user_id: t.user_id.clone(),
            session_id: Some("old".to_string()),
            message_id: Some(format!("old{n}")),
            speaker: None,
            role: MessageRole::User,
            content: format!("An old episode number {n} {}", canary(0, 24 + n)),
            observed_at: None,
            recorded_at: old,
            embedded: true,
            metadata: json!({}),
        };
        fx.episode_store
            .insert_episodes(&t, vec![episode.clone()])
            .await
            .unwrap();
        fx.raw_episode_vectors
            .upsert_vector(
                &t,
                VectorRecord {
                    id: episode.id,
                    fact_id: episode.id,
                    org_id: t.org_id.clone(),
                    user_id: t.user_id.clone(),
                    embedding: deterministic_embedding(&episode.content, DIMS).unwrap(),
                    content: episode.content.clone(),
                    memory_type: MemoryType::Conversation,
                    metadata: json!({ "kind": "episode" }),
                },
            )
            .await
            .unwrap();
        ids.push(episode.id);
    }
    let x2 = fx.others(0).await;
    fx.faults.arm("ep.delete_by_fact_id", 1, Mode::Error);
    let result = fx
        .engine(MockLlmProvider::new())
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
    assert!(result.is_err());
    assert_eq!(fx.faults.fired("ep.delete_by_fact_id"), 1, "X1");
    assert!(
        fx.episode_store
            .get_episodes(&t, &ids)
            .await
            .unwrap()
            .is_empty(),
        "rows hard-deleted"
    );
    fx.faults.reset();
    let dry = fx.dry(0).await;
    assert_eq!(
        cat(&dry.episodes, "hard_deleted_orphan"),
        3,
        "found by the tenant scroll"
    );
    let report = fx.repair(0).await;
    assert_eq!(report.after.as_ref().unwrap().remaining_inconsistent, 0);
    for id in &ids {
        assert!(fx.episode_points(0, *id).await.is_empty());
    }
    assert_eq!(fx.others(0).await, x2, "X2");
}

// ---------------------------------------------------------------------------------------------
// E4 and focused safeguards
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn e4_the_report_contains_ids_categories_and_hashes_only() {
    let fx = fx().await;
    dv(&fx, 0, 1).await;
    for report in [fx.dry(0).await, fx.repair(0).await] {
        let json = json_of(&report);
        assert_no_canary(&json);
        for item in &report.items {
            if let Some(hash) = &item.content_sha256 {
                assert_eq!(hash.len(), 16);
                assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
            }
        }
        assert!(!json.contains("prefers"), "no fact text");
        assert!(!json.contains("weekend plans"), "no episode text");
    }
}

#[tokio::test]
async fn a_consistent_tenant_is_clean_and_untouched() {
    let fx = fx().await;
    let before = fx.snapshot(2).await;
    let report = fx.dry(2).await;
    assert!(report.scan.complete);
    assert_eq!(report.outcome, "clean");
    assert_eq!(report.exit_code, 0);
    assert_eq!(cat(&report.facts, "consistent"), 4);
    assert_eq!(cat(&report.episodes, "consistent"), 3);
    assert_eq!(cat(&report.episodes, "episode_short_exempt"), 1);
    let repaired = fx.repair(2).await;
    assert_eq!(repaired.repairs.attempted, 0);
    assert_eq!(repaired.outcome, "repair_completed");
    assert_eq!(fx.faults.writes(), 0);
    assert_eq!(fx.snapshot(2).await, before);
}

#[tokio::test]
async fn a_stuck_scroll_cursor_is_an_incomplete_scan_never_clean() {
    let fx = fx().await;
    // Calls 1-4 list each fact's points; call 5 is the tenant scroll of the fact collection.
    fx.faults.arm("scroll_points", 5, Mode::Stuck);
    let report = fx.dry(2).await;
    assert!(
        fx.faults.fired("scroll_points") >= 2,
        "X1: the cursor never advanced"
    );
    assert!(!report.scan.complete);
    assert_eq!(report.outcome, "blocked");
    assert_eq!(report.exit_code, 4);
}

#[tokio::test]
async fn a_truncated_scroll_page_is_an_incomplete_scan_never_clean() {
    for nth in [1, 5] {
        let fx = fx().await;
        // 1: the first fact's own point listing; 5: the tenant scroll. Every fact gets a second
        // point, so whichever fact is listed first loses one to the truncation.
        for n in 1..=4 {
            let fact = fx.fact_n(2, n).await;
            fx.point(false, 2, fact.id, &fact.content).await;
        }
        fx.faults.reset();
        fx.faults.arm("scroll_points", nth, Mode::Truncate);
        let report = fx.dry(2).await;
        assert_eq!(fx.faults.fired("scroll_points"), 1, "X1 call {nth}");
        assert!(!report.scan.complete, "call {nth}: {:?}", report.facts);
        assert_eq!(report.outcome, "blocked");
        assert_eq!(report.exit_code, 4);
    }
}

#[tokio::test]
async fn a_failing_tenant_scroll_or_count_blocks_the_scan() {
    for op in ["scroll_points", "count_points", "ep.scroll_points"] {
        let fx = fx().await;
        fx.faults.arm(op, 1, Mode::Error);
        let report = fx.dry(2).await;
        assert_eq!(fx.faults.fired(op), 1, "X1 {op}");
        assert_eq!(report.outcome, "blocked", "{op}");
        assert_eq!(report.exit_code, 4);
    }
}

#[tokio::test]
async fn a_sql_read_failure_blocks_and_triggers_no_guessed_repair() {
    let fx = fx().await;
    dv(&fx, 0, 1).await;
    let before = fx.snapshot(0).await;
    fx.faults.arm("list_fact_states", 1, Mode::Error);
    let report = fx.repair(0).await;
    assert_eq!(fx.faults.fired("list_fact_states"), 1, "X1");
    assert_eq!(report.outcome, "blocked");
    // After the failure the run only classifies: no write in either collection, no embedding.
    assert_eq!(
        fx.faults.writes()
            + fx.faults.calls("ep.write_vector")
            + fx.faults.calls("ep.delete_points"),
        0
    );
    assert_eq!(fx.faults.calls("embed_text"), 0);
    assert_eq!(fx.snapshot(0).await, before);
}

#[tokio::test]
async fn an_attribution_check_failure_never_deletes_the_point() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    fx.faults
        .arm("fact_id_exists_in_any_tenant", 1, Mode::Error);
    let report = fx.repair(0).await;
    assert_eq!(fx.faults.fired("fact_id_exists_in_any_tenant"), 1, "X1");
    assert_eq!(report.outcome, "blocked");
    let ta = fx
        .raw_vectors
        .scroll_points(&PointScope::Tenant(tenant(0)), None, 1000)
        .await
        .unwrap();
    assert!(
        ta.points
            .iter()
            .any(|p| p.point_id == dv.conflict_point || p.point_id == dv.fact_orphan_point),
        "the point whose attribution failed is kept"
    );
}

#[tokio::test]
async fn a_concurrent_edit_before_the_repair_is_not_overwritten() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    let mut before = BTreeMap::new();
    for id in [dv.missing, dv.stale, dv.duplicated] {
        before.insert(id, fx.fact_points(0, id).await);
    }
    // The first get_fact of the repair is the guard of the first repaired live fact: a concurrent
    // writer changes that row just before the guard reads it.
    fx.faults.arm("get_fact", 1, Mode::ConcurrentEdit);
    let report = fx.repair(0).await;
    assert_eq!(fx.faults.fired("get_fact"), 1, "X1");
    let changed: Vec<_> = report
        .items
        .iter()
        .filter(|i| i.action == "skipped_changed")
        .collect();
    assert_eq!(
        changed.len(),
        1,
        "the changed row is skipped, not repaired from the old text"
    );
    assert_eq!(report.outcome, "repair_incomplete");
    let edited = Uuid::parse_str(changed[0].entity_id.as_deref().unwrap()).unwrap();
    let row = fx
        .raw_facts
        .get_fact(&tenant(0), edited)
        .await
        .unwrap()
        .unwrap();
    assert!(row.content.ends_with("(edited concurrently)"));
    assert_eq!(
        fx.fact_points(0, edited).await,
        before[&edited],
        "nothing written for the edited fact"
    );
    fx.faults.reset();
    let rerun = fx.repair(0).await;
    assert_eq!(
        rerun.after.as_ref().unwrap().remaining_inconsistent,
        0,
        "resumed by a re-run"
    );
}

#[tokio::test]
async fn an_expected_episode_point_that_is_missing_is_detected_and_repaired() {
    let fx = fx().await;
    let e1 = fx
        .episode_store
        .list_episodes(&tenant(2), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.embedded)
        .unwrap();
    fx.raw_episode_vectors
        .delete_by_fact_id(&tenant(2), e1.id)
        .await
        .unwrap();
    let report = fx.dry(2).await;
    assert_eq!(cat(&report.episodes, "missing_vector"), 1);
    let repaired = fx.repair(2).await;
    assert_eq!(repaired.outcome, "repair_completed");
    assert_eq!(fx.episode_points(2, e1.id).await.len(), 1);
}

#[tokio::test]
async fn a_failing_delete_is_surfaced_as_failed() {
    let fx = fx().await;
    let dv = dv(&fx, 0, 1).await;
    fx.faults.arm("delete_points", 1, Mode::Error);
    let report = fx.repair(0).await;
    assert_eq!(fx.faults.fired("delete_points"), 1, "X1");
    assert!(report.repairs.failed >= 1);
    assert_eq!(report.outcome, "repair_incomplete");
    let _ = dv;
}

#[tokio::test]
async fn small_pages_enumerate_completely() {
    let fx = fx().await;
    dv(&fx, 0, 1).await;
    let mut options = VerifyOptions::dry_run();
    options.page_size = 1;
    let small = verify_vectors(
        &fx.stores(VectorIdentityStatus::Verified),
        &tenant(0),
        &options,
    )
    .await;
    let normal = fx.dry(0).await;
    assert!(small.scan.complete);
    assert_eq!(small.facts, normal.facts);
    assert_eq!(small.episodes, normal.episodes);
    assert_eq!(small.global, normal.global);
}
