//! Consolidation v2 (`MEMCORE_CONSOLIDATION_MODE=llm_v2`, design 3.2) against mock stores
//! and a scripted mock model. Every name and fact here is invented for these tests.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use memcore_core::ports::{
    ConsolidationDecision, ConsolidationInput, ConsolidationOperation, MemoryEventQuery,
};
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, BuildContextInput, CandidateFact, ConsolidationConfig, Fact,
    FactStore, FactStructure, MemoryEngine, MemoryEvent, MemoryEventOperation, MemoryEventStore,
    MemoryMessage, MemoryType, MessageRole, SearchMemoryInput, SearchOptions, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

struct World {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    events: Arc<MockMemoryEventStore>,
    tenant: TenantContext,
}

impl World {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
            events: Arc::new(MockMemoryEventStore::new()),
            tenant: TenantContext::new("org_c", "user_c").unwrap(),
        }
    }

    async fn add(&self, engine: &MemoryEngine, tenant: &TenantContext) -> AddMemoryOutput {
        engine
            .add_memory(AddMemoryInput {
                tenant: tenant.clone(),
                messages: vec![MemoryMessage::new(MessageRole::User, "a message")],
                metadata: json!({ "client": "test" }),
                session: Default::default(),
            })
            .await
            .expect("add_memory")
    }

    async fn all_facts(&self, tenant: &TenantContext) -> Vec<Fact> {
        self.facts
            .search_facts(memcore_core::FactSearchQuery::new(tenant.clone(), 100))
            .await
            .unwrap()
    }

    async fn events(&self) -> Vec<MemoryEvent> {
        let mut events = self
            .events
            .list_events(MemoryEventQuery::new(self.tenant.clone(), 100))
            .await
            .unwrap();
        events.sort_by_key(|event| event.created_at);
        events
    }
}

fn candidate(content: &str, subject: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some(subject.to_string()),
            ..FactStructure::default()
        })
}

/// `content` stated at `observed`, with its time expression settled the way extraction does.
fn with_time(
    content: &str,
    expression: &str,
    observed: Option<chrono::DateTime<Utc>>,
) -> CandidateFact {
    let mut c = candidate(content, "Ravi");
    c.structure.observed_at = observed;
    c.structure.event_time_text = Some(expression.to_string());
    c.structure.event_time =
        memcore_core::temporal::settle_event_time(None, Some(expression), observed).0;
    assert!(c.structure.event_time.is_some(), "{expression} resolves");
    c
}

fn llm(candidates: Vec<CandidateFact>) -> MockLlmProvider {
    MockLlmProvider::new().with_extraction_candidates(candidates)
}

fn scripted(
    candidates: Vec<CandidateFact>,
    decide: impl Fn(&ConsolidationInput) -> ConsolidationDecision + Send + Sync + 'static,
) -> Arc<MockLlmProvider> {
    Arc::new(llm(candidates).with_consolidation_decider(Arc::new(decide)))
}

fn engine_with(
    world: &World,
    llm: Arc<MockLlmProvider>,
    config: ConsolidationConfig,
) -> MemoryEngine {
    MemoryEngine::new(
        world.facts.clone(),
        world.vectors.clone(),
        llm,
        Arc::new(MockEmbeddingProvider::new(64)),
    )
    .with_event_store(world.events.clone())
    .with_min_importance(0.0)
    .with_consolidation(Some(config))
}

/// Every candidate with any vector similarity goes to the model.
fn always_ask() -> ConsolidationConfig {
    ConsolidationConfig {
        candidates: 10,
        similarity_floor: 0.0,
    }
}

fn decision(
    operation: ConsolidationOperation,
    target: Option<Uuid>,
    merged: Option<&str>,
) -> ConsolidationDecision {
    ConsolidationDecision {
        operation,
        target_fact_id: target,
        merged_content: merged.map(str::to_string),
        invalid_at: None,
        invalid_at_unparsed: None,
        reason: Some("scripted".to_string()),
    }
}

async fn search(
    engine: &MemoryEngine,
    tenant: &TenantContext,
    include_history: bool,
) -> Vec<String> {
    search_for(engine, tenant, "Ravi lives", include_history).await
}

