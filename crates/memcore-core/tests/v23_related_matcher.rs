//! candidate related-fact matcher (`semantic_v23`): what it offers the classifier
//! and what its guard lets a decision change, against the V2.2 matcher (`semantic`).
//!
//! The deterministic cases use hand-made vectors: related facts filed under different memory
//! types, events, subject spellings, people who must stay apart, the similarity floor, history and
//! other tenants. `replay_recorded_v22_vectors` (ignored; needs `MEMCORE_V23_REPLAY_FIXTURE`) runs
//! both lookups over the V2.2 states with the recorded text-embedding-3-small vectors and writes
//! what each offered, for comparison with the bench replay script.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use memcore_core::lifecycle::{
    RelatedDecision, canonical_subject, find_related_facts_semantic,
    find_related_facts_semantic_v23, guard_related_decision, guard_related_decision_v23,
};
use memcore_core::{
    CandidateFact, Fact, FactOperation, FactOperationDecision, FactStore, FactStructure,
    MemorySource, MemoryType, TenantContext, VectorRecord, VectorStore,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_v23", user).expect("tenant")
}

struct Store {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
}

impl Store {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn put(
        &self,
        tenant: &TenantContext,
        id: Uuid,
        memory_type: MemoryType,
        content: &str,
        subject: Option<&str>,
        embedding: Vec<f32>,
        recorded_at: DateTime<Utc>,
    ) -> Fact {
        let mut fact = Fact::new(
            id,
            tenant.org_id.clone(),
            tenant.user_id.clone(),
            memory_type,
            content,
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            recorded_at,
            recorded_at,
            json!({}),
        )
        .expect("fact");
        fact.structure = FactStructure {
            subject: subject.map(str::to_string),
            ..FactStructure::default()
        };
        let stored = self
            .facts
            .insert_fact(tenant, fact)
            .await
            .expect("insert fact");
        self.vectors
            .upsert_vector(
                tenant,
                VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id: stored.id,
                    org_id: tenant.org_id.clone(),
                    user_id: tenant.user_id.clone(),
                    embedding,
                    content: content.to_string(),
                    memory_type,
                    metadata: json!({}),
                },
            )
            .await
            .expect("upsert vector");
        stored
    }

    async fn offered(
        &self,
        tenant: &TenantContext,
        candidate: &CandidateFact,
        embedding: &[f32],
        v23: bool,
    ) -> Vec<Fact> {
        if v23 {
            find_related_facts_semantic_v23(
                self.facts.as_ref(),
                self.vectors.as_ref(),
                tenant,
                candidate,
                embedding,
            )
            .await
            .expect("lookup")
        } else {
            find_related_facts_semantic(
                self.facts.as_ref(),
                self.vectors.as_ref(),
                tenant,
                candidate,
                embedding,
            )
            .await
            .expect("lookup")
        }
    }
}

fn candidate(content: &str, memory_type: MemoryType, subject: Option<&str>) -> CandidateFact {
    CandidateFact::new(content, memory_type, 0.9, 0.8, None, json!({}))
        .expect("candidate")
        .with_structure(FactStructure {
            subject: subject.map(str::to_string),
            ..FactStructure::default()
        })
}

/// A unit vector at `cos` similarity to the axis [1, 0, 0, 0] (and in the plane of axis 2).
fn at(cos: f32) -> Vec<f32> {
    vec![cos, (1.0 - cos * cos).sqrt(), 0.0, 0.0]
}

const AXIS: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

fn ids(facts: &[Fact]) -> Vec<Uuid> {
    facts.iter().map(|fact| fact.id).collect()
}

#[test]
fn canonical_subjects_unify_spellings_and_never_merge_names() {
    for (left, right) in [
        ("team", "the team"),
        ("The Team", "team"),
        ("team's", "team"),
        ("the user", "user"),
        ("I", "user"),
        ("community garden.", "community garden"),
    ] {
        assert_eq!(
            canonical_subject(Some(left)),
            canonical_subject(Some(right)),
            "{left:?} = {right:?}"
        );
    }
    for (left, right) in [
        ("user", "Alex"),
        ("Mum", "user"),
        ("his team", "her team"),
        ("community garden project", "community garden"),
        ("Acme Ltd", "Acme Labs"),
    ] {
        assert_ne!(
            canonical_subject(Some(left)),
            canonical_subject(Some(right)),
            "{left:?} != {right:?}"
        );
    }
    assert_eq!(canonical_subject(Some("  ")), None);
    assert_eq!(canonical_subject(None), None);
    // "the" alone is a (strange) name, not an empty one.
    assert_eq!(canonical_subject(Some("the")), Some("the".to_string()));
}

