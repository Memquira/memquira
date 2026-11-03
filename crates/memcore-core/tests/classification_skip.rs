//! `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED`: on the legacy ingest
//! path a candidate whose related-fact lookup returns no fact is added without the classification
//! call; with related facts the classifier runs exactly as before. Every scenario runs the same
//! input with the setting off (the previous path) and on, and compares what was stored.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::observability::CLASSIFICATION_SKIPPED_METRIC;
use memcore_core::ports::{
    FactClassificationInput, FactExtractionInput, LlmProvider, MemoryEventQuery, SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, EmbeddingDeduplicationConfig, Fact,
    FactExtractionOutput, FactOperation, FactOperationDecision, FactSearchQuery, FactStore,
    MemoryEngine, MemoryEvent, MemoryEventStore, MemoryMessage, MemoryType, MessageRole,
    TenantContext,
};
use memcore_providers::MockEmbeddingProvider;
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use metrics::{
    Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use serde_json::{Value, json};

type Decide = fn(&FactClassificationInput) -> MemcoreResult<FactOperationDecision>;

/// Extraction returns the next scripted candidate list; classification is decided by `decide` and
/// every call's related-fact count is recorded.
struct ScriptedLlm {
    extractions: Mutex<Vec<Vec<CandidateFact>>>,
    decide: Decide,
    classify_calls: Mutex<Vec<usize>>,
}

impl ScriptedLlm {
    fn new(extractions: Vec<Vec<CandidateFact>>, decide: Decide) -> Arc<Self> {
        Arc::new(Self {
            extractions: Mutex::new(extractions),
            decide,
            classify_calls: Mutex::new(Vec::new()),
        })
    }

    fn classify_calls(&self) -> Vec<usize> {
        self.classify_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn extract_facts(
        &self,
        _input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        let mut queue = self.extractions.lock().unwrap();
        let next = if queue.is_empty() {
            Vec::new()
        } else {
            queue.remove(0)
        };
        Ok(FactExtractionOutput::from_candidates(next))
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.classify_calls
            .lock()
            .unwrap()
            .push(input.existing_facts.len());
        (self.decide)(&input)
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        Ok(input
            .facts
            .iter()
            .map(|f| f.content.clone())
            .collect::<Vec<_>>()
            .join(" "))
    }
}

fn add_decision(_: &FactClassificationInput) -> MemcoreResult<FactOperationDecision> {
    Ok(decision(FactOperation::Add, None))
}

fn update_first(input: &FactClassificationInput) -> MemcoreResult<FactOperationDecision> {
    Ok(match input.existing_facts.first() {
        Some(fact) => decision(FactOperation::Update, Some(fact)),
        None => decision(FactOperation::Add, None),
    })
}

fn delete_first(input: &FactClassificationInput) -> MemcoreResult<FactOperationDecision> {
    Ok(match input.existing_facts.first() {
        Some(fact) => decision(FactOperation::Delete, Some(fact)),
        None => decision(FactOperation::Add, None),
    })
}

fn noop_with_related(input: &FactClassificationInput) -> MemcoreResult<FactOperationDecision> {
    Ok(match input.existing_facts.first() {
        Some(fact) => decision(FactOperation::NoOp, Some(fact)),
        None => decision(FactOperation::Add, None),
    })
}

fn failing(_: &FactClassificationInput) -> MemcoreResult<FactOperationDecision> {
    Err(MemcoreError::ProviderError(
        "classification unavailable".to_string(),
    ))
}

fn decision(operation: FactOperation, target: Option<&Fact>) -> FactOperationDecision {
    FactOperationDecision {
        operation,
        target_fact_id: target.map(|f| f.id),
        reason: Some("scripted".to_string()),
        confidence: 0.9,
    }
}

fn tenant(org_id: &str, user_id: &str) -> TenantContext {
    TenantContext::new(org_id, user_id).unwrap()
}

fn candidate(content: &str) -> CandidateFact {
    CandidateFact::new(
        content,
        MemoryType::Event,
        0.9,
        0.8,
        Some(Utc.with_ymd_and_hms(2022, 4, 3, 14, 13, 0).unwrap()),
        json!({ "session_id": "session_4" }),
    )
    .unwrap()
}

/// One store set and one engine; `skip` is the S1 setting.
struct World {
    facts: Arc<MockFactStore>,
    events: Arc<MockMemoryEventStore>,
    llm: Arc<ScriptedLlm>,
    engine: MemoryEngine,
}

impl World {
    fn new(skip: bool, extractions: Vec<Vec<CandidateFact>>, decide: Decide) -> Self {
        let facts = Arc::new(MockFactStore::new());
        let events = Arc::new(MockMemoryEventStore::new());
        let llm = ScriptedLlm::new(extractions, decide);
        let engine = MemoryEngine::new(
            facts.clone(),
            Arc::new(MockVectorStore::new()),
            llm.clone(),
            Arc::new(MockEmbeddingProvider::new(8)),
        )
        // The deterministic mock embeddings are not semantic; embedding dedup is off so that the
        // related-fact path, not the embedding check, decides these scenarios.
        .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
            enabled: false,
            ..Default::default()
        })
        .with_event_store(events.clone())
        .with_audit_provider_info(Some("mock".to_string()), Some("mock-llm".to_string()))
        .with_classification_skip_when_no_related(skip);
        Self {
            facts,
            events,
            llm,
            engine,
        }
    }

    async fn add(&self, tenant: &TenantContext) -> MemcoreResult<AddMemoryOutput> {
        self.engine
            .add_memory(AddMemoryInput {
                tenant: tenant.clone(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "James told me about the tournament.".to_string(),
                    ..Default::default()
                }],
                metadata: json!({ "conversation_index": 6, "session_id": "session_4" }),
                session: Default::default(),
            })
            .await
    }

    /// Every fact of `tenant`, deleted ones included, without the generated id and wall-clock times.
    async fn stored(&self, tenant: &TenantContext) -> Vec<Value> {
        let mut facts = self
            .facts
            .search_facts(FactSearchQuery {
                tenant: tenant.clone(),
                memory_types: None,
                query_text: None,
                limit: 100,
                cursor: None,
                include_deleted: true,
            })
            .await
            .unwrap();
        facts.sort_by(|a, b| a.content.cmp(&b.content));
        facts.iter().map(normalized_fact).collect()
    }

    async fn events(&self, tenant: &TenantContext) -> Vec<Value> {
        let mut events = self
            .events
            .list_events(MemoryEventQuery::new(tenant.clone(), 100))
            .await
            .unwrap();
        events.sort_by_key(|e| e.created_at);
        events.iter().map(normalized_event).collect()
    }
}

