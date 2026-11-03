//! The remaining protocol v1.0 §5 cases (W2-U, W6, W6-E, W7, FU1) and
//! the R6 "write failures by path" counter (`memcore_cross_store_write_failures_total`), with the
//! E3 cardinality rule applied to it. In-process engine over the mocks, wrapped by a test-only
//! `FaultPlan` over `FactStore`, `VectorStore` (facts and episodes), `EpisodeStore`,
//! `MemoryEventStore` and `EmbeddingProvider` (protocol §1 test-double extensions).
//!
//! Rules: X1 every armed fault fired exactly once at the intended call; X2 TB / TC snapshots
//! unchanged and every logged call of a TA operation carries TA; X3 no foreign canary in outputs,
//! errors or metric keys; X4 every case runs twice from a fresh fixture with equal summaries; X5 a
//! served text is the SQL row's. Invented data only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::observability::{CROSS_STORE_WRITE_FAILURES_METRIC, CROSS_STORE_WRITE_PATHS};
use memcore_core::ports::{
    FactFulltextQuery, MemoryEventQuery, OrgMemoryEventQuery, OrgUserListQuery, OrgUserSummary,
    PointScope, RetentionPruneResult, ScoredFact, StoredFactState, VectorIdentityStatus,
    VectorPointState,
};
use memcore_core::retention::{ApplyRetentionInput, RetentionPolicy};
use memcore_core::verify::{VerifyOptions, VerifyReport, VerifyStores, verify_vectors};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, DeleteMemoryInput, EmbeddingProvider, Episode,
    EpisodeComponents, EpisodeStore, ExportUserDataInput, Fact, FactOperation,
    FactOperationDecision, FactSearchQuery, FactStore, ForgetUserInput, ImportMode,
    ImportUserDataInput, MemoryEngine, MemoryEvent, MemoryEventOperation, MemoryEventStore,
    MemoryMessage, MemoryType, MessageRole, RetrievalMode, SearchEpisodesInput, SearchMemoryInput,
    SearchOptions, TenantContext, VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore,
    VectorWriteIntent,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore};
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
}

impl Captured {
    /// The write-failure counter, by `path`, for the paths that were incremented.
    fn failures(&self) -> BTreeMap<String, u64> {
        let prefix = format!("{CROSS_STORE_WRITE_FAILURES_METRIC}{{path=");
        self.counters
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix('}'))
                    .map(|path| (path.to_string(), *value))
            })
            .filter(|(_, value)| *value > 0)
            .collect()
    }

    fn failure_keys(&self) -> BTreeSet<String> {
        self.counters
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(CROSS_STORE_WRITE_FAILURES_METRIC))
            .cloned()
            .collect()
    }

    fn all_keys(&self) -> Vec<String> {
        self.counters.lock().unwrap().keys().cloned().collect()
    }
}

fn expected(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(p, v)| (p.to_string(), *v)).collect()
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

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

// ---------------------------------------------------------------------------------------------
// FaultPlan (protocol §1): fails the n-th call of a named operation BEFORE the inner call (no
// effect) and logs every call as (seq, op, tenant, fired)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct PlanState {
    seq: usize,
    calls: HashMap<&'static str, usize>,
    armed: HashMap<&'static str, usize>,
    fired: HashMap<&'static str, Vec<usize>>,
    log: Vec<(usize, &'static str, String, bool)>,
}

#[derive(Default)]
struct FaultPlan {
    state: Mutex<PlanState>,
}

impl FaultPlan {
    /// Arms `op` to fail its `nth` call counted from now.
    fn arm(&self, op: &'static str, nth: usize) {
        let mut state = self.state.lock().unwrap();
        state.calls.insert(op, 0);
        state.armed.insert(op, nth);
    }

    /// Clears every arming, count and the log (call before the operation under test).
    fn reset(&self) {
        *self.state.lock().unwrap() = PlanState::default();
    }

    fn hit(&self, op: &'static str, tenant: Option<&TenantContext>) -> MemcoreResult<()> {
        let mut state = self.state.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        let n = {
            let count = state.calls.entry(op).or_insert(0);
            *count += 1;
            *count
        };
        let fire = state.armed.get(op) == Some(&n);
        if fire {
            state.fired.entry(op).or_default().push(n);
        }
        let who = tenant
            .map(|t| format!("{}/{}", t.org_id, t.user_id))
            .unwrap_or_else(|| "-".to_string());
        state.log.push((seq, op, who, fire));
        if fire {
            Err(MemcoreError::Internal(format!("injected {op} failure")))
        } else {
            Ok(())
        }
    }

    /// The call numbers at which `op` fired.
    fn fired(&self, op: &'static str) -> Vec<usize> {
        self.state
            .lock()
            .unwrap()
            .fired
            .get(op)
            .cloned()
            .unwrap_or_default()
    }

    fn calls(&self, op: &'static str) -> usize {
        self.state
            .lock()
            .unwrap()
            .calls
            .get(op)
            .copied()
            .unwrap_or(0)
    }

    /// X2: every logged call that names a tenant names `tenant`.
    fn only_tenant(&self, tenant: &TenantContext) -> bool {
        let who = format!("{}/{}", tenant.org_id, tenant.user_id);
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .all(|(_, _, t, _)| t == "-" || *t == who)
    }
}

struct PlanFacts {
    inner: Arc<MockFactStore>,
    plan: Arc<FaultPlan>,
}

#[async_trait]
impl FactStore for PlanFacts {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.plan.hit("FactStore.insert_fact", Some(tenant))?;
        self.inner.insert_fact(tenant, fact).await
    }
    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.plan.hit("FactStore.update_fact", Some(tenant))?;
        self.inner.update_fact(tenant, fact).await
    }
    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
        self.plan.hit("FactStore.get_fact", Some(tenant))?;
        self.inner.get_fact(tenant, fact_id).await
    }
    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.plan
            .hit("FactStore.get_fact_including_deleted", Some(tenant))?;
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
        self.plan
            .hit("FactStore.search_facts", Some(&query.tenant))?;
        self.inner.search_facts(query).await
    }
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>> {
        self.inner.fulltext_search_facts(query).await
    }
    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.plan.hit("FactStore.soft_delete_fact", Some(tenant))?;
        self.inner.soft_delete_fact(tenant, fact_id).await
    }
    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.plan.hit("FactStore.delete_user_data", Some(tenant))?;
        self.inner.delete_user_data(tenant).await
    }
    async fn delete_facts_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        self.plan
            .hit("FactStore.delete_facts_older_than", Some(tenant))?;
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

