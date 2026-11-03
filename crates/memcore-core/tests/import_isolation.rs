//! Import id-mapping cases on the mocks.
//! IS13 and the SQL error classes of IS19 / IS21(8) run on in-process SQLite in
//! `memcore-api/tests/import_id_mapping_api.rs`; IS12 (W8-E) is the retry / import test
//! `w8_e_episode_only_export_without_message_ids_duplicates_as_documented`.
//!
//! A world is a fresh set of stores. TA = (org_a, u1) imports; TB = (org_a, u2) is the other
//! tenant whose rows vary between worlds; TC = (org_b, u1) has the same user id in another org.
//! W0: the source id `f` is absent everywhere; W1: `f` is a live fact of TB; W2: `f` is a
//! soft-deleted fact of TB. For ids TA owns, `f` cannot also be TB's (global keys), so V0 / V1
//! vary TB's other rows instead (V1 holds TA's content under another id). TA's state and the
//! request are identical in every world. Traces normalise only ids and times memcore mints
//! independently of other tenants, never a stored fact id or a fact reference.
//! Invented data only.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    FactFulltextQuery, OrgUserListQuery, OrgUserSummary, PointScope, RetentionPruneResult,
    ScoredFact, StoredFactState, VectorPointState,
};
use memcore_core::{
    DUPLICATE_FACT_ID, Entity, EntityComponents, EntityConfig, EntityExport, EntityRelation,
    EntityRole, EntityType, Episode, EpisodeComponents, ExportUserDataInput, Fact, FactEntityLink,
    FactSearchQuery, FactStore, ForgetUserInput, ImportMode, ImportUserDataInput,
    ImportUserDataOutput, MemoryEngine, MemoryEvent, MemoryEventOperation, MemorySource,
    MemoryType, MessageRole, TenantContext, UserMemoryExport, VectorRecord, VectorSearchQuery,
    VectorSearchResult, VectorStore, VectorWriteIntent, derived_import_fact_id,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{
    MockEntityStore, MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use uuid::Uuid;

const DIMS: usize = 8;

fn ta() -> TenantContext {
    TenantContext::new("org_a", "u1").expect("tenant")
}
fn tb() -> TenantContext {
    TenantContext::new("org_a", "u2").expect("tenant")
}
fn tc() -> TenantContext {
    TenantContext::new("org_b", "u1").expect("tenant")
}

fn id(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("uuid")
}

/// Source ids of the export (v4-shaped, as memcore mints them).
fn f_id() -> Uuid {
    id("5b0c6f1e-2a4d-4e8f-9c31-00000000000f")
}
fn g_id() -> Uuid {
    id("5b0c6f1e-2a4d-4e8f-9c31-00000000000a")
}
fn h_id() -> Uuid {
    id("5b0c6f1e-2a4d-4e8f-9c31-00000000000b")
}

fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 1, 9, minute, 0).unwrap()
}

fn fact(tenant: &TenantContext, fact_id: Uuid, content: &str, minute: u32) -> Fact {
    Fact::new(
        fact_id,
        tenant.org_id.clone(),
        tenant.user_id.clone(),
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
        json!({"label": content.split_whitespace().next().unwrap_or("x")}),
    )
    .expect("fact")
}