async fn search_for(
    engine: &MemoryEngine,
    tenant: &TenantContext,
    query: &str,
    include_history: bool,
) -> Vec<String> {
    engine
        .search_memory_with_options(
            SearchMemoryInput {
                tenant: tenant.clone(),
                query: query.to_string(),
                limit: 20,
                memory_types: None,
                metadata_filter: None,
            },
            SearchOptions {
                include_history,
                ..SearchOptions::default()
            },
        )
        .await
        .unwrap()
        .results
        .into_iter()
        .map(|result| result.content)
        .collect()
}

#[tokio::test]
async fn legacy_default_never_asks_for_a_v2_decision_and_hides_nothing() {
    let world = World::new();
    let model = Arc::new(llm(vec![candidate("Ravi lives in Pune.", "Ravi")]));
    let engine = MemoryEngine::new(
        world.facts.clone(),
        world.vectors.clone(),
        model.clone(),
        Arc::new(MockEmbeddingProvider::new(64)),
    )
    .with_min_importance(0.0);
    assert!(engine.consolidation_config().is_none());
    world.add(&engine, &world.tenant).await;
    assert!(model.consolidation_inputs().is_empty());

    // A fact carrying invalid_at or superseded_at (e.g. imported) stays visible in legacy mode.
    let mut fact = world.all_facts(&world.tenant).await.remove(0);
    fact.invalid_at = Some(Utc::now());
    fact.superseded_at = Some(Utc::now());
    world.facts.update_fact(&world.tenant, fact).await.unwrap();
    assert_eq!(search(&engine, &world.tenant, false).await.len(), 1);
}

#[tokio::test]
async fn an_exact_duplicate_is_a_noop_without_a_model_call() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi grows orchids.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let second = scripted(vec![candidate("ravi grows orchids", "RAVI")], |_| {
        panic!("the model must not be asked")
    });
    let output = world
        .add(
            &engine_with(&world, second.clone(), always_ask()),
            &world.tenant,
        )
        .await;

    assert_eq!((output.added, output.noop), (0, 1));
    assert_eq!(output.discarded.duplicate_content, 1);
    assert!(second.consolidation_inputs().is_empty());
    let noop = world.events().await.pop().unwrap();
    assert_eq!(noop.operation, MemoryEventOperation::NoOp);
    assert_eq!(noop.metadata["path"], "fast_noop");
}

#[tokio::test]
async fn nothing_related_enough_is_added_without_a_model_call() {
    let world = World::new();
    let floor_one = ConsolidationConfig {
        candidates: 10,
        similarity_floor: 1.0,
    };
    let first = scripted(
        vec![candidate("Ravi grows orchids.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, floor_one), &world.tenant)
        .await;
    let second = scripted(vec![candidate("Ravi collects old maps.", "Ravi")], |_| {
        panic!("the model must not be asked")
    });
    let output = world
        .add(
            &engine_with(&world, second.clone(), floor_one),
            &world.tenant,
        )
        .await;

    assert_eq!(output.added, 1);
    assert!(second.consolidation_inputs().is_empty());
    assert_eq!(world.all_facts(&world.tenant).await.len(), 2);
    assert_eq!(
        world.events().await.pop().unwrap().metadata["consolidation"]["path"],
        "fast_add"
    );
}

#[tokio::test]
async fn a_model_update_rewrites_the_target_and_keeps_its_id() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi grows orchids.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let original = world.all_facts(&world.tenant).await.remove(0);

    let second = scripted(
        vec![candidate("Ravi grows twelve orchid species.", "Ravi")],
        |input| {
            decision(
                ConsolidationOperation::Update,
                Some(input.existing_facts[0].id),
                Some("Ravi grows twelve orchid species in a greenhouse."),
            )
        },
    );
    let output = world
        .add(
            &engine_with(&world, second.clone(), always_ask()),
            &world.tenant,
        )
        .await;

    assert_eq!((output.added, output.updated), (0, 1));
    let facts = world.all_facts(&world.tenant).await;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].id, original.id);
    assert_eq!(
        facts[0].content,
        "Ravi grows twelve orchid species in a greenhouse."
    );
    let offered = &second.consolidation_inputs()[0];
    assert_eq!(offered.existing_facts[0].id, original.id);
    let update = world.events().await.pop().unwrap();
    assert_eq!(update.operation, MemoryEventOperation::Update);
    assert_eq!(update.metadata["consolidation"]["path"], "model");
    assert_eq!(update.metadata["client"], "test");
}