/// A vector store under a name prefix (`VectorStore` for facts, `EpisodeVectors` for episodes).
struct PlanVectors {
    name: &'static str,
    inner: Arc<MockVectorStore>,
    plan: Arc<FaultPlan>,
}

impl PlanVectors {
    fn op(&self, op: &'static str) -> &'static str {
        match (self.name, op) {
            ("VectorStore", "upsert_vector") => "VectorStore.upsert_vector",
            ("VectorStore", "write_vector") => "VectorStore.write_vector",
            ("VectorStore", "delete_by_fact_id") => "VectorStore.delete_by_fact_id",
            ("VectorStore", "delete_by_user") => "VectorStore.delete_by_user",
            ("VectorStore", "search_vectors") => "VectorStore.search_vectors",
            ("EpisodeVectors", "upsert_vector") => "EpisodeVectors.upsert_vector",
            ("EpisodeVectors", "write_vector") => "EpisodeVectors.write_vector",
            ("EpisodeVectors", "delete_by_fact_id") => "EpisodeVectors.delete_by_fact_id",
            ("EpisodeVectors", "delete_by_user") => "EpisodeVectors.delete_by_user",
            ("EpisodeVectors", "search_vectors") => "EpisodeVectors.search_vectors",
            _ => "other",
        }
    }
}

#[async_trait]
impl VectorStore for PlanVectors {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        self.plan.hit(self.op("upsert_vector"), Some(tenant))?;
        self.inner.upsert_vector(tenant, record).await
    }
    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        self.plan.hit(self.op("write_vector"), Some(tenant))?;
        self.inner.write_vector(tenant, record, intent).await
    }
    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.plan
            .hit(self.op("search_vectors"), Some(&query.tenant))?;
        self.inner.search_vectors(query).await
    }
    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.plan.hit(self.op("delete_by_fact_id"), Some(tenant))?;
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }
    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.plan.hit(self.op("delete_by_user"), Some(tenant))?;
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

struct PlanEpisodes {
    inner: Arc<MockEpisodeStore>,
    plan: Arc<FaultPlan>,
}

#[async_trait]
impl EpisodeStore for PlanEpisodes {
    async fn insert_episodes(
        &self,
        tenant: &TenantContext,
        episodes: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        self.plan
            .hit("EpisodeStore.insert_episodes", Some(tenant))?;
        self.inner.insert_episodes(tenant, episodes).await
    }
    async fn insert_episodes_checked(
        &self,
        tenant: &TenantContext,
        episodes: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        self.plan
            .hit("EpisodeStore.insert_episodes_checked", Some(tenant))?;
        self.inner.insert_episodes_checked(tenant, episodes).await
    }
    async fn get_episodes(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<Vec<Episode>> {
        self.inner.get_episodes(tenant, ids).await
    }
    async fn mark_episodes_embedded(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<()> {
        self.plan
            .hit("EpisodeStore.mark_episodes_embedded", Some(tenant))?;
        self.inner.mark_episodes_embedded(tenant, ids).await
    }
    async fn list_episodes(
        &self,
        tenant: &TenantContext,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        self.inner.list_episodes(tenant, limit).await
    }
    async fn list_episodes_after(
        &self,
        tenant: &TenantContext,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        self.inner.list_episodes_after(tenant, after, limit).await
    }
    async fn delete_user_episodes(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.plan
            .hit("EpisodeStore.delete_user_episodes", Some(tenant))?;
        self.inner.delete_user_episodes(tenant).await
    }
    async fn delete_episodes_recorded_before(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<Vec<Uuid>> {
        self.plan
            .hit("EpisodeStore.delete_episodes_recorded_before", Some(tenant))?;
        self.inner
            .delete_episodes_recorded_before(tenant, cutoff, dry_run)
            .await
    }
    async fn count_episodes_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_episodes_by_org(org_id).await
    }
    async fn episode_id_exists_in_any_tenant(&self, episode_id: Uuid) -> MemcoreResult<bool> {
        self.inner.episode_id_exists_in_any_tenant(episode_id).await
    }
}

struct PlanEvents {
    inner: Arc<MockMemoryEventStore>,
    plan: Arc<FaultPlan>,
}

#[async_trait]
impl MemoryEventStore for PlanEvents {
    async fn record_event(
        &self,
        tenant: &TenantContext,
        event: MemoryEvent,
    ) -> MemcoreResult<MemoryEvent> {
        self.plan
            .hit("MemoryEventStore.record_event", Some(tenant))?;
        self.inner.record_event(tenant, event).await
    }
    async fn list_events(&self, query: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        self.inner.list_events(query).await
    }
    async fn list_events_by_org(
        &self,
        query: OrgMemoryEventQuery,
    ) -> MemcoreResult<Vec<MemoryEvent>> {
        self.inner.list_events_by_org(query).await
    }
    async fn delete_events_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize> {
        self.inner
            .delete_events_older_than(tenant, cutoff, dry_run)
            .await
    }
    async fn count_events_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_events_by_org(org_id).await
    }
    async fn erase_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.plan
            .hit("MemoryEventStore.erase_user_audit_content", Some(tenant))?;
        self.inner.erase_user_audit_content(tenant).await
    }
    async fn count_user_audit_content(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.inner.count_user_audit_content(tenant).await
    }
}

struct PlanEmbedding {
    inner: MockEmbeddingProvider,
    plan: Arc<FaultPlan>,
}

#[async_trait]
impl EmbeddingProvider for PlanEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        self.plan.hit("EmbeddingProvider.embed_text", None)?;
        self.inner.embed_text(text).await
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        self.plan.hit("EmbeddingProvider.embed_batch", None)?;
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
    let (org, user) = if index < TENANTS.len() {
        (TENANTS[index].0.to_string(), TENANTS[index].1.to_string())
    } else {
        ("org_extra".to_string(), format!("x{index}"))
    };
    TenantContext::new(org, user).expect("tenant")
}