fn normalized_fact(fact: &Fact) -> Value {
    let mut value = serde_json::to_value(fact).unwrap();
    let object = value.as_object_mut().unwrap();
    for generated in [
        "id",
        "recorded_at",
        "updated_at",
        "superseded_at",
        "superseded_by",
    ] {
        object.remove(generated);
    }
    value
}

fn normalized_event(event: &MemoryEvent) -> Value {
    json!({
        "operation": format!("{:?}", event.operation),
        "has_fact_id": event.fact_id.is_some(),
        "input_text": event.input_text,
        "previous_content": event.previous_content,
        "new_content": event.new_content,
        "provider_name": event.provider_name,
        "model_name": event.model_name,
        "metadata": strip_ids(event.metadata.clone()),
    })
}

fn strip_ids(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("target_fact_id");
    }
    value
}

fn summary(output: &AddMemoryOutput) -> Value {
    json!({
        "extracted": output.extracted,
        "added": output.added,
        "updated": output.updated,
        "deleted": output.deleted,
        "invalidated": output.invalidated,
        "noop": output.noop,
        "discarded": format!("{:?}", output.discarded),
        "memories": output.memories.iter().map(normalized_fact).collect::<Vec<_>>(),
    })
}

// --- metrics capture (thread-local recorder; `#[tokio::test]` runs on one thread) -------------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<BTreeMap<String, u64>>>);

impl Captured {
    fn skipped(&self) -> u64 {
        self.0
            .lock()
            .unwrap()
            .get(CLASSIFICATION_SKIPPED_METRIC)
            .copied()
            .unwrap_or(0)
    }
}

struct CounterHandle(String, Captured);