#[tokio::test]
async fn a_model_invalidation_keeps_history_and_adds_the_new_fact() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi lives in Pune.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let old = world.all_facts(&world.tenant).await.remove(0);
    let changed = Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();

    // The source resolves "March 2024" (a window); the model's date lies inside it.
    let second = scripted(
        vec![with_time(
            "Ravi moved to Mysore in March 2024.",
            "March 2024",
            None,
        )],
        move |input| ConsolidationDecision {
            invalid_at: Some(changed),
            invalid_at_unparsed: None,
            ..decision(
                ConsolidationOperation::Invalidate,
                Some(input.existing_facts[0].id),
                None,
            )
        },
    );
    let engine = engine_with(&world, second, always_ask());
    let output = world.add(&engine, &world.tenant).await;

    assert_eq!(
        (output.added, output.invalidated, output.updated),
        (1, 1, 0)
    );
    let old_now = world
        .facts
        .get_fact(&world.tenant, old.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_now.invalid_at, Some(changed));
    assert!(old_now.superseded_at.is_some());
    assert!(!old_now.is_active());
    assert_eq!(old_now.content, old.content);
    let facts = world.all_facts(&world.tenant).await;
    assert_eq!(facts.len(), 2, "never deleted");
    let replacement = facts.iter().find(|fact| fact.id != old.id).unwrap();
    assert!(replacement.is_active());
    assert_eq!(old_now.superseded_by, Some(replacement.id));

    let events = world.events().await;
    let invalidate = events
        .iter()
        .find(|event| event.operation == MemoryEventOperation::Invalidate)
        .unwrap();
    assert_eq!(invalidate.fact_id, Some(old.id));
    assert_eq!(
        invalidate.metadata["invalid_at_source"],
        "model_within_source_window"
    );
    assert_eq!(invalidate.metadata["invalid_at"], json!(changed));
    assert_eq!(
        invalidate.metadata["superseded_at"],
        json!(old_now.superseded_at)
    );
    assert_eq!(invalidate.metadata["superseded_by"], json!(replacement.id));
    assert!(invalidate.metadata.get("invalid_at_rejected").is_none());
    let add = events
        .iter()
        .find(|event| {
            event.operation == MemoryEventOperation::Add && event.fact_id == Some(replacement.id)
        })
        .unwrap();
    assert_eq!(
        add.metadata["consolidation"]["superseded_fact_id"],
        json!(old.id)
    );

    // History: left out by default under v2, returned on request (search and context).
    let default = search(&engine, &world.tenant, false).await;
    assert!(
        default.iter().all(|content| !content.contains("Pune")),
        "{default:?}"
    );
    let history = search(&engine, &world.tenant, true).await;
    assert!(
        history.iter().any(|content| content.contains("Pune")),
        "{history:?}"
    );
    let context = |include_history| BuildContextInput {
        tenant: world.tenant.clone(),
        query: "Ravi lives".to_string(),
        include_history,
        ..BuildContextInput::default()
    };
    let without = engine.build_context(context(false)).await.unwrap().context;
    let with = engine.build_context(context(true)).await.unwrap().context;
    assert!(
        !without.contains("Pune") && with.contains("Pune"),
        "{without} / {with}"
    );
}