fn canary(index: usize, n: usize) -> String {
    let t = tenant(index);
    format!("CANARY-{}-{}-{n}", t.org_id, t.user_id)
}

fn fact_text(index: usize, n: usize) -> String {
    let topic = [
        "kayaking on mountain lakes",
        "baking sourdough bread",
        "restoring vintage radios",
    ];
    format!("Fact {n} {} prefers {}", canary(index, n), topic[n - 1])
}

fn message(content: String, id: &str) -> MemoryMessage {
    MemoryMessage {
        role: MessageRole::User,
        content,
        message_id: Some(id.to_string()),
        ..Default::default()
    }
}

fn episode_messages(index: usize) -> Vec<MemoryMessage> {
    let mut messages: Vec<MemoryMessage> = (1..=3)
        .map(|n| {
            message(
                format!(
                    "Episode {n} message {} about weekend plans",
                    canary(index, 10 + n)
                ),
                &format!("e{n}"),
            )
        })
        .collect();
    messages.push(message("short message 4".to_string(), "e4")); // 15 chars
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
    plan: Arc<FaultPlan>,
    raw_facts: Arc<MockFactStore>,
    raw_vectors: Arc<MockVectorStore>,
    raw_episodes: Arc<MockEpisodeStore>,
    raw_episode_vectors: Arc<MockVectorStore>,
    raw_events: Arc<MockMemoryEventStore>,
    raw_embedding: MockEmbeddingProvider,
}

impl Fx {
    fn engine(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            Arc::new(PlanFacts {
                inner: self.raw_facts.clone(),
                plan: self.plan.clone(),
            }),
            Arc::new(PlanVectors {
                name: "VectorStore",
                inner: self.raw_vectors.clone(),
                plan: self.plan.clone(),
            }),
            Arc::new(llm),
            Arc::new(PlanEmbedding {
                inner: MockEmbeddingProvider::new(DIMS),
                plan: self.plan.clone(),
            }),
        )
        .with_event_store(Arc::new(PlanEvents {
            inner: self.raw_events.clone(),
            plan: self.plan.clone(),
        }))
        .with_episodes(Some(EpisodeComponents {
            store: Arc::new(PlanEpisodes {
                inner: self.raw_episodes.clone(),
                plan: self.plan.clone(),
            }),
            vectors: Arc::new(PlanVectors {
                name: "EpisodeVectors",
                inner: self.raw_episode_vectors.clone(),
                plan: self.plan.clone(),
            }),
        }))
        .with_search_max_limit(100)
        .expect("search max limit")
    }

    fn plain(&self) -> MemoryEngine {
        self.engine(MockLlmProvider::new().with_extraction_candidates(vec![]))
    }

    async fn facts_of(&self, index: usize) -> Vec<StoredFactState> {
        self.raw_facts
            .list_fact_states(&tenant(index), None, 1000)
            .await
            .expect("facts")
    }

    async fn fact_n(&self, index: usize, n: usize) -> Fact {
        let marker = canary(index, n);
        self.facts_of(index)
            .await
            .into_iter()
            .map(|s| s.fact)
            .find(|fact| fact.content.contains(&marker))
            .expect("fixture fact")
    }

    async fn points(store: &MockVectorStore, scope: PointScope) -> Vec<(String, String)> {
        let mut points: Vec<(String, String)> = store
            .scroll_points(&scope, None, 10_000)
            .await
            .expect("points")
            .points
            .into_iter()
            .map(|p| {
                (
                    p.entity_id.unwrap_or_default(),
                    p.content.unwrap_or_default(),
                )
            })
            .collect();
        points.sort();
        points
    }

    async fn fact_points(&self, index: usize) -> Vec<(String, String)> {
        Self::points(&self.raw_vectors, PointScope::Tenant(tenant(index))).await
    }

    async fn points_of(&self, index: usize, fact_id: Uuid) -> Vec<(String, String)> {
        Self::points(
            &self.raw_vectors,
            PointScope::TenantEntity(tenant(index), fact_id),
        )
        .await
    }

    async fn episode_points(&self, index: usize) -> Vec<(String, String)> {
        Self::points(&self.raw_episode_vectors, PointScope::Tenant(tenant(index))).await
    }

    async fn episodes(&self, index: usize) -> Vec<Episode> {
        let mut episodes = self
            .raw_episodes
            .list_episodes(&tenant(index), 1000)
            .await
            .expect("episodes");
        episodes.sort_by_key(|e| e.id);
        episodes
    }

    async fn events(&self, index: usize) -> Vec<MemoryEvent> {
        self.raw_events
            .list_events(MemoryEventQuery::new(tenant(index), 200))
            .await
            .expect("events")
    }

    /// `S(tenant)`: fact rows, fact points, episode rows, episode points and the event count.
    async fn snapshot(&self, index: usize) -> String {
        let mut rows: Vec<String> = self
            .facts_of(index)
            .await
            .iter()
            .map(|s| format!("{}|{}|{}", s.fact.id, s.fact.content, s.deleted))
            .collect();
        rows.sort();
        let episodes: Vec<String> = self
            .episodes(index)
            .await
            .iter()
            .map(|e| format!("{}|{}|{}", e.id, e.content, e.embedded))
            .collect();
        format!(
            "{rows:?}\n{:?}\n{episodes:?}\n{:?}\nevents={}",
            self.fact_points(index).await,
            self.episode_points(index).await,
            self.events(index).await.len()
        )
    }

    async fn others(&self) -> (String, String) {
        (self.snapshot(1).await, self.snapshot(2).await)
    }

    /// `consistent(T)`: every live fact has exactly one point whose payload is the row's text, no
    /// fact point lacks a live row, and every embeddable episode flagged embedded has one point.
    async fn consistent(&self, index: usize) -> Result<(), String> {
        let states = self.facts_of(index).await;
        let live: HashMap<String, String> = states
            .iter()
            .filter(|s| !s.deleted)
            .map(|s| (s.fact.id.to_string(), s.fact.content.clone()))
            .collect();
        for (id, content) in &live {
            let points = self.points_of(index, Uuid::parse_str(id).unwrap()).await;
            if points.len() != 1 || points[0].1 != *content {
                return Err(format!("fact {id}: points {points:?}"));
            }
        }
        for (id, _) in self.fact_points(index).await {
            if !live.contains_key(&id) {
                return Err(format!("point without a live row: {id}"));
            }
        }
        let episode_points = self.episode_points(index).await;
        for episode in self.episodes(index).await {
            if episode.embedded && episode.is_embeddable() {
                let n = episode_points
                    .iter()
                    .filter(|(id, _)| *id == episode.id.to_string())
                    .count();
                if n != 1 {
                    return Err(format!("episode {}: {n} points", episode.id));
                }
            }
        }
        Ok(())
    }

    /// R5 dry run over the raw stores (no write).
    async fn verify(&self, index: usize) -> VerifyReport {
        verify_vectors(
            &VerifyStores {
                facts: self.raw_facts.as_ref(),
                episodes: Some(self.raw_episodes.as_ref()),
                fact_vectors: Some(self.raw_vectors.as_ref()),
                episode_vectors: Some(self.raw_episode_vectors.as_ref()),
                embedding: Some(&self.raw_embedding),
                fact_identity: VectorIdentityStatus::Verified,
                episode_identity: VectorIdentityStatus::Verified,
            },
            &tenant(index),
            &VerifyOptions::dry_run(),
        )
        .await
    }
}