fn event(
    tenant: &TenantContext,
    fact_id: Option<Uuid>,
    op: MemoryEventOperation,
    minute: u32,
) -> MemoryEvent {
    MemoryEvent {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        fact_id,
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

fn entity(tenant: &TenantContext, entity_id: Uuid, name: &str) -> Entity {
    Entity {
        id: entity_id,
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        canonical_name: name.to_string(),
        entity_type: EntityType::Person,
        aliases: vec![],
        summary: None,
        is_speaker: false,
        name_embedding: None,
        name_embedding_model: None,
        name_embedding_dimensions: None,
        created_at: at(1),
        updated_at: at(1),
    }
}

fn link(tenant: &TenantContext, fact_id: Uuid, entity_id: Uuid) -> FactEntityLink {
    FactEntityLink {
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        fact_id,
        entity_id,
        role: EntityRole::Subject,
        mention: "Avery".to_string(),
        created_at: at(2),
    }
}

fn episode(tenant: &TenantContext, content: &str, message_id: Option<&str>) -> Episode {
    Episode {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        session_id: Some("S1".to_string()),
        message_id: message_id.map(str::to_string),
        speaker: None,
        role: MessageRole::User,
        content: content.to_string(),
        observed_at: None,
        recorded_at: at(3),
        embedded: false,
        metadata: json!({}),
    }
}

const F_TEXT: &str = "Avery leads the Tuesday trail running club.";
const G_TEXT: &str = "Avery moved the trail running club to Thursdays.";
const H_TEXT: &str = "Avery prefers oat milk in coffee.";

/// Export X of TA: f superseded by g, h; restored events on f, g and one without a fact;
/// entities Avery and Trail Club with links to f and g and a relation stated by f; two episodes,
/// one with a client message id.
fn export_x() -> UserMemoryExport {
    let t = ta();
    let mut f = fact(&t, f_id(), F_TEXT, 10);
    f.superseded_by = Some(g_id());
    f.superseded_at = Some(at(12));
    let facts = vec![
        f,
        fact(&t, g_id(), G_TEXT, 11),
        fact(&t, h_id(), H_TEXT, 13),
    ];
    let events = vec![
        event(&t, Some(f_id()), MemoryEventOperation::Add, 20),
        event(&t, Some(g_id()), MemoryEventOperation::Add, 21),
        event(&t, Some(f_id()), MemoryEventOperation::Invalidate, 22),
        event(&t, None, MemoryEventOperation::NoOp, 23),
    ];
    let avery = id("9a000000-0000-4000-8000-000000000001");
    let club = id("9a000000-0000-4000-8000-000000000002");
    UserMemoryExport::new(&t.org_id, &t.user_id, facts, events)
        .with_episodes(vec![
            episode(&t, "I run with the trail club every week.", Some("M1")),
            episode(&t, "The club now meets on Thursday evenings.", None),
        ])
        .with_entities(EntityExport {
            entities: vec![entity(&t, avery, "Avery"), entity(&t, club, "Trail Club")],
            links: vec![link(&t, f_id(), avery), link(&t, g_id(), avery)],
            relations: vec![EntityRelation {
                id: id("9a000000-0000-4000-8000-0000000000a1"),
                org_id: t.org_id.clone(),
                user_id: t.user_id.clone(),
                source_entity_id: avery,
                relation: "leads".to_string(),
                target_entity_id: club,
                valid_at: None,
                invalid_at: None,
                source_fact_id: Some(f_id()),
                created_at: at(2),
            }],
        })
}

/// X restricted to the facts with these ids (references of the rest stay in place).
fn only(mut x: UserMemoryExport, keep: &[Uuid]) -> UserMemoryExport {
    x.facts.retain(|f| keep.contains(&f.id));
    x
}

// ---------------------------------------------------------------------------------------------
// Probe: write counters, one vector-write fault and one insert gate (IS9, IS16, IS18)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Probe {
    counts: Mutex<HashMap<&'static str, usize>>,
    fail_upsert_at: Mutex<Option<usize>>,
    gate: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl Probe {
    fn hit(&self, op: &'static str) -> usize {
        let mut counts = self.counts.lock().unwrap();
        let n = counts.entry(op).or_insert(0);
        *n += 1;
        *n
    }
    fn count(&self, op: &'static str) -> usize {
        self.counts.lock().unwrap().get(op).copied().unwrap_or(0)
    }
    fn reset(&self) {
        self.counts.lock().unwrap().clear();
    }
    fn writes(&self) -> usize {
        [
            "insert_fact",
            "soft_delete_fact",
            "delete_user_data",
            "upsert_vector",
            "write_vector",
            "delete_by_fact_id",
            "delete_by_user",
        ]
        .iter()
        .map(|op| self.count(op))
        .sum()
    }
}

struct ProbeFacts {
    inner: Arc<MockFactStore>,
    probe: Arc<Probe>,
}

#[async_trait]
impl FactStore for ProbeFacts {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.probe.hit("insert_fact");
        let gate = self.probe.gate.lock().unwrap().take();
        if let Some((reached, release)) = gate {
            reached.notify_one();
            release.notified().await;
        }
        self.inner.insert_fact(tenant, fact).await
    }
    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.probe.hit("update_fact");
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
        // The import path must never consult this (amendment v1.1 §4).
        self.probe.hit("fact_id_exists_in_any_tenant");
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
        self.probe.hit("soft_delete_fact");
        self.inner.soft_delete_fact(tenant, fact_id).await
    }
    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.probe.hit("delete_user_data");
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

struct ProbeVectors {
    inner: Arc<MockVectorStore>,
    probe: Arc<Probe>,
}

#[async_trait]
impl VectorStore for ProbeVectors {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        let n = self.probe.hit("upsert_vector");
        if *self.probe.fail_upsert_at.lock().unwrap() == Some(n) {
            self.probe.hit("upsert_vector_fault_fired");
            return Err(MemcoreError::Internal(
                "injected upsert_vector failure".to_string(),
            ));
        }
        self.inner.upsert_vector(tenant, record).await
    }
    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        self.probe.hit("write_vector");
        self.inner.write_vector(tenant, record, intent).await
    }
    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(query).await
    }
    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.probe.hit("delete_by_fact_id");
        self.inner.delete_by_fact_id(tenant, fact_id).await
    }
    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.probe.hit("delete_by_user");
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
// Worlds and observation traces
// ---------------------------------------------------------------------------------------------

struct World {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
    entities: Arc<MockEntityStore>,
    probe: Arc<Probe>,
}