/// V2.2's largest loss: the restated value is filed under another memory type.
#[tokio::test]
async fn a_related_fact_of_another_type_is_offered_only_by_v23() {
    let store = Store::new();
    let user = tenant("cross_type");
    let now = Utc::now();
    let home = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Profile,
            "The user lives in Karen, Nairobi.",
            Some("user"),
            at(0.62),
            now,
        )
        .await;
    let moved = candidate(
        "The user relocated to Mombasa.",
        MemoryType::Event,
        Some("the user"),
    );
    assert!(store.offered(&user, &moved, &AXIS, false).await.is_empty());
    assert_eq!(
        ids(&store.offered(&user, &moved, &AXIS, true).await),
        vec![home.id]
    );
}

/// An event is history: it is offered to another event, never to another type.
#[tokio::test]
async fn an_event_is_offered_only_to_an_event() {
    let store = Store::new();
    let user = tenant("events");
    let now = Utc::now();
    let marathon = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Event,
            "The user ran the Berlin marathon.",
            None,
            at(0.7),
            now,
        )
        .await;
    let profile = candidate("The user is a marathon runner.", MemoryType::Profile, None);
    assert!(store.offered(&user, &profile, &AXIS, true).await.is_empty());
    let event = candidate("The user ran the Paris marathon.", MemoryType::Event, None);
    assert_eq!(
        ids(&store.offered(&user, &event, &AXIS, true).await),
        vec![marathon.id]
    );
}

#[tokio::test]
async fn subjects_floor_history_and_tenants_still_bound_the_offer() {
    let store = Store::new();
    let user = tenant("bounds");
    let other = tenant("someone_else");
    let now = Utc::now();
    let team = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Project,
            "The team ships on Fridays.",
            Some("the team"),
            at(0.8),
            now,
        )
        .await;
    let mum = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Profile,
            "Mum lives in Leeds.",
            Some("Mum"),
            at(0.9),
            now,
        )
        .await;
    let _faint = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Project,
            "The team uses a kanban board.",
            Some("team"),
            at(0.45),
            now,
        )
        .await;
    let _foreign = store
        .put(
            &other,
            Uuid::new_v4(),
            MemoryType::Project,
            "The team ships on Mondays.",
            Some("team"),
            at(0.99),
            now,
        )
        .await;
    let ships = candidate(
        "The team now ships on Thursdays.",
        MemoryType::Project,
        Some("team"),
    );
    // V2.2: "team" != "the team", so the old schedule was never offered.
    assert!(store.offered(&user, &ships, &AXIS, false).await.is_empty());
    assert_eq!(
        ids(&store.offered(&user, &ships, &AXIS, true).await),
        vec![team.id],
        "same team, above the floor, this tenant only"
    );
    // The user's own home never meets Mum's.
    let home = candidate("The user lives in York.", MemoryType::Profile, Some("user"));
    let offered = store.offered(&user, &home, &AXIS, true).await;
    assert!(!ids(&offered).contains(&mum.id), "{offered:?}");

    // A superseded fact is history and never offered again.
    let mut old = store
        .facts
        .get_fact(&user, team.id)
        .await
        .expect("read")
        .expect("fact");
    old.superseded_at = Some(now);
    store.facts.update_fact(&user, old).await.expect("update");
    assert!(store.offered(&user, &ships, &AXIS, true).await.is_empty());
}

fn update(target: Uuid) -> FactOperationDecision {
    FactOperationDecision {
        operation: FactOperation::Update,
        target_fact_id: Some(target),
        reason: Some("new value".into()),
        confidence: 0.9,
    }
}