impl CounterFn for CounterHandle {
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
        let name = if key.labels().next().is_none() {
            key.name().to_string()
        } else {
            format!("{key:?}")
        };
        Counter::from_arc(Arc::new(CounterHandle(name, self.0.clone())))
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

// --- scenarios ----------------------------------------------------------------------------------

#[tokio::test]
async fn empty_related_set_skips_the_call_and_stores_the_same_fact_and_event() {
    let user = tenant("org_a", "u1");
    let script = || vec![vec![candidate("James joined the tournament.")]];
    let off = World::new(false, script(), add_decision);
    let on = World::new(true, script(), add_decision);

    let captured = Captured::default();
    let recorder = TestRecorder(captured.clone());
    let _guard = metrics::set_default_local_recorder(&recorder);

    let off_out = off.add(&user).await.unwrap();
    assert_eq!(captured.skipped(), 0, "the previous path never skips");
    let on_out = on.add(&user).await.unwrap();
    assert_eq!(captured.skipped(), 1, "one skipped call is counted");

    assert_eq!(
        off.llm.classify_calls(),
        vec![0],
        "previous path: one call, no related fact"
    );
    assert!(
        on.llm.classify_calls().is_empty(),
        "S1: no classification call"
    );
    assert_eq!(summary(&off_out), summary(&on_out));
    assert_eq!(on_out.added, 1);
    assert_eq!(off.stored(&user).await, on.stored(&user).await);
    assert_eq!(off.events(&user).await, on.events(&user).await);
}

#[tokio::test]
async fn related_facts_keep_the_classifier_for_update_delete_noop_and_add() {
    let user = tenant("org_a", "u1");
    // The second candidate is contained in the first fact's text, so the related-fact lookup
    // (LIKE '%candidate%') returns that fact and the classifier must decide.
    let script = || {
        vec![
            vec![candidate("On 3 April 2022, James joined the tournament.")],
            vec![candidate("James joined the tournament.")],
        ]
    };
    for (name, decide, expected) in [
        ("update", update_first as Decide, (0, 1, 0)),
        ("delete", delete_first as Decide, (0, 0, 1)),
        ("noop", noop_with_related as Decide, (0, 0, 0)),
        ("add", add_decision as Decide, (1, 0, 0)),
    ] {
        let off = World::new(false, script(), decide);
        let on = World::new(true, script(), decide);
        for world in [&off, &on] {
            world.add(&user).await.unwrap();
        }
        let off_second = off.add(&user).await.unwrap();
        let on_second = on.add(&user).await.unwrap();

        assert_eq!(
            off.llm.classify_calls(),
            vec![0, 1],
            "{name}: previous path"
        );
        assert_eq!(
            on.llm.classify_calls(),
            vec![1],
            "{name}: S1 calls only with a related fact"
        );
        assert_eq!(
            (on_second.added, on_second.updated, on_second.deleted),
            expected,
            "{name}"
        );
        assert_eq!(summary(&off_second), summary(&on_second), "{name}");
        assert_eq!(off.stored(&user).await, on.stored(&user).await, "{name}");
        assert_eq!(off.events(&user).await, on.events(&user).await, "{name}");
    }
}

#[tokio::test]
async fn another_tenants_fact_never_counts_as_related() {
    let owner = tenant("org_a", "u1");
    let other = tenant("org_a", "u2");
    let script = || {
        vec![
            vec![candidate("On 3 April 2022, James joined the tournament.")],
            vec![candidate("James joined the tournament.")],
        ]
    };
    let off = World::new(false, script(), update_first);
    let on = World::new(true, script(), update_first);
    for world in [&off, &on] {
        world.add(&owner).await.unwrap();
        world.add(&other).await.unwrap();
    }

    // The other tenant sees no related fact: S1 skips, the previous path calls with none.
    assert_eq!(off.llm.classify_calls(), vec![0, 0]);
    assert!(on.llm.classify_calls().is_empty());
    for user in [&owner, &other] {
        assert_eq!(off.stored(user).await, on.stored(user).await);
        assert_eq!(off.events(user).await, on.events(user).await);
    }
    assert_eq!(
        on.stored(&owner).await.len(),
        1,
        "the owner's fact is untouched"
    );
}

#[tokio::test]
async fn deterministic_dedup_still_runs_before_classification() {
    let user = tenant("org_a", "u1");
    let script = || {
        vec![
            vec![candidate("James joined the tournament.")],
            vec![candidate("james joined the tournament.")],
        ]
    };
    let off = World::new(false, script(), add_decision);
    let on = World::new(true, script(), add_decision);
    for world in [&off, &on] {
        world.add(&user).await.unwrap();
        let retry = world.add(&user).await.unwrap();
        assert_eq!((retry.added, retry.discarded.duplicate_content), (0, 1));
    }
    assert_eq!(off.llm.classify_calls(), vec![0]);
    assert!(on.llm.classify_calls().is_empty());
    assert_eq!(off.stored(&user).await, on.stored(&user).await);
    assert_eq!(off.events(&user).await, on.events(&user).await);
}

#[tokio::test]
async fn a_classifier_failure_fails_only_the_calls_that_are_still_made() {
    let user = tenant("org_a", "u1");

    // No related fact: the previous path fails on the provider error; S1 makes no call.
    let off = World::new(
        false,
        vec![vec![candidate("James joined the tournament.")]],
        failing,
    );
    let on = World::new(
        true,
        vec![vec![candidate("James joined the tournament.")]],
        failing,
    );
    assert!(off.add(&user).await.is_err());
    assert_eq!(on.add(&user).await.unwrap().added, 1);

    // With a related fact the call is made and its failure fails the request, as before.
    let on = World::new(
        true,
        vec![
            vec![candidate("On 3 April 2022, James joined the tournament.")],
            vec![candidate("James joined the tournament.")],
        ],
        failing,
    );
    assert_eq!(on.add(&user).await.unwrap().added, 1);
    assert!(on.add(&user).await.is_err());
    assert_eq!(on.llm.classify_calls(), vec![1]);
}