impl World {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
            events: Arc::new(MockMemoryEventStore::new()),
            episodes: Arc::new(MockEpisodeStore::new()),
            episode_vectors: Arc::new(MockVectorStore::new()),
            entities: Arc::new(MockEntityStore::new()),
            probe: Arc::new(Probe::default()),
        }
    }

    fn engine_with(&self, probe: Arc<Probe>) -> MemoryEngine {
        MemoryEngine::new(
            Arc::new(ProbeFacts {
                inner: self.facts.clone(),
                probe: probe.clone(),
            }),
            Arc::new(ProbeVectors {
                inner: self.vectors.clone(),
                probe,
            }),
            Arc::new(MockLlmProvider::new()),
            Arc::new(MockEmbeddingProvider::new(DIMS)),
        )
        .with_event_store(self.events.clone())
        .with_min_importance(0.0)
        .with_episodes(Some(EpisodeComponents {
            store: self.episodes.clone(),
            vectors: self.episode_vectors.clone(),
        }))
        .with_entities(Some(EntityComponents {
            store: self.entities.clone(),
            config: EntityConfig {
                name_similarity_threshold: 0.9,
                llm_confirmation: false,
                embedding_model: "mock/mock-embedding".to_string(),
            },
        }))
    }

    fn engine(&self) -> MemoryEngine {
        self.engine_with(self.probe.clone())
    }

    /// Plants a fact (and one point) directly in the stores, as pre-existing state.
    async fn plant(&self, tenant: &TenantContext, fact_id: Uuid, content: &str, deleted: bool) {
        let stored = self
            .facts
            .insert_fact(tenant, fact(tenant, fact_id, content, 5))
            .await
            .expect("plant");
        self.vectors
            .upsert_vector(
                tenant,
                VectorRecord {
                    id: Uuid::from_u128(fact_id.as_u128() ^ 0xFFFF),
                    fact_id,
                    org_id: tenant.org_id.clone(),
                    user_id: tenant.user_id.clone(),
                    embedding: vec![0.25; DIMS],
                    content: stored.content.clone(),
                    memory_type: stored.memory_type,
                    metadata: stored.metadata.clone(),
                },
            )
            .await
            .expect("plant point");
        if deleted {
            self.facts
                .soft_delete_fact(tenant, fact_id)
                .await
                .expect("soft delete");
        }
    }

    async fn import(
        &self,
        x: &UserMemoryExport,
        mode: ImportMode,
    ) -> MemcoreResult<ImportUserDataOutput> {
        self.engine()
            .import_user_data(ImportUserDataInput {
                tenant: ta(),
                export: x.clone(),
                mode,
                restore_events: true,
                dry_run: false,
            })
            .await
    }

    /// IS15: full snapshots of TB (same org) and TC (same user id, other org).
    async fn others(&self) -> Value {
        let (ids, times) = (HashSet::new(), HashSet::new());
        json!([
            self.observe(&tb(), &ids, &times).await,
            self.observe(&tc(), &ids, &times).await
        ])
    }

    /// Everything a tenant can observe of its own data (amendment §8 items 3–7).
    async fn observe(
        &self,
        tenant: &TenantContext,
        known: &HashSet<Uuid>,
        restored: &HashSet<String>,
    ) -> Value {
        let export = self
            .engine_with(Arc::new(Probe::default()))
            .export_user_data(ExportUserDataInput {
                tenant: tenant.clone(),
                include_events: true,
                include_deleted: true,
            })
            .await
            .expect("export");
        let states: Vec<Value> = self
            .facts
            .list_fact_states(tenant, None, 1000)
            .await
            .expect("states")
            .into_iter()
            .map(|s| json!([s.fact.id, s.deleted]))
            .collect();
        let mut points: Vec<Value> = self
            .vectors
            .scroll_points(&PointScope::Tenant(tenant.clone()), None, 1000)
            .await
            .expect("points")
            .points
            .into_iter()
            .map(|p| json!([p.entity_id, p.content]))
            .collect();
        points.sort_by_key(|p| p.to_string());
        let mut value = json!({
            "export": serde_json::to_value(&export).unwrap(),
            "fact_states": states,
            "points": points,
        });
        normalise(&mut value, &export, known, restored);
        value
    }
}

/// Amendment §8: labels for values memcore mints independently of other tenants only.
fn normalise(
    value: &mut Value,
    export: &UserMemoryExport,
    known: &HashSet<Uuid>,
    restored: &HashSet<String>,
) {
    let mut labels: HashMap<String, String> = HashMap::new();
    // Re-key fact ids: fresh v4 values not supplied by the export or the pre-state.
    for fact in &export.facts {
        if fact.id.get_version_num() == 4 && !known.contains(&fact.id) {
            labels.insert(fact.id.to_string(), format!("rekey:{}", fact.content));
        }
    }
    if let Some(entities) = &export.entities {
        for e in &entities.entities {
            labels.insert(e.id.to_string(), format!("entity:{}", e.canonical_name));
        }
        for r in &entities.relations {
            labels.insert(r.id.to_string(), format!("relation:{}", r.relation));
        }
    }
    for e in export.episodes.iter().flatten() {
        labels.insert(e.id.to_string(), format!("episode:{}", e.content));
    }
    for e in &export.memory_events {
        labels.insert(e.id.to_string(), "event".to_string());
    }
    let ex = &mut value["export"];
    ex["exported_at"] = json!("T");
    if let Some(events) = ex["memory_events"].as_array_mut() {
        for e in events {
            let created = e["created_at"].as_str().unwrap_or_default().to_string();
            if !restored.contains(&created) {
                e["created_at"] = json!("T");
            }
        }
    }
    if let Some(episodes) = ex["episodes"].as_array_mut() {
        for e in episodes {
            e["recorded_at"] = json!("T");
        }
    }
    relabel(value, &labels);
    sort_arrays(value);
}

fn relabel(value: &mut Value, labels: &HashMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(label) = labels.get(s.as_str()) {
                *s = label.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| relabel(v, labels)),
        Value::Object(map) => map.values_mut().for_each(|v| relabel(v, labels)),
        _ => {}
    }
}

fn sort_arrays(value: &mut Value) {
    match value {
        Value::Array(items) => {
            items.iter_mut().for_each(sort_arrays);
            items.sort_by_key(|v| v.to_string());
        }
        Value::Object(map) => map.values_mut().for_each(sort_arrays),
        _ => {}
    }
}