/// Adds "Ravi swims daily.", then `newer` with the scripted model invalidating it. Returns the old
/// fact as stored afterwards, the replacement, and the invalidate event.
async fn supersede_with(
    world: &World,
    newer: CandidateFact,
    decide: impl Fn(&ConsolidationInput) -> ConsolidationDecision + Send + Sync + 'static,
) -> (Fact, Fact, MemoryEvent, MemoryEngine) {
    let first = scripted(
        vec![candidate("Ravi swims daily.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(world, first, always_ask()), &world.tenant)
        .await;
    let old = world.all_facts(&world.tenant).await.remove(0);
    let engine = engine_with(world, scripted(vec![newer], decide), always_ask());
    let output = world.add(&engine, &world.tenant).await;
    assert_eq!((output.added, output.invalidated), (1, 1));
    let old_now = world
        .facts
        .get_fact(&world.tenant, old.id)
        .await
        .unwrap()
        .unwrap();
    let replacement = world
        .all_facts(&world.tenant)
        .await
        .into_iter()
        .find(|fact| fact.id != old.id)
        .unwrap();
    let invalidate = world
        .events()
        .await
        .into_iter()
        .find(|event| event.operation == MemoryEventOperation::Invalidate)
        .unwrap();
    (old_now, replacement, invalidate, engine)
}

fn invalidate_first(input: &ConsolidationInput) -> ConsolidationDecision {
    decision(
        ConsolidationOperation::Invalidate,
        Some(input.existing_facts[0].id),
        None,
    )
}

/// With no source-supported change time, the old fact is superseded with
/// `invalid_at` unset, is left out of search and context, and `observed_at` is not copied.
#[tokio::test]
async fn an_unknown_change_time_supersedes_without_invalid_at() {
    let world = World::new();
    let stated = Utc.with_ymd_and_hms(2025, 5, 6, 9, 0, 0).unwrap();
    let mut newer = candidate("Ravi stopped swimming.", "Ravi");
    newer.structure.observed_at = Some(stated);
    let before = Utc::now();
    let (old, replacement, invalidate, engine) =
        supersede_with(&world, newer, invalidate_first).await;

    assert_eq!(old.invalid_at, None, "observed_at is not copied");
    let superseded_at = old.superseded_at.expect("superseded");
    assert!(superseded_at >= before && superseded_at <= Utc::now());
    assert_eq!(old.superseded_by, Some(replacement.id));
    assert!(!old.is_active());
    assert_eq!(invalidate.metadata["invalid_at_source"], "unknown");
    assert_eq!(invalidate.metadata["invalid_at"], json!(null));
    assert_eq!(invalidate.metadata["superseded_at"], json!(superseded_at));

    let default = search_for(&engine, &world.tenant, "Ravi swims", false).await;
    assert!(!default.contains(&old.content), "{default:?}");
    let history = search_for(&engine, &world.tenant, "Ravi swims", true).await;
    assert!(history.contains(&old.content), "{history:?}");
    let context = |include_history| BuildContextInput {
        tenant: world.tenant.clone(),
        query: "Ravi swims".to_string(),
        include_history,
        ..BuildContextInput::default()
    };
    let without = engine.build_context(context(false)).await.unwrap().context;
    let with = engine.build_context(context(true)).await.unwrap().context;
    assert!(
        !without.contains("swims daily") && with.contains("swims daily"),
        "{without} / {with}"
    );
}

/// Without any client time, neither the receipt time nor `recorded_at` becomes
/// `invalid_at`.
#[tokio::test]
async fn receipt_and_recorded_time_are_never_copied_into_invalid_at() {
    let world = World::new();
    let (old, _, invalidate, _) = supersede_with(
        &world,
        candidate("Ravi stopped swimming.", "Ravi"),
        invalidate_first,
    )
    .await;
    assert_eq!(old.invalid_at, None);
    assert!(old.superseded_at.is_some());
    assert_eq!(invalidate.metadata["invalid_at_source"], "unknown");
}

/// A model `invalid_at` later than the source message is rejected and recorded, and
/// no other time is manufactured in its place. An unreadable one is treated the same way.
#[tokio::test]
async fn a_model_invalid_at_after_the_source_message_is_rejected() {
    let world = World::new();
    let stated = Utc.with_ymd_and_hms(2025, 5, 6, 9, 0, 0).unwrap();
    let mut newer = candidate("Ravi stopped swimming.", "Ravi");
    newer.structure.observed_at = Some(stated);
    let later = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
    let (old, _, invalidate, _) =
        supersede_with(&world, newer, move |input| ConsolidationDecision {
            invalid_at: Some(later),
            ..invalidate_first(input)
        })
        .await;
    assert_eq!(old.invalid_at, None);
    assert!(old.superseded_at.is_some());
    assert_eq!(invalidate.metadata["invalid_at_source"], "unknown");
    assert_eq!(
        invalidate.metadata["invalid_at_rejected"]["value"],
        json!(later.to_rfc3339())
    );
    assert!(
        invalidate.metadata["invalid_at_rejected"]["reason"]
            .as_str()
            .unwrap()
            .contains("later than the source message")
    );
    assert_eq!(
        invalidate.metadata["model_decision"]["invalid_at"],
        json!(later)
    );

    let world = World::new();
    let (old, _, invalidate, _) = supersede_with(
        &world,
        candidate("Ravi stopped swimming.", "Ravi"),
        |input| ConsolidationDecision {
            invalid_at_unparsed: Some("around spring".to_string()),
            ..invalidate_first(input)
        },
    )
    .await;
    assert_eq!(old.invalid_at, None);
    assert_eq!(
        invalidate.metadata["invalid_at_rejected"]["value"],
        "around spring"
    );
}

/// A day-precision time expression the resolver settles is the change time; a plausible
/// model date without such evidence is rejected, and a model-only event time is no evidence.
#[tokio::test]
async fn a_resolved_event_time_is_a_known_change_time() {
    let world = World::new();
    let stated = Utc.with_ymd_and_hms(2025, 5, 6, 9, 0, 0).unwrap();
    let changed = Utc.with_ymd_and_hms(2025, 4, 1, 0, 0, 0).unwrap();
    let newer = with_time(
        "Ravi stopped swimming on 1 April 2025.",
        "1 April 2025",
        Some(stated),
    );
    let (old, replacement, invalidate, _) = supersede_with(&world, newer, invalidate_first).await;
    assert_eq!(old.invalid_at, Some(changed));
    assert!(old.superseded_at.is_some());
    assert_eq!(old.superseded_by, Some(replacement.id));
    assert_eq!(
        invalidate.metadata["invalid_at_source"],
        "source_event_time"
    );
}

#[tokio::test]
async fn an_unsupported_model_date_is_rejected_even_when_plausible() {
    let world = World::new();
    let stated = Utc.with_ymd_and_hms(2025, 5, 6, 9, 0, 0).unwrap();
    let plausible = Utc.with_ymd_and_hms(2025, 4, 20, 0, 0, 0).unwrap();
    let mut newer = candidate("Ravi no longer swims.", "Ravi");
    newer.structure.observed_at = Some(stated);
    let (old, _, invalidate, engine) =
        supersede_with(&world, newer, move |input| ConsolidationDecision {
            invalid_at: Some(plausible),
            ..invalidate_first(input)
        })
        .await;
    assert_eq!(old.invalid_at, None);
    assert!(old.superseded_at.is_some() && !old.is_active());
    assert_eq!(invalidate.metadata["invalid_at_source"], "unknown");
    assert!(
        invalidate.metadata["invalid_at_rejected"]["reason"]
            .as_str()
            .unwrap()
            .contains("no deterministic source temporal evidence")
    );
    let default = search_for(&engine, &world.tenant, "Ravi swims", false).await;
    assert!(!default.contains(&old.content), "{default:?}");

    // A model date outside the source-resolved window is rejected too.
    let world = World::new();
    let newer = with_time(
        "Ravi stopped swimming in April 2025.",
        "April 2025",
        Some(stated),
    );
    let outside = Utc.with_ymd_and_hms(2025, 3, 2, 0, 0, 0).unwrap();
    let (old, _, invalidate, _) =
        supersede_with(&world, newer, move |input| ConsolidationDecision {
            invalid_at: Some(outside),
            ..invalidate_first(input)
        })
        .await;
    assert_eq!(old.invalid_at, None);
    assert!(
        invalidate.metadata["invalid_at_rejected"]["reason"]
            .as_str()
            .unwrap()
            .contains("outside the source-resolved window")
    );
}

/// A superseded fact with no stored replacement is still inactive: left out of v2
/// search and never offered to the model again.
#[tokio::test]
async fn a_superseded_fact_without_a_replacement_is_inactive() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi lives in Pune.", "Ravi")],
        |_| unreachable!(),
    );
    let engine = engine_with(&world, first, always_ask());
    world.add(&engine, &world.tenant).await;
    let mut fact = world.all_facts(&world.tenant).await.remove(0);
    fact.superseded_at = Some(Utc::now());
    world
        .facts
        .update_fact(&world.tenant, fact.clone())
        .await
        .unwrap();
    let stored = world
        .facts
        .get_fact(&world.tenant, fact.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((stored.invalid_at, stored.superseded_by), (None, None));
    assert!(!stored.is_active());
    assert!(search(&engine, &world.tenant, false).await.is_empty());
    assert_eq!(search(&engine, &world.tenant, true).await.len(), 1);

    let next = scripted(vec![candidate("Ravi lives in Pune.", "Ravi")], |_| {
        panic!("an inactive fact is never offered")
    });
    let output = world
        .add(
            &engine_with(&world, next.clone(), always_ask()),
            &world.tenant,
        )
        .await;
    assert_eq!(output.added, 1);
    assert!(next.consolidation_inputs().is_empty());
}

#[tokio::test]
async fn a_cross_subject_decision_is_refused_and_stored_as_new() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi adopted a parrot.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let second = scripted(
        vec![candidate("Meena adopted a parrot.", "Meena")],
        |input| {
            decision(
                ConsolidationOperation::Update,
                Some(input.existing_facts[0].id),
                Some("Ravi and Meena adopted parrots."),
            )
        },
    );
    let output = world
        .add(&engine_with(&world, second, always_ask()), &world.tenant)
        .await;

    assert_eq!((output.added, output.updated), (1, 0));
    let contents: Vec<String> = world
        .all_facts(&world.tenant)
        .await
        .into_iter()
        .map(|fact| fact.content)
        .collect();
    assert!(contents.contains(&"Ravi adopted a parrot.".to_string()));
    assert!(contents.contains(&"Meena adopted a parrot.".to_string()));
    let add = world.events().await.pop().unwrap();
    assert_eq!(add.metadata["consolidation"]["path"], "model_rejected");
    assert!(
        add.metadata["consolidation"]["rejected"]
            .as_str()
            .unwrap()
            .contains("cross-subject")
    );
}

#[tokio::test]
async fn a_target_that_was_not_offered_is_refused() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi lives in Pune.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let second = scripted(vec![candidate("Ravi lives in Mysore.", "Ravi")], |_| {
        decision(
            ConsolidationOperation::Invalidate,
            Some(Uuid::new_v4()),
            None,
        )
    });
    let output = world
        .add(&engine_with(&world, second, always_ask()), &world.tenant)
        .await;
    assert_eq!((output.added, output.invalidated), (1, 0));
    assert!(
        world
            .all_facts(&world.tenant)
            .await
            .iter()
            .all(Fact::is_active)
    );
}