async fn fx_with(extra_tenants: usize) -> Fx {
    let fx = Fx {
        plan: Arc::new(FaultPlan::default()),
        raw_facts: Arc::new(MockFactStore::new()),
        raw_vectors: Arc::new(MockVectorStore::new()),
        raw_episodes: Arc::new(MockEpisodeStore::new()),
        raw_episode_vectors: Arc::new(MockVectorStore::new()),
        raw_events: Arc::new(MockMemoryEventStore::new()),
        raw_embedding: MockEmbeddingProvider::new(DIMS),
    };
    for index in 0..TENANTS.len() + extra_tenants {
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
    fx.plan.reset();
    fx
}

async fn fx() -> Fx {
    fx_with(0).await
}

fn request(index: usize, message_id: &str) -> AddMemoryInput {
    AddMemoryInput {
        tenant: tenant(index),
        messages: vec![message(
            format!(
                "Message {message_id} for {} with enough text",
                canary(index, 20)
            ),
            message_id,
        )],
        metadata: json!({}),
        session: Default::default(),
    }
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

async fn ingest(
    fx: &Fx,
    llm: MockLlmProvider,
    input: AddMemoryInput,
) -> MemcoreResult<AddMemoryOutput> {
    fx.engine(llm).add_memory(input).await
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

async fn search_episodes(fx: &Fx, index: usize, query: &str) -> Vec<String> {
    fx.plain()
        .search_episodes(SearchEpisodesInput {
            tenant: tenant(index),
            query: query.to_string(),
            limit: 10,
        })
        .await
        .expect("episode search")
        .results
        .into_iter()
        .map(|r| r.episode.content)
        .collect()
}

/// X3: no other fixture tenant's canary in `text`.
fn assert_no_foreign_canary(text: &str, own: usize) {
    for index in 0..TENANTS.len() {
        if index != own {
            for n in 1..=30 {
                assert!(
                    !text.contains(&canary(index, n)),
                    "X3: foreign canary in {text}"
                );
            }
        }
    }
}

/// X3 for metrics: keys of the family carry only `path` from the fixed list; no canary, org or
/// user id anywhere in any key.
fn assert_bounded_metrics(captured: &Captured) {
    for key in captured.failure_keys() {
        let path = key
            .strip_prefix(&format!("{CROSS_STORE_WRITE_FAILURES_METRIC}{{path="))
            .and_then(|rest| rest.strip_suffix('}'))
            .unwrap_or_else(|| panic!("unexpected key shape {key}"));
        assert!(
            CROSS_STORE_WRITE_PATHS.contains(&path),
            "unknown path {path}"
        );
    }
    for key in captured.all_keys() {
        assert!(!key.contains("CANARY"), "{key}");
        for index in 0..TENANTS.len() {
            assert!(!key.contains(&tenant(index).user_id), "{key}");
        }
        assert!(!key.contains("org_"), "{key}");
    }
}

/// Runs `case` twice from fresh fixtures (X4); the summaries must be equal.
async fn twice<F, Fut>(case: F) -> String
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let first = case().await;
    let second = case().await;
    assert_eq!(first, second, "X4: the two runs differ");
    first
}

// ---------------------------------------------------------------------------------------------
// W2-U: a later different update after W2 (no fault)
// ---------------------------------------------------------------------------------------------

async fn case_w2_u() -> String {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let f1 = fx.fact_n(0, 1).await;
    let new_text = format!("Fact 1 {} now prefers sea kayaking at night", canary(0, 1));
    let newest = format!("Fact 1 {} finally prefers rowing on rivers", canary(0, 1));

    // Prerequisite W2: the UPDATE's vector write fails after the row update.
    fx.plan.arm("VectorStore.write_vector", 1);
    let w2 = ingest(&fx, updating(&new_text, f1.id), request(0, "w2")).await;
    let w2_err = w2.expect_err("W2: 5xx");
    assert_eq!(fx.plan.fired("VectorStore.write_vector"), vec![1], "X1");
    assert!(fx.plan.only_tenant(&tenant(0)), "X2 (log)");
    assert_no_foreign_canary(&w2_err.to_string(), 0);
    let row = fx
        .raw_facts
        .get_fact(&tenant(0), f1.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.content, new_text, "W2: row = new text");
    assert_eq!(
        fx.points_of(0, f1.id).await,
        vec![(f1.id.to_string(), f1.content.clone())],
        "W2: point = old text"
    );
    assert_eq!(captured.failures(), expected(&[("fact_update", 1)]), "R6");

    // W2-U: a later different update of F1, no fault.
    fx.plan.reset();
    let out = ingest(&fx, updating(&newest, f1.id), request(0, "w2u"))
        .await
        .expect("W2-U: 200");
    assert_eq!((out.added, out.updated), (0, 1));
    assert!(fx.plan.only_tenant(&tenant(0)), "X2 (log)");
    let row = fx
        .raw_facts
        .get_fact(&tenant(0), f1.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.content, newest, "newest text");
    // The update returned 200 only after its own verification found exactly
    // one point carrying the row's text.
    assert_eq!(
        fx.points_of(0, f1.id).await,
        vec![(f1.id.to_string(), newest.clone())],
        "1 point = newest"
    );
    fx.consistent(0).await.expect("consistent(TA)");
    for mode in [
        RetrievalMode::Semantic,
        RetrievalMode::Hybrid,
        RetrievalMode::Rrf,
    ] {
        for query in [f1.content.as_str(), new_text.as_str(), newest.as_str()] {
            for (id, content) in search(&fx, 0, query, mode).await {
                assert!(
                    !content.contains("mountain lakes"),
                    "the original text is never served"
                );
                assert!(
                    !content.contains("sea kayaking"),
                    "the W2 text is never served"
                );
                assert_no_foreign_canary(&content, 0);
                if id == f1.id {
                    assert_eq!(content, newest, "X5: SQL text");
                }
            }
        }
    }
    assert!(
        search(&fx, 0, &newest, RetrievalMode::Semantic)
            .await
            .contains(&(f1.id, newest.clone()))
    );
    assert_eq!(
        captured.failures(),
        expected(&[("fact_update", 1)]),
        "W2-U adds none"
    );
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded_metrics(&captured);
    format!(
        "W2-U: w2=err fired={:?}; w2u=200 updated={}; points=1; failures={:?}",
        vec![1],
        out.updated,
        captured.failures()
    )
}

#[tokio::test]
async fn w2_u_a_later_update_after_w2_converges_to_one_newest_point() {
    let summary = twice(case_w2_u).await;
    eprintln!("{summary}");
}

// ---------------------------------------------------------------------------------------------
// W6 / W6-E: episode indexing windows
// ---------------------------------------------------------------------------------------------

fn w6_request() -> AddMemoryInput {
    AddMemoryInput {
        tenant: tenant(0),
        messages: vec![
            message(
                format!("New episode A {} about the harbour festival", canary(0, 21)),
                "w6a",
            ),
            message(
                format!("New episode B {} about the night market", canary(0, 22)),
                "w6b",
            ),
            message("tiny w6 message".to_string(), "w6c"), // 15 chars: not embeddable
        ],
        metadata: json!({}),
        session: Default::default(),
    }
}

async fn w6_case(op: &'static str) -> String {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let rows_before = fx.episodes(0).await.len();
    let points_before = fx.episode_points(0).await.len();

    fx.plan.arm(op, 1);
    let err = fx.plain().add_memory(w6_request()).await.expect_err("5xx");
    assert_eq!(fx.plan.fired(op), vec![1], "X1: {op} fired once");
    assert!(fx.plan.only_tenant(&tenant(0)), "X2 (log)");
    assert_no_foreign_canary(&err.to_string(), 0);
    let new_rows: Vec<Episode> = fx
        .episodes(0)
        .await
        .into_iter()
        .filter(|e| {
            e.message_id
                .as_deref()
                .is_some_and(|id| id.starts_with("w6"))
        })
        .collect();
    assert_eq!(new_rows.len(), 3, "3 rows");
    assert!(new_rows.iter().all(|e| !e.embedded), "embedded = 0");
    assert_eq!(fx.episodes(0).await.len(), rows_before + 3);
    let points_after_fault = fx.episode_points(0).await.len() - points_before;
    let expected_points = if op == "EmbeddingProvider.embed_batch" {
        0
    } else {
        2
    };
    assert_eq!(points_after_fault, expected_points, "{op}: new points");
    let pending = fx.verify(0).await;
    assert_eq!(
        pending.episodes["episode_pending"], 2,
        "R5 sees the 2 pending episodes"
    );
    assert_eq!(
        pending.episodes["episode_short_exempt"], 2,
        "E4 + the new short: exempt"
    );
    assert_eq!(captured.failures(), expected(&[("episode_index", 1)]), "R6");

    // The stable-id retry, no fault.
    fx.plan.reset();
    fx.plain()
        .add_memory(w6_request())
        .await
        .expect("retry 200");
    let mut flags: Vec<(String, bool)> = fx
        .episodes(0)
        .await
        .into_iter()
        .filter_map(|e| {
            e.message_id
                .clone()
                .filter(|id| id.starts_with("w6"))
                .map(|id| (id, e.embedded))
        })
        .collect();
    flags.sort();
    assert_eq!(
        flags,
        vec![
            ("w6a".to_string(), true),
            ("w6b".to_string(), true),
            ("w6c".to_string(), false)
        ],
        "flags = 1, 1, 0 (short)"
    );
    assert_eq!(
        fx.episodes(0).await.len(),
        rows_before + 3,
        "no duplicate episode"
    );
    assert_eq!(
        fx.episode_points(0).await.len(),
        points_before + 2,
        "2 points, no third"
    );
    let after = fx.verify(0).await;
    assert_eq!(after.episodes["episode_pending"], 0, "nothing pending");
    assert_eq!(
        after.episodes["episode_short_exempt"], 2,
        "short never pending"
    );
    assert_eq!(after.outcome, "clean");
    fx.consistent(0).await.expect("consistent(TA)");
    assert_eq!(
        captured.failures(),
        expected(&[("episode_index", 1)]),
        "retry adds none"
    );
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded_metrics(&captured);
    format!(
        "{op}: err; rows+3 embedded=0; new points={points_after_fault}; pending=2; retry flags={flags:?}; points+2; failures={:?}",
        captured.failures()
    )
}

#[tokio::test]
async fn w6_failed_embedded_flag_is_completed_by_the_stable_id_retry() {
    let summary = twice(|| w6_case("EpisodeStore.mark_episodes_embedded")).await;
    eprintln!("{summary}");
}

#[tokio::test]
async fn w6_e_failed_episode_embedding_is_completed_by_the_stable_id_retry() {
    let summary = twice(|| w6_case("EmbeddingProvider.embed_batch")).await;
    eprintln!("{summary}");
}

// ---------------------------------------------------------------------------------------------
// W7: a best-effort audit event that cannot be stored
// ---------------------------------------------------------------------------------------------

async fn case_w7() -> String {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let text = format!("Fact 7 {} prefers mapping hiking trails", canary(0, 7));
    let events_before = fx.events(0).await.len();

    fx.plan.arm("MemoryEventStore.record_event", 1);
    let out = ingest(&fx, extracting(&text), request(0, "w7"))
        .await
        .expect("W7: 200");
    assert_eq!(
        fx.plan.fired("MemoryEventStore.record_event"),
        vec![1],
        "X1"
    );
    let event_calls = fx.plan.calls("MemoryEventStore.record_event");
    assert!(fx.plan.only_tenant(&tenant(0)), "X2 (log)");
    assert_eq!(out.added, 1);
    let row = fx
        .facts_of(0)
        .await
        .into_iter()
        .find(|s| s.fact.content == text)
        .expect("row live");
    assert!(!row.deleted);
    assert_eq!(
        fx.points_of(0, row.fact.id).await,
        vec![(row.fact.id.to_string(), text.clone())],
        "1 point"
    );
    assert!(
        search(&fx, 0, &text, RetrievalMode::Semantic)
            .await
            .contains(&(row.fact.id, text.clone())),
        "present"
    );
    fx.consistent(0).await.expect("consistent(TA)");
    let add_events = |events: &[MemoryEvent]| {
        events
            .iter()
            .filter(|e| e.fact_id == Some(row.fact.id) && e.operation == MemoryEventOperation::Add)
            .count()
    };
    let events = fx.events(0).await;
    assert_eq!(
        add_events(&events),
        0,
        "the ADD event is missing (best-effort)"
    );
    let stored_in_request = events.len() - events_before;
    assert_eq!(
        stored_in_request,
        event_calls - 1,
        "every other event of the request was stored"
    );
    assert_eq!(
        captured.failures(),
        expected(&[("event", 1)]),
        "R6 {{path=\"event\"}} +1"
    );

    // The identical retry (no fault): exact-dedup NOOP; the missing ADD event is not re-created.
    fx.plan.reset();
    let retry = ingest(&fx, extracting(&text), request(0, "w7"))
        .await
        .expect("retry 200");
    let after = fx.events(0).await;
    let retry_ops: Vec<String> = after
        .iter()
        .take(after.len() - events.len())
        .map(|e| format!("{:?}", e.operation))
        .collect();
    assert_eq!(add_events(&after), 0, "no ADD event recreated by the retry");
    assert_eq!((retry.added, retry.noop), (0, 1));
    assert_eq!(
        captured.failures(),
        expected(&[("event", 1)]),
        "retry adds none"
    );
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded_metrics(&captured);
    format!(
        "W7: 200 added=1; record_event calls={event_calls} fired=[1]; add_events=0; stored_others={stored_in_request}; retry noop=1 retry_events={retry_ops:?}; failures={:?}",
        captured.failures()
    )
}

#[tokio::test]
async fn w7_a_failed_audit_event_keeps_the_200_and_is_counted() {
    let summary = twice(case_w7).await;
    eprintln!("{summary}");
}

// ---------------------------------------------------------------------------------------------
// FU1: forget-user with a failing vector deletion
// ---------------------------------------------------------------------------------------------

async fn case_fu1() -> String {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    let x2 = fx.others().await;
    let fact_texts: Vec<String> = (1..=3).map(|n| fact_text(0, n)).collect();
    let episode_texts: Vec<String> = fx
        .episodes(0)
        .await
        .into_iter()
        .map(|e| e.content)
        .collect();
    let points_before = fx.fact_points(0).await.len();

    fx.plan.arm("VectorStore.delete_by_user", 1);
    let err = fx
        .plain()
        .forget_user(ForgetUserInput { tenant: tenant(0) })
        .await
        .expect_err("FU1: error");
    assert_eq!(fx.plan.fired("VectorStore.delete_by_user"), vec![1], "X1");
    assert!(fx.plan.only_tenant(&tenant(0)), "X2 (log)");
    assert_no_foreign_canary(&err.to_string(), 0);
    assert!(fx.facts_of(0).await.is_empty(), "(4) TA facts removed");
    let episodes_left = fx.episodes(0).await.len();
    assert_eq!(episodes_left, 0, "(4) TA episodes removed");
    assert_eq!(
        fx.fact_points(0).await.len(),
        points_before,
        "(5) TA points remain"
    );
    for text in &fact_texts {
        for mode in [
            RetrievalMode::Semantic,
            RetrievalMode::Hybrid,
            RetrievalMode::Fulltext,
        ] {
            assert!(
                search(&fx, 0, text, mode).await.is_empty(),
                "(7) no fact searchable"
            );
        }
    }
    for text in &episode_texts {
        assert!(
            search_episodes(&fx, 0, text).await.is_empty(),
            "(7) no episode searchable"
        );
    }
    assert_eq!(captured.failures(), expected(&[("forget_user", 1)]), "R6");

    // The retry completes the erasure.
    fx.plan.reset();
    fx.plain()
        .forget_user(ForgetUserInput { tenant: tenant(0) })
        .await
        .expect("retry");
    assert!(fx.facts_of(0).await.is_empty(), "0 TA rows");
    assert!(fx.episodes(0).await.is_empty());
    assert!(fx.fact_points(0).await.is_empty(), "0 TA points");
    assert!(fx.episode_points(0).await.is_empty());
    assert_eq!(
        fx.raw_events
            .count_user_audit_content(&tenant(0))
            .await
            .unwrap(),
        0,
        "audit erasure verified"
    );
    assert_eq!(
        captured.failures(),
        expected(&[("forget_user", 1)]),
        "retry adds none"
    );
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded_metrics(&captured);
    format!(
        "FU1: err fired=[1]; rows=0 episodes=0 points_remain={points_before}; nothing searchable; retry: 0 rows 0 points audit=0; failures={:?}",
        captured.failures()
    )
}

#[tokio::test]
async fn fu1_forget_user_with_a_failing_vector_delete_hides_everything_and_the_retry_completes() {
    let summary = twice(case_fu1).await;
    eprintln!("{summary}");
}

// ---------------------------------------------------------------------------------------------
// R6: one increment per failed path, none for success or a failure before the first write
// ---------------------------------------------------------------------------------------------

/// Runs `action` on a fresh FX with `op#nth` armed (if any) and returns (fired, counter deltas).
async fn r6_case(
    arm: Option<(&'static str, usize)>,
    action: impl AsyncFnOnce(&Fx) -> bool,
) -> (Vec<usize>, BTreeMap<String, u64>) {
    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);
    let fx = fx().await;
    assert!(
        captured.failures().is_empty(),
        "the fixture's writes count nothing"
    );
    let x2 = fx.others().await;
    if let Some((op, nth)) = arm {
        fx.plan.arm(op, nth);
    }
    let failed = action(&fx).await;
    let fired = arm.map(|(op, _)| fx.plan.fired(op)).unwrap_or_default();
    if let Some((op, nth)) = arm {
        assert_eq!(
            fired,
            vec![nth],
            "X1: {op}#{nth} (calls {}, failed {failed})",
            fx.plan.calls(op)
        );
    }
    let _ = failed;
    assert_eq!(fx.others().await, x2, "X2");
    assert_bounded_metrics(&captured);
    (fired, captured.failures())
}

async fn age_facts(fx: &Fx) {
    let t = tenant(0);
    for state in fx.facts_of(0).await {
        let mut fact = state.fact;
        fact.updated_at = Utc::now() - Duration::days(400);
        fx.raw_facts.update_fact(&t, fact).await.unwrap();
    }
}

fn retention(dry_run: bool) -> ApplyRetentionInput {
    ApplyRetentionInput {
        tenant: tenant(0),
        policy: RetentionPolicy {
            enabled: true,
            fact_retention_days: Some(30),
            event_retention_days: None,
        },
        dry_run,
    }
}

async fn export_ta(fx: &Fx) -> memcore_core::UserMemoryExport {
    fx.plain()
        .export_user_data(ExportUserDataInput {
            tenant: tenant(0),
            include_events: false,
            include_deleted: false,
        })
        .await
        .expect("export")
}

fn import_input(export: memcore_core::UserMemoryExport) -> ImportUserDataInput {
    ImportUserDataInput {
        tenant: tenant(0),
        export,
        mode: ImportMode::Append,
        restore_events: false,
        dry_run: false,
    }
}

#[tokio::test]
async fn r6_each_path_counts_one_failure_and_nothing_else() {
    let new_text = || format!("Fact 8 {} prefers carving wooden spoons", canary(0, 8));
    let mut rows: Vec<String> = Vec::new();

    // fact_add: the new fact's vector write fails (W1 window).
    let (_, got) = r6_case(Some(("VectorStore.write_vector", 1)), async |fx: &Fx| {
        ingest(fx, extracting(&new_text()), request(0, "a1"))
            .await
            .is_err()
    })
    .await;
    assert_eq!(got, expected(&[("fact_add", 1)]));
    rows.push(format!("fact_add {got:?}"));

    // fact_update: W2.
    let (_, got) = r6_case(Some(("VectorStore.write_vector", 1)), async |fx: &Fx| {
        let f1 = fx.fact_n(0, 1).await;
        ingest(fx, updating(&new_text(), f1.id), request(0, "u1"))
            .await
            .is_err()
    })
    .await;
    assert_eq!(got, expected(&[("fact_update", 1)]));
    rows.push(format!("fact_update {got:?}"));

    // fact_heal: W1, then the identical retry whose R4 repair write fails too.
    let (_, got) = r6_case(None, async |fx: &Fx| {
        fx.plan.arm("VectorStore.write_vector", 1);
        assert!(
            ingest(fx, extracting(&new_text()), request(0, "h1"))
                .await
                .is_err()
        );
        fx.plan.arm("VectorStore.write_vector", 1);
        let failed = ingest(fx, extracting(&new_text()), request(0, "h1"))
            .await
            .is_err();
        assert_eq!(
            fx.plan.fired("VectorStore.write_vector"),
            vec![1, 1],
            "X1 (twice, armed twice)"
        );
        failed
    })
    .await;
    assert_eq!(got, expected(&[("fact_add", 1), ("fact_heal", 1)]));
    rows.push(format!("fact_heal {got:?}"));

    // fact_delete: D1.
    let (_, got) = r6_case(
        Some(("VectorStore.delete_by_fact_id", 1)),
        async |fx: &Fx| {
            let f1 = fx.fact_n(0, 1).await;
            fx.plain()
                .delete_memory(DeleteMemoryInput {
                    tenant: tenant(0),
                    memory_id: f1.id,
                })
                .await
                .is_err()
        },
    )
    .await;
    assert_eq!(got, expected(&[("fact_delete", 1)]));
    rows.push(format!("fact_delete {got:?}"));

    // episode_index: W6.
    let (_, got) = r6_case(
        Some(("EpisodeStore.mark_episodes_embedded", 1)),
        async |fx: &Fx| fx.plain().add_memory(w6_request()).await.is_err(),
    )
    .await;
    assert_eq!(got, expected(&[("episode_index", 1)]));
    rows.push(format!("episode_index {got:?}"));

    // retention: RT1.
    let (_, got) = r6_case(None, async |fx: &Fx| {
        age_facts(fx).await;
        fx.plan.arm("VectorStore.delete_by_fact_id", 2);
        let failed = fx.plain().apply_retention(retention(false)).await.is_err();
        assert_eq!(
            fx.plan.fired("VectorStore.delete_by_fact_id"),
            vec![2],
            "X1"
        );
        failed
    })
    .await;
    assert_eq!(got, expected(&[("retention", 1)]));
    rows.push(format!("retention {got:?}"));

    // forget_user: FU1.
    let (_, got) = r6_case(Some(("VectorStore.delete_by_user", 1)), async |fx: &Fx| {
        fx.plain()
            .forget_user(ForgetUserInput { tenant: tenant(0) })
            .await
            .is_err()
    })
    .await;
    assert_eq!(got, expected(&[("forget_user", 1)]));
    rows.push(format!("forget_user {got:?}"));

    // import: W8-A (the second vector write of an append import fails).
    let (_, got) = r6_case(None, async |fx: &Fx| {
        let x = export_ta(fx).await;
        fx.plain()
            .forget_user(ForgetUserInput { tenant: tenant(0) })
            .await
            .unwrap();
        fx.plan.arm("VectorStore.upsert_vector", 2);
        let failed = fx.plain().import_user_data(import_input(x)).await.is_err();
        assert_eq!(fx.plan.fired("VectorStore.upsert_vector"), vec![2], "X1");
        failed
    })
    .await;
    assert_eq!(got, expected(&[("import", 1)]));
    rows.push(format!("import {got:?}"));

    // event: W7.
    let (_, got) = r6_case(
        Some(("MemoryEventStore.record_event", 1)),
        async |fx: &Fx| {
            ingest(fx, extracting(&new_text()), request(0, "ev1"))
                .await
                .is_err()
        },
    )
    .await;
    assert_eq!(got, expected(&[("event", 1)]));
    rows.push(format!("event {got:?}"));

    eprintln!("R6 matrix: {rows:#?}");
}

#[tokio::test]
async fn r6_success_and_failures_before_the_first_write_count_nothing() {
    // The same text as the fact_add case: one that dedup classifies as new (an ADD).
    let new_text = || format!("Fact 8 {} prefers carving wooden spoons", canary(0, 8));
    let none = BTreeMap::new();

    // Successful writes on every path.
    let (_, got) = r6_case(None, async |fx: &Fx| {
        let f1 = fx.fact_n(0, 1).await;
        let f2 = fx.fact_n(0, 2).await;
        ingest(fx, extracting(&new_text()), request(0, "s1"))
            .await
            .unwrap();
        ingest(
            fx,
            updating("Fact 1 now prefers canoeing on quiet rivers", f1.id),
            request(0, "s2"),
        )
        .await
        .unwrap();
        fx.plain().add_memory(w6_request()).await.unwrap();
        fx.plain()
            .delete_memory(DeleteMemoryInput {
                tenant: tenant(0),
                memory_id: f2.id,
            })
            .await
            .unwrap();
        fx.plain().apply_retention(retention(true)).await.unwrap();
        let x = export_ta(fx).await;
        fx.plain()
            .forget_user(ForgetUserInput { tenant: tenant(0) })
            .await
            .unwrap();
        fx.plain().import_user_data(import_input(x)).await.unwrap();
        false
    })
    .await;
    assert_eq!(got, none, "success counts nothing");

    // Failures before the operation's first write.
    for (op, label) in [
        ("FactStore.insert_fact", "add: insert fails"),
        ("FactStore.update_fact", "update: row update fails"),
        ("FactStore.soft_delete_fact", "delete: soft delete fails"),
        ("FactStore.delete_user_data", "forget: first deletion fails"),
        (
            "EpisodeStore.insert_episodes_checked",
            "ingest: episode insert fails",
        ),
    ] {
        let (_, got) = r6_case(Some((op, 1)), async |fx: &Fx| {
            let f1 = fx.fact_n(0, 1).await;
            match op {
                "FactStore.insert_fact" => ingest(fx, extracting(&new_text()), request(0, "n1"))
                    .await
                    .is_err(),
                "FactStore.update_fact" => {
                    ingest(fx, updating(&new_text(), f1.id), request(0, "n2"))
                        .await
                        .is_err()
                }
                "FactStore.soft_delete_fact" => fx
                    .plain()
                    .delete_memory(DeleteMemoryInput {
                        tenant: tenant(0),
                        memory_id: f1.id,
                    })
                    .await
                    .is_err(),
                "FactStore.delete_user_data" => fx
                    .plain()
                    .forget_user(ForgetUserInput { tenant: tenant(0) })
                    .await
                    .is_err(),
                _ => fx.plain().add_memory(w6_request()).await.is_err(),
            }
        })
        .await;
        assert_eq!(
            got, none,
            "{label}: nothing is out of step, nothing counted"
        );
    }

    // Rejections before any write: K1 (MESSAGE_ID_CONFLICT), R7 409 and a 400.
    let (_, got) = r6_case(None, async |fx: &Fx| {
        let mut changed = AddMemoryInput {
            tenant: tenant(0),
            messages: episode_messages(0),
            metadata: json!({}),
            session: Default::default(),
        };
        changed.messages[0].content = format!("Changed content {}", canary(0, 11));
        let k1 = fx.plain().add_memory(changed).await.expect_err("409");
        assert!(matches!(k1, MemcoreError::Conflict(_)), "{k1:?}");
        let x = export_ta(fx).await;
        let r7 = fx
            .plain()
            .import_user_data(import_input(x.clone()))
            .await
            .expect_err("409");
        assert!(r7.is_import_already_applied());
        let mut dup = x;
        let twin = dup.facts[0].clone();
        dup.facts.push(twin);
        let v = fx
            .plain()
            .import_user_data(import_input(dup))
            .await
            .expect_err("400");
        assert!(matches!(v, MemcoreError::ValidationError(_)));
        true
    })
    .await;
    assert_eq!(got, none, "rejections before any write count nothing");
}

#[tokio::test]
async fn e3_write_failure_series_are_bounded_and_independent_of_tenant_count() {
    async fn series(extra: usize) -> (BTreeSet<String>, BTreeMap<String, u64>) {
        let captured = Captured::default();
        let recorder = TestRecorder(captured.clone());
        let _guard = metrics::set_default_local_recorder(&recorder);
        let fx = fx_with(extra).await;
        for (op, run) in [
            ("VectorStore.write_vector", 0usize),
            ("MemoryEventStore.record_event", 1),
            ("EpisodeStore.mark_episodes_embedded", 2),
        ] {
            fx.plan.reset();
            fx.plan.arm(op, 1);
            let _ = match run {
                0 => {
                    ingest(
                        &fx,
                        extracting(&format!("Fact 8 {} carves spoons", canary(0, 8))),
                        request(0, "x1"),
                    )
                    .await
                }
                1 => {
                    ingest(
                        &fx,
                        extracting(&format!("Fact 9 {} paints murals", canary(0, 9))),
                        request(0, "x2"),
                    )
                    .await
                }
                _ => fx.plain().add_memory(w6_request()).await,
            };
            assert_eq!(fx.plan.fired(op), vec![1], "X1 {op}");
        }
        assert_bounded_metrics(&captured);
        (captured.failure_keys(), captured.failures())
    }
    let base = series(0).await;
    let wider = series(3).await;
    assert_eq!(
        base, wider,
        "same series set and values with 3 extra tenants"
    );
    assert_eq!(
        base.1,
        expected(&[("episode_index", 1), ("event", 1), ("fact_add", 1)])
    );
    assert!(base.0.len() <= CROSS_STORE_WRITE_PATHS.len());
}