fn outcome(result: &MemcoreResult<ImportUserDataOutput>) -> Value {
    match result {
        Ok(out) => json!({"ok": out}),
        Err(error) => json!({"err": format!("{error:?}")}),
    }
}

fn restored_times(x: &UserMemoryExport) -> HashSet<String> {
    x.memory_events
        .iter()
        .map(|e| {
            serde_json::to_value(e.created_at)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn m(source: Uuid) -> Uuid {
    derived_import_fact_id(&ta(), source)
}

/// Which foreign state a world holds (TB's rows; TC always holds one fact).
#[derive(Clone, Copy, Debug)]
enum Foreign {
    /// W0: no row with any listed id.
    Absent,
    /// W1: TB holds the listed ids live.
    Live,
    /// W2: TB holds the listed ids soft-deleted.
    Deleted,
    /// V1 (owned-id cases): TB holds TA's contents under other ids.
    SameContent,
    /// IS19: TB holds a legacy row under M(TA, id) for each listed id.
    LegacyDerived,
}

const W012: [Foreign; 3] = [Foreign::Absent, Foreign::Live, Foreign::Deleted];

async fn world(foreign: Foreign, ids: &[Uuid]) -> World {
    let w = World::new();
    w.plant(
        &tb(),
        id("7b000000-0000-4000-8000-000000000001"),
        "TB unrelated canary fact.",
        false,
    )
    .await;
    w.plant(
        &tc(),
        id("7c000000-0000-4000-8000-000000000001"),
        "TC canary fact, same user id.",
        false,
    )
    .await;
    for (n, fid) in ids.iter().enumerate() {
        match foreign {
            Foreign::Absent => {}
            Foreign::Live => {
                w.plant(&tb(), *fid, &format!("TB private fact {n}."), false)
                    .await
            }
            Foreign::Deleted => {
                w.plant(&tb(), *fid, &format!("TB private fact {n}."), true)
                    .await
            }
            Foreign::SameContent => {
                let content = export_x()
                    .facts
                    .iter()
                    .find(|f| f.id == *fid)
                    .map(|f| f.content.clone())
                    .unwrap_or_else(|| format!("TB copy {n}."));
                w.plant(&tb(), Uuid::from_u128(0x7b00 + n as u128), &content, false)
                    .await;
            }
            Foreign::LegacyDerived => {
                w.plant(&tb(), m(*fid), &format!("TB legacy row {n}."), false)
                    .await
            }
        }
    }
    w
}

struct Case {
    trace: Value,
    first: MemcoreResult<ImportUserDataOutput>,
    second: MemcoreResult<ImportUserDataOutput>,
    second_writes: usize,
}

/// Runs the import twice in `w` and records Obs_A (amendment §8 item 8 included); asserts IS15
/// (TB and TC unchanged) and that no import consults a global id lookup.
async fn run(w: &World, x: &UserMemoryExport, mode: ImportMode, known: &HashSet<Uuid>) -> Case {
    let restored = restored_times(x);
    let all: HashSet<Uuid> = HashSet::new();
    let tb_before = w.observe(&tb(), &all, &restored).await;
    let tc_before = w.observe(&tc(), &all, &restored).await;
    w.probe.reset();
    let first = w.import(x, mode).await;
    let after_first = w.observe(&ta(), known, &restored).await;
    w.probe.reset();
    let second = w.import(x, mode).await;
    let second_writes = w.probe.writes();
    let after_second = w.observe(&ta(), known, &restored).await;
    assert_eq!(
        w.probe.count("fact_id_exists_in_any_tenant"),
        0,
        "no global lookup"
    );
    assert_eq!(
        w.observe(&tb(), &all, &restored).await,
        tb_before,
        "IS15: TB unchanged"
    );
    assert_eq!(
        w.observe(&tc(), &all, &restored).await,
        tc_before,
        "IS15: TC unchanged"
    );
    Case {
        trace: json!([outcome(&first), after_first, outcome(&second), after_second]),
        first,
        second,
        second_writes,
    }
}

fn known_of(x: &UserMemoryExport, extra: &[Uuid]) -> HashSet<Uuid> {
    x.facts
        .iter()
        .map(|f| f.id)
        .chain(extra.iter().copied())
        .collect()
}

fn assert_same(traces: &[(String, Value)]) {
    let (first_label, first) = &traces[0];
    for (label, trace) in &traces[1..] {
        assert_eq!(trace, first, "trace {label} differs from {first_label}");
    }
}

async fn ta_facts(w: &World) -> HashMap<Uuid, Fact> {
    w.facts
        .list_fact_states(&ta(), None, 1000)
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.fact.id, s.fact))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// IS1–IS3, IS8, IS11, IS16: non-owned ids in W0 / W1 / W2
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is1_is2_is3_non_owned_ids_are_noninterfering_and_references_follow() {
    let x = export_x();
    let ids: Vec<Uuid> = x.facts.iter().map(|f| f.id).collect();
    let mut traces = Vec::new();
    for foreign in W012 {
        let w = world(foreign, &ids).await;
        let case = run(&w, &x, ImportMode::Append, &known_of(&x, &[])).await;
        let out = case
            .first
            .as_ref()
            .expect("IS1-3: import succeeds in every world");
        assert_eq!(out.imported_facts, 3);
        assert_eq!(out.imported_events, 4);
        assert_eq!(out.imported_episodes, Some(2), "IS11: episodes as today");
        assert_eq!(out.imported_entities, Some(2));
        let facts = ta_facts(&w).await;
        let mut stored: Vec<Uuid> = facts.keys().copied().collect();
        stored.sort();
        let mut expected = vec![m(f_id()), m(g_id()), m(h_id())];
        expected.sort();
        assert_eq!(stored, expected, "{foreign:?}: stored under M(TA, f)");
        assert_eq!(
            facts[&m(f_id())].superseded_by,
            Some(m(g_id())),
            "superseded_by mapped"
        );
        let export = w
            .engine()
            .export_user_data(ExportUserDataInput {
                tenant: ta(),
                include_events: true,
                include_deleted: true,
            })
            .await
            .unwrap();
        let event_ids: Vec<Option<Uuid>> = export.memory_events.iter().map(|e| e.fact_id).collect();
        for expected in [Some(m(f_id())), Some(m(g_id())), None] {
            assert!(
                event_ids.contains(&expected),
                "restored event fact_id mapped: {expected:?}"
            );
        }
        assert!(
            !event_ids.contains(&Some(f_id())),
            "no event keeps the source id"
        );
        let layer = export.entities.expect("entities");
        let mut linked: Vec<Uuid> = layer.links.iter().map(|l| l.fact_id).collect();
        linked.sort();
        let mut want = vec![m(f_id()), m(g_id())];
        want.sort();
        assert_eq!(linked, want, "entity links mapped");
        assert_eq!(
            layer.relations[0].source_fact_id,
            Some(m(f_id())),
            "relation mapped"
        );
        for (fid, f) in &facts {
            let points = w
                .vectors
                .fact_vector_points(&ta(), *fid)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(points.len(), 1, "one point per fact");
            assert_eq!(points[0].content, f.content, "payload fact_id = stored id");
        }
        // IS8 / IS16: the identical second import is rejected before any write.
        let err = case.second.as_ref().expect_err("IS8: second import 409");
        assert!(err.is_import_already_applied(), "{foreign:?}: {err:?}");
        assert_eq!(case.second_writes, 0, "IS16: zero writes");
        traces.push((format!("{foreign:?}"), case.trace));
    }
    assert_same(&traces);
}

// ---------------------------------------------------------------------------------------------
// IS4–IS6: ids TA already owns (V0 / V1)
// ---------------------------------------------------------------------------------------------

async fn owned_world(foreign: Foreign, content: &str, deleted: bool) -> World {
    let w = world(foreign, &[f_id()]).await;
    w.plant(&ta(), f_id(), content, deleted).await;
    w
}

#[tokio::test]
async fn is4_owned_live_identical_is_already_applied() {
    let x = only(export_x(), &[f_id()]);
    let mut traces = Vec::new();
    for foreign in [Foreign::Absent, Foreign::SameContent] {
        let w = owned_world(foreign, F_TEXT, false).await;
        let restored = restored_times(&x);
        let before = w.observe(&ta(), &known_of(&x, &[]), &restored).await;
        let case = run(&w, &x, ImportMode::Append, &known_of(&x, &[])).await;
        let err = case.first.as_ref().expect_err("409");
        assert!(err.is_import_already_applied(), "{err:?}");
        assert_eq!(
            w.observe(&ta(), &known_of(&x, &[]), &restored).await,
            before,
            "zero writes"
        );
        traces.push((format!("{foreign:?}"), case.trace));
    }
    assert_same(&traces);
}

#[tokio::test]
async fn is5_owned_live_different_is_rekeyed_and_references_follow() {
    let x = only(export_x(), &[f_id()]);
    let mut traces = Vec::new();
    for foreign in [Foreign::Absent, Foreign::SameContent] {
        let w = owned_world(foreign, "Avery once led a cycling club.", false).await;
        let case = run(&w, &x, ImportMode::Append, &known_of(&x, &[])).await;
        assert_eq!(case.first.as_ref().expect("200").imported_facts, 1);
        let facts = ta_facts(&w).await;
        let copy = facts
            .values()
            .find(|f| f.id != f_id() && f.content == F_TEXT)
            .expect("re-keyed");
        assert_eq!(copy.id.get_version_num(), 4, "fresh v4 re-key");
        let export = w
            .engine()
            .export_user_data(ExportUserDataInput {
                tenant: ta(),
                include_events: true,
                include_deleted: true,
            })
            .await
            .unwrap();
        let first_copy_events = export
            .memory_events
            .iter()
            .filter(|e| e.fact_id == Some(f_id()))
            .count();
        assert_eq!(
            first_copy_events, 0,
            "events follow the re-key, not the owned row"
        );
        assert!(
            export
                .entities
                .unwrap()
                .links
                .iter()
                .all(|l| l.fact_id != f_id()),
            "links follow"
        );
        // Pre-existing (S2-accepted) behaviour: a live row with different content re-keys again.
        assert_eq!(
            case.second.as_ref().expect("re-key again").imported_facts,
            1
        );
        traces.push((format!("{foreign:?}"), case.trace));
    }
    assert_same(&traces);
}

#[tokio::test]
async fn is6_owned_deleted_is_id_conflict() {
    let x = only(export_x(), &[f_id()]);
    let mut traces = Vec::new();
    for foreign in [Foreign::Absent, Foreign::SameContent] {
        let w = owned_world(foreign, F_TEXT, true).await;
        let case = run(&w, &x, ImportMode::Append, &known_of(&x, &[])).await;
        let err = case.first.as_ref().expect_err("409");
        assert!(err.is_import_id_conflict(), "{err:?}");
        assert_eq!(w.probe.count("insert_fact"), 0);
        traces.push((format!("{foreign:?}"), case.trace));
    }
    assert_same(&traces);
}

// ---------------------------------------------------------------------------------------------
// IS7 / IS7b: DUPLICATE_FACT_ID before any write
// ---------------------------------------------------------------------------------------------

fn assert_duplicate(error: &MemcoreError) {
    match error {
        MemcoreError::ValidationError(message) => {
            assert!(message.starts_with(DUPLICATE_FACT_ID), "{message}");
            assert!(
                !message.contains(&f_id().to_string()),
                "no id in the message"
            );
            assert!(!message.contains(F_TEXT), "no content in the message");
        }
        other => panic!("expected VALIDATION_ERROR, got {other:?}"),
    }
}

#[tokio::test]
async fn is7_duplicate_source_ids_are_rejected_before_any_write() {
    let mut x = only(export_x(), &[f_id(), g_id()]);
    let mut twin = x.facts[0].clone();
    twin.content = "A second record with the first record's id.".to_string();
    x.facts.push(twin);
    assert!(x.completeness.is_none(), "legacy export, no manifest");
    let mut traces = Vec::new();
    for foreign in [Foreign::Absent, Foreign::Live] {
        let w = world(foreign, &[f_id()]).await;
        w.plant(
            &ta(),
            id("7a000000-0000-4000-8000-0000000000aa"),
            "TA prior fact kept.",
            false,
        )
        .await;
        let restored = restored_times(&x);
        let known = known_of(&x, &[]);
        let before = w.observe(&ta(), &known, &restored).await;
        let others = w.others().await;
        let mut trace = Vec::new();
        for mode in [ImportMode::Append, ImportMode::Replace] {
            w.probe.reset();
            let err = w.import(&x, mode).await.expect_err("400");
            assert_duplicate(&err);
            assert_eq!(w.probe.writes(), 0, "{mode:?}: zero writes");
            assert_eq!(
                w.observe(&ta(), &known, &restored).await,
                before,
                "{mode:?}: TA intact"
            );
            let dry = w
                .engine()
                .import_user_data(ImportUserDataInput {
                    tenant: ta(),
                    export: x.clone(),
                    mode,
                    restore_events: true,
                    dry_run: true,
                })
                .await
                .expect("dry run");
            assert!(
                dry.validation
                    .errors
                    .iter()
                    .any(|e| e.code == DUPLICATE_FACT_ID),
                "dry run lists the code"
            );
            trace.push(json!([
                format!("{err:?}"),
                serde_json::to_value(&dry).unwrap()
            ]));
        }
        assert_eq!(w.others().await, others, "IS15: TB and TC unchanged");
        traces.push((format!("{foreign:?}"), json!(trace)));
    }
    assert_same(&traces);
}

#[tokio::test]
async fn is7b_colliding_effective_ids_are_rejected_before_the_replace_deletion() {
    // X holds f (not owned) and M(TA, f) (owned by TA): both would be stored under M(TA, f).
    let mut x = only(export_x(), &[f_id()]);
    let mut derived = fact(&ta(), m(f_id()), "A fact stored under the derived id.", 14);
    derived.metadata = json!({});
    x.facts.push(derived);
    let w = world(Foreign::Absent, &[]).await;
    w.plant(
        &ta(),
        m(f_id()),
        "A fact stored under the derived id.",
        false,
    )
    .await;
    let restored = restored_times(&x);
    let known = known_of(&x, &[]);
    let before = w.observe(&ta(), &known, &restored).await;
    let others = w.others().await;
    for mode in [ImportMode::Append, ImportMode::Replace] {
        w.probe.reset();
        let err = w.import(&x, mode).await.expect_err("400");
        assert_duplicate(&err);
        assert_eq!(
            w.probe.writes(),
            0,
            "{mode:?}: zero writes (replace did not delete)"
        );
        assert_eq!(w.observe(&ta(), &known, &restored).await, before);
    }
    assert_eq!(w.others().await, others, "IS15: TB and TC unchanged");
}

// ---------------------------------------------------------------------------------------------
// IS9: partial import, retry, replace
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is9_partial_import_is_rejected_on_retry_and_replace_recovers() {
    let x = export_x();
    let ids: Vec<Uuid> = x.facts.iter().map(|f| f.id).collect();
    let w = world(Foreign::Live, &ids).await;
    let others = w.others().await;
    *w.probe.fail_upsert_at.lock().unwrap() = Some(2);
    w.import(&x, ImportMode::Append).await.expect_err("5xx");
    assert_eq!(w.probe.count("upsert_vector_fault_fired"), 1, "X1");
    *w.probe.fail_upsert_at.lock().unwrap() = None;
    let partial = ta_facts(&w).await;
    assert_eq!(partial.len(), 2);
    assert!(
        partial
            .keys()
            .all(|k| [m(f_id()), m(g_id()), m(h_id())].contains(k))
    );
    w.probe.reset();
    let err = w.import(&x, ImportMode::Append).await.expect_err("409");
    assert!(err.is_import_already_applied(), "{err:?}");
    assert_eq!(w.probe.writes(), 0, "retry: zero writes");
    let out = w
        .import(&x, ImportMode::Replace)
        .await
        .expect("replace recovers");
    assert_eq!(out.imported_facts, 3);
    let mut stored: Vec<Uuid> = ta_facts(&w).await.keys().copied().collect();
    stored.sort();
    let mut expected = vec![m(f_id()), m(g_id()), m(h_id())];
    expected.sort();
    assert_eq!(stored, expected, "exactly X under t()");
    assert_eq!(w.others().await, others, "IS15: TB and TC unchanged");
}

// ---------------------------------------------------------------------------------------------
// IS10: references to facts outside the import
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is10_references_outside_the_import_keep_their_treatment() {
    let owned_outside = id("7a000000-0000-4000-8000-0000000000bb");
    let absent = id("7a000000-0000-4000-8000-0000000000cc");
    let foreign_ref = id("7a000000-0000-4000-8000-0000000000dd");
    for (label, reference) in [
        ("a owned", owned_outside),
        ("b absent", absent),
        ("c foreign", foreign_ref),
    ] {
        let t = ta();
        let mut g = fact(&t, g_id(), G_TEXT, 11);
        g.superseded_by = Some(reference);
        g.superseded_at = Some(at(12));
        let mut x = export_x();
        x.facts = vec![g];
        x.memory_events = vec![event(&t, Some(reference), MemoryEventOperation::Add, 30)];
        if let Some(layer) = x.entities.as_mut() {
            layer.links = vec![link(&t, reference, layer.entities[0].id)];
            layer.relations[0].source_fact_id = Some(reference);
        }
        let worlds: Vec<Foreign> = if reference == foreign_ref {
            vec![Foreign::Absent, Foreign::Live]
        } else {
            vec![Foreign::Absent]
        };
        let mut traces = Vec::new();
        for foreign in worlds {
            let w = world(foreign, &[foreign_ref]).await;
            w.plant(&ta(), owned_outside, "TA fact outside the export.", false)
                .await;
            let case = run(&w, &x, ImportMode::Append, &known_of(&x, &[owned_outside])).await;
            case.first.as_ref().expect("200");
            let facts = ta_facts(&w).await;
            assert_eq!(
                facts[&m(g_id())].superseded_by,
                Some(reference),
                "{label}: kept as exported"
            );
            let export = w
                .engine()
                .export_user_data(ExportUserDataInput {
                    tenant: ta(),
                    include_events: true,
                    include_deleted: true,
                })
                .await
                .unwrap();
            assert_eq!(
                export.memory_events[0].fact_id,
                Some(reference),
                "{label}: event kept"
            );
            let layer = export.entities.unwrap();
            assert!(layer.links.is_empty(), "{label}: link dropped");
            assert_eq!(
                layer.relations[0].source_fact_id, None,
                "{label}: relation source None"
            );
            traces.push((format!("{label} {foreign:?}"), case.trace));
        }
        assert_same(&traces);
    }
}

// ---------------------------------------------------------------------------------------------
// IS14: replace with owned and non-owned ids
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is14_replace_plans_ownership_before_the_deletion() {
    let x = only(export_x(), &[f_id()]);
    for (label, deleted) in [("a owned live", false), ("b owned deleted", true)] {
        let mut traces = Vec::new();
        for foreign in [Foreign::Absent, Foreign::SameContent] {
            let w = owned_world(foreign, F_TEXT, deleted).await;
            let case = run(&w, &x, ImportMode::Replace, &known_of(&x, &[])).await;
            case.first.as_ref().expect("200");
            let facts = ta_facts(&w).await;
            assert_eq!(
                facts.keys().copied().collect::<Vec<_>>(),
                vec![f_id()],
                "{label}: f kept"
            );
            traces.push((format!("{label} {foreign:?}"), case.trace));
        }
        assert_same(&traces);
    }
    let mut traces = Vec::new();
    for foreign in W012 {
        let w = world(foreign, &[f_id()]).await;
        let case = run(&w, &x, ImportMode::Replace, &known_of(&x, &[])).await;
        case.first.as_ref().expect("200");
        assert_eq!(
            ta_facts(&w).await.keys().copied().collect::<Vec<_>>(),
            vec![m(f_id())],
            "c: M(TA, f)"
        );
        traces.push((format!("c {foreign:?}"), case.trace));
    }
    assert_same(&traces);
}

// ---------------------------------------------------------------------------------------------
// IS17: a model of approach A (foreign-only re-key) is not noninterfering
// ---------------------------------------------------------------------------------------------

/// Test-only model of approach A: keep the source id unless ANY tenant holds it.
async fn approach_a_id(w: &World, source: Uuid) -> Uuid {
    let owned = w
        .facts
        .get_fact_including_deleted(&ta(), source)
        .await
        .unwrap()
        .is_some();
    if !owned && w.facts.fact_id_exists_in_any_tenant(source).await.unwrap() {
        Uuid::new_v4()
    } else {
        source
    }
}

#[tokio::test]
async fn is17_foreign_only_rekey_would_leak_through_the_stored_id() {
    let w0 = world(Foreign::Absent, &[f_id()]).await;
    let w1 = world(Foreign::Live, &[f_id()]).await;
    let (others0, others1) = (w0.others().await, w1.others().await);
    let a0 = approach_a_id(&w0, f_id()).await;
    let a1 = approach_a_id(&w1, f_id()).await;
    assert_eq!(a0, f_id(), "A keeps the id in W0");
    assert_ne!(
        a1,
        f_id(),
        "A changes the id in W1: TA learns that f exists"
    );
    // C+ (the product) stores the same id in both worlds.
    let x = only(export_x(), &[f_id()]);
    w0.import(&x, ImportMode::Append).await.expect("W0");
    w1.import(&x, ImportMode::Append).await.expect("W1");
    assert_eq!(
        ta_facts(&w0).await.keys().collect::<Vec<_>>(),
        vec![&m(f_id())]
    );
    assert_eq!(
        ta_facts(&w1).await.keys().collect::<Vec<_>>(),
        vec![&m(f_id())]
    );
    assert_eq!(w0.others().await, others0, "IS15 (W0)");
    assert_eq!(w1.others().await, others1, "IS15 (W1)");
}

// ---------------------------------------------------------------------------------------------
// IS18: deterministic interleaving of two identical imports
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is18_interleaved_identical_imports_store_each_fact_once() {
    let x = export_x();
    let w = Arc::new(world(Foreign::Live, &[f_id()]).await);
    let others = w.others().await;
    let held = Arc::new(Probe::default());
    let (reached, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *held.gate.lock().unwrap() = Some((reached.clone(), release.clone()));
    let first = {
        let engine = w.engine_with(held.clone());
        let x = x.clone();
        tokio::spawn(async move {
            engine
                .import_user_data(ImportUserDataInput {
                    tenant: ta(),
                    export: x,
                    mode: ImportMode::Append,
                    restore_events: true,
                    dry_run: false,
                })
                .await
        })
    };
    reached.notified().await; // import 1 passed its preflight and holds at its first insert
    let second = w
        .import(&x, ImportMode::Append)
        .await
        .expect("import 2 completes");
    assert_eq!(second.imported_facts, 3);
    release.notify_one();
    let first = first.await.expect("join");
    let err = first.expect_err("import 1 fails at the global key");
    eprintln!("IS18 import 1 error class (mock): {err:?}");
    let facts = ta_facts(&w).await;
    assert_eq!(facts.len(), 3, "exactly one fact per M(TA, ·)");
    for source in [f_id(), g_id(), h_id()] {
        assert!(facts.contains_key(&m(source)));
    }
    assert_eq!(w.others().await, others, "IS15: TB and TC unchanged");
}

// ---------------------------------------------------------------------------------------------
// IS19: legacy row of TB under M(TA, f) — known limitation, recorded
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is19_legacy_derived_id_collision_is_recorded_as_a_limitation() {
    // TB's planted row stands for a pre-fix import that stored an arbitrary id equal to M(TA, f).
    let x = only(export_x(), &[g_id(), f_id()]);
    let mut x = x;
    x.facts.sort_by_key(|f| if f.id == g_id() { 0 } else { 1 });
    let w0 = world(Foreign::Absent, &[f_id()]).await;
    let wl = world(Foreign::LegacyDerived, &[f_id()]).await;
    let c0 = run(&w0, &x, ImportMode::Append, &known_of(&x, &[])).await;
    let cl = run(&wl, &x, ImportMode::Append, &known_of(&x, &[])).await; // asserts TB unchanged
    c0.first.as_ref().expect("W0 succeeds");
    let err = cl
        .first
        .as_ref()
        .expect_err("the legacy row collides at the global key");
    assert!(
        matches!(err, MemcoreError::Conflict(_)),
        "mock error class: {err:?}"
    );
    let partial = ta_facts(&wl).await;
    assert_eq!(
        partial.keys().collect::<Vec<_>>(),
        vec![&m(g_id())],
        "facts before f stay (partial)"
    );
    assert_ne!(
        c0.trace, cl.trace,
        "the signal remains: LEGACY-LIMITATION (observed)"
    );
    eprintln!(
        "IS19 LEGACY-LIMITATION (observed, mock): first={err:?}; retry={:?}",
        cl.second.as_ref().err()
    );
}

// ---------------------------------------------------------------------------------------------
// IS20: export -> forget -> import cycles drift deterministically
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn is20_repeated_restore_cycles_derive_again() {
    let w = world(Foreign::Absent, &[]).await;
    w.plant(&ta(), f_id(), F_TEXT, false).await;
    let others = w.others().await;
    let mut expected = f_id();
    for cycle in 1..=2 {
        let x = w
            .engine()
            .export_user_data(ExportUserDataInput {
                tenant: ta(),
                include_events: false,
                include_deleted: false,
            })
            .await
            .unwrap();
        assert_eq!(x.facts[0].id, expected);
        w.engine()
            .forget_user(ForgetUserInput { tenant: ta() })
            .await
            .unwrap();
        w.import(&x, ImportMode::Append).await.expect("restore");
        expected = derived_import_fact_id(&ta(), expected);
        let facts = ta_facts(&w).await;
        assert_eq!(
            facts.keys().collect::<Vec<_>>(),
            vec![&expected],
            "cycle {cycle}"
        );
        assert_eq!(facts[&expected].content, F_TEXT, "content preserved");
    }
    assert_eq!(expected, m(m(f_id())), "f -> M(TA,f) -> M(TA, M(TA,f))");
    assert_eq!(w.others().await, others, "IS15: TB and TC unchanged");
}