#[tokio::test]
async fn a_model_noop_discards_the_candidate_and_names_its_target() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi grows orchids.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let second = scripted(
        vec![candidate("Orchids are what Ravi grows.", "Ravi")],
        |input| {
            decision(
                ConsolidationOperation::NoOp,
                Some(input.existing_facts[0].id),
                None,
            )
        },
    );
    let output = world
        .add(&engine_with(&world, second, always_ask()), &world.tenant)
        .await;
    assert_eq!((output.added, output.noop), (0, 1));
    assert_eq!(output.discarded.no_op_decision, 1);
    let noop = world.events().await.pop().unwrap();
    assert_eq!(noop.operation, MemoryEventOperation::NoOp);
    assert!(noop.fact_id.is_some());
    assert_eq!(noop.metadata["path"], "model");
}

#[tokio::test]
async fn another_users_facts_are_never_offered_or_matched() {
    let world = World::new();
    let other = TenantContext::new("org_c", "user_other").unwrap();
    let theirs = scripted(
        vec![candidate("Ravi grows orchids.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, theirs, always_ask()), &other)
        .await;

    let mine = scripted(vec![candidate("Ravi grows orchids.", "Ravi")], |_| {
        panic!("no related fact exists for this user")
    });
    let output = world
        .add(
            &engine_with(&world, mine.clone(), always_ask()),
            &world.tenant,
        )
        .await;
    assert_eq!(output.added, 1, "not a duplicate of another user's fact");
    assert!(mine.consolidation_inputs().is_empty());
    assert_eq!(world.all_facts(&other).await.len(), 1);
}

#[tokio::test]
async fn invalidated_facts_are_never_offered_again() {
    let world = World::new();
    let first = scripted(
        vec![candidate("Ravi lives in Pune.", "Ravi")],
        |_| unreachable!(),
    );
    world
        .add(&engine_with(&world, first, always_ask()), &world.tenant)
        .await;
    let second = scripted(
        vec![candidate("Ravi lives in Mysore now.", "Ravi")],
        |input| {
            decision(
                ConsolidationOperation::Invalidate,
                Some(input.existing_facts[0].id),
                None,
            )
        },
    );
    world
        .add(&engine_with(&world, second, always_ask()), &world.tenant)
        .await;
    let third = scripted(vec![candidate("Ravi lives in Mysore.", "Ravi")], |input| {
        decision(
            ConsolidationOperation::NoOp,
            Some(input.existing_facts[0].id),
            None,
        )
    });
    world
        .add(
            &engine_with(&world, third.clone(), always_ask()),
            &world.tenant,
        )
        .await;
    let offered = &third.consolidation_inputs()[0].existing_facts;
    assert!(offered.iter().all(Fact::is_active));
    assert!(offered.iter().all(|fact| !fact.content.contains("Pune")));
}