#[tokio::test]
async fn the_v23_guard_compares_canonical_subjects_and_keeps_events_as_history() {
    let store = Store::new();
    let user = tenant("guard");
    let now = Utc::now();
    let team = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Project,
            "The team ships on Fridays.",
            Some("the team"),
            at(0.8),
            now,
        )
        .await;
    let run = store
        .put(
            &user,
            Uuid::new_v4(),
            MemoryType::Event,
            "The user ran the Berlin marathon.",
            Some("user"),
            at(0.8),
            now,
        )
        .await;
    let ships = candidate(
        "The team now ships on Thursdays.",
        MemoryType::Project,
        Some("team"),
    );
    let offered = vec![team.clone(), run.clone()];
    assert_eq!(
        guard_related_decision_v23(&update(team.id), &offered, &ships),
        RelatedDecision::Supersede { target: team.id }
    );
    // The V2.2 guard treats the two spellings as different subjects.
    assert!(matches!(
        guard_related_decision(&update(team.id), &offered, &ships),
        RelatedDecision::Apply {
            downgraded: Some("different_subject"),
            ..
        }
    ));
    let paris = candidate("The user ran the Paris marathon.", MemoryType::Event, None);
    assert!(matches!(
        guard_related_decision_v23(&update(run.id), &offered, &paris),
        RelatedDecision::Apply {
            downgraded: Some("event_is_history"),
            ..
        }
    ));
    // A target that was not offered is never changed.
    assert!(matches!(
        guard_related_decision_v23(&update(Uuid::new_v4()), &offered, &ships),
        RelatedDecision::Apply {
            downgraded: Some("target_not_offered"),
            ..
        }
    ));
}

fn memory_type(label: &str) -> MemoryType {
    *MemoryType::ALL
        .iter()
        .find(|memory_type| memory_type.as_str() == label)
        .unwrap_or_else(|| panic!("memory type {label:?}"))
}

/// The V2.2 replay with recorded vectors (exposed data: a parity check of the Rust lookups with
/// the bench script, not an accuracy measurement). Writes `{scenario: {lookup_id: {semantic: [...],
/// semantic_v23: [...]}}}` to `MEMCORE_V23_REPLAY_OUT`.
#[tokio::test]
#[ignore = "needs MEMCORE_V23_REPLAY_FIXTURE (V2.2 states and recorded embeddings)"]
async fn replay_recorded_v22_vectors() {
    let path = std::env::var("MEMCORE_V23_REPLAY_FIXTURE").expect("fixture path");
    let out = std::env::var("MEMCORE_V23_REPLAY_OUT").expect("output path");
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read fixture")).expect("json");
    let vectors = fixture["vectors"].as_object().expect("vectors");
    let vector = |text: &str| -> Vec<f32> {
        vectors[text]
            .as_array()
            .expect("vector")
            .iter()
            .map(|value| value.as_f64().expect("float") as f32)
            .collect()
    };
    let mut result = serde_json::Map::new();
    let mut timings_us: Vec<u128> = Vec::new();
    for scenario in fixture["scenarios"].as_array().expect("scenarios") {
        let name = scenario["scenario"].as_str().expect("name");
        let facts = scenario["facts"].as_array().expect("facts");
        let mut per_lookup = serde_json::Map::new();
        for lookup_id in scenario["lookup_ids"].as_array().expect("lookups") {
            let lookup_id = lookup_id.as_str().expect("id");
            let index = facts
                .iter()
                .position(|fact| fact["id"] == lookup_id)
                .expect("lookup fact");
            let store = Store::new();
            let user = tenant(name);
            for fact in &facts[..index] {
                store
                    .put(
                        &user,
                        Uuid::parse_str(fact["id"].as_str().unwrap()).unwrap(),
                        memory_type(fact["memory_type"].as_str().unwrap()),
                        fact["content"].as_str().unwrap(),
                        fact["subject"].as_str(),
                        vector(fact["content"].as_str().unwrap()),
                        DateTime::parse_from_rfc3339(fact["recorded_at"].as_str().unwrap())
                            .unwrap()
                            .with_timezone(&Utc),
                    )
                    .await;
            }
            let fact = &facts[index];
            let content = fact["content"].as_str().unwrap();
            let lookup = candidate(
                content,
                memory_type(fact["memory_type"].as_str().unwrap()),
                fact["subject"].as_str(),
            );
            let embedding = vector(content);
            let legacy = ids(&store.offered(&user, &lookup, &embedding, false).await);
            let started = std::time::Instant::now();
            let v23 = ids(&store.offered(&user, &lookup, &embedding, true).await);
            timings_us.push(started.elapsed().as_micros());
            per_lookup.insert(
                lookup_id.to_string(),
                json!({ "semantic": legacy, "semantic_v23": v23 }),
            );
        }
        result.insert(name.to_string(), serde_json::Value::Object(per_lookup));
    }
    timings_us.sort_unstable();
    let p = |q: f64| timings_us[((timings_us.len() as f64 - 1.0) * q).round() as usize];
    let report = json!({
        "offered": result,
        "mock_store_lookup_us": { "n": timings_us.len(), "p50": p(0.5), "p95": p(0.95), "max": p(1.0) },
    });
    std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).expect("write");
}
