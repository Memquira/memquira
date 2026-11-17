//! Update-target authorization, stale targets, the event policy and the
//! related-fact floor. Invented data; no benchmark text; no model calls.
//!
//! The classifier here is ADVERSARIAL on purpose: it returns whatever target id a test sets, whether
//! or not that fact was offered, so the tests show what the engine lets a wrong or manipulated model
//! decision change. The legacy (default) path is pinned as it behaves today; the candidate path is
//! `semantic_v23`, where every Update / Delete goes through the history-keeping guard.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::MemcoreResult;
use memcore_core::lifecycle::{
    EventUpdatePolicy, RelatedDecision, find_related_facts_semantic_v23_with_floor,
    guard_related_decision_v23_with_policy, states_a_change,
};
use memcore_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider,
    SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, Fact, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, FactStructure, MemoryEngine,
    MemoryMessage, MemorySource, MemoryType, MessageRole, RelatedFactMatcher, TemporalGranularity,
    TemporalValue, TenantContext, VectorRecord, VectorStore,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 256;

fn lexical(text: &str) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIMS];
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let mut h: u64 = 1_469_598_103_934_665_603;
        for b in word.to_lowercase().bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(1_099_511_628_211);
        }
        v[(h % DIMS as u64) as usize] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / norm).collect()
}

struct LexicalEmbedding;

#[async_trait]
impl EmbeddingProvider for LexicalEmbedding {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        Ok(lexical(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| lexical(t)).collect())
    }
    fn dimensions(&self) -> usize {
        DIMS
    }
}

/// What the adversarial classifier does to the store while it "thinks" (between the lookup and the
/// write): simulates another request or the user changing the offered target.
#[derive(Clone, Copy)]
enum Meanwhile {
    Nothing,
    DeleteTarget,
    SupersedeTarget(Uuid),
}

struct Llm {
    facts: Arc<MockFactStore>,
    queue: Mutex<Vec<CandidateFact>>,
    /// The decision for the next classification: (operation, target id).
    decision: Mutex<(FactOperation, Option<Uuid>)>,
    meanwhile: Mutex<(Meanwhile, Option<TenantContext>)>,
    offered: Mutex<Vec<Vec<Uuid>>>,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut q = self.queue.lock().unwrap();
        let next = if q.is_empty() {
            Vec::new()
        } else {
            vec![q.remove(0)]
        };
        Ok(FactExtractionOutput::from_candidates(next))
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.offered
            .lock()
            .unwrap()
            .push(input.existing_facts.iter().map(|f| f.id).collect());
        let (operation, target) = *self.decision.lock().unwrap();
        let (meanwhile, tenant) = self.meanwhile.lock().unwrap().clone();
        if let (Some(target), Some(tenant)) = (target, tenant) {
            match meanwhile {
                Meanwhile::Nothing => {}
                Meanwhile::DeleteTarget => {
                    self.facts.soft_delete_fact(&tenant, target).await?;
                }
                Meanwhile::SupersedeTarget(by) => {
                    let mut fact = self.facts.get_fact(&tenant, target).await?.unwrap();
                    fact.superseded_at = Some(Utc::now());
                    fact.superseded_by = Some(by);
                    self.facts.update_fact(&tenant, fact).await?;
                }
            }
        }
        Ok(FactOperationDecision {
            operation,
            target_fact_id: target,
            reason: Some("adversarial test classifier".into()),
            confidence: 0.9,
        })
    }

    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).unwrap()
}

fn candidate(text: &str, kind: MemoryType, subject: &str) -> CandidateFact {
    CandidateFact::new(text, kind, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some(subject.to_string()),
            ..Default::default()
        })
}

struct World {
    facts: Arc<MockFactStore>,
    llm: Arc<Llm>,
    engine: MemoryEngine,
}

impl World {
    fn new(matcher: RelatedFactMatcher) -> Self {
        let facts = Arc::new(MockFactStore::new());
        let llm = Arc::new(Llm {
            facts: facts.clone(),
            queue: Mutex::new(Vec::new()),
            decision: Mutex::new((FactOperation::Add, None)),
            meanwhile: Mutex::new((Meanwhile::Nothing, None)),
            offered: Mutex::new(Vec::new()),
        });
        let engine = MemoryEngine::new(
            facts.clone(),
            Arc::new(MockVectorStore::new()),
            llm.clone(),
            Arc::new(LexicalEmbedding),
        )
        .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
            enabled: false,
            ..Default::default()
        })
        .with_related_fact_matcher(matcher);
        Self { facts, llm, engine }
    }

    async fn add(
        &self,
        t: &TenantContext,
        c: CandidateFact,
        decision: (FactOperation, Option<Uuid>),
        meanwhile: Meanwhile,
    ) -> MemcoreResult<()> {
        *self.llm.decision.lock().unwrap() = decision;
        *self.llm.meanwhile.lock().unwrap() = (meanwhile, Some(t.clone()));
        let text = c.content.clone();
        self.llm.queue.lock().unwrap().push(c);
        self.engine
            .add_memory(AddMemoryInput {
                tenant: t.clone(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: text,
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .map(|_| ())
    }

    async fn seed(&self, t: &TenantContext, text: &str) -> Fact {
        self.add(
            t,
            candidate(text, MemoryType::Profile, "Maya"),
            (FactOperation::Add, None),
            Meanwhile::Nothing,
        )
        .await
        .unwrap();
        self.row(t, text).await.expect("seeded")
    }

    async fn rows(&self, t: &TenantContext) -> Vec<Fact> {
        self.facts
            .search_facts(FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 200,
                cursor: None,
                include_deleted: true,
            })
            .await
            .unwrap()
    }

    async fn row(&self, t: &TenantContext, text: &str) -> Option<Fact> {
        self.rows(t).await.into_iter().find(|f| f.content == text)
    }

    async fn active(&self, t: &TenantContext) -> Vec<String> {
        let mut v: Vec<String> = self
            .facts
            .search_facts(FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 200,
                cursor: None,
                include_deleted: false,
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|f| f.is_active())
            .map(|f| f.content)
            .collect();
        v.sort();
        v
    }
}

const OLD: &str = "Maya works as a nurse at Riverside Clinic.";
const NEW: &str = "Maya now works as a paramedic at Riverside Clinic.";

/// A fact of another user (same organization) or another organization, named by the model, is
/// never changed on either path; on the candidate path the request succeeds and the candidate is an Add.
#[tokio::test]
async fn foreign_targets_are_never_changed() {
    for matcher in [RelatedFactMatcher::Legacy, RelatedFactMatcher::SemanticV23] {
        for (org_b, user_b) in [("org_t01", "user_b"), ("org_other", "user_a")] {
            let w = World::new(matcher);
            let a = tenant("org_t01", "user_a");
            let b = tenant(org_b, user_b);
            w.seed(&a, OLD).await;
            let foreign = w.seed(&b, OLD).await;
            let result = w
                .add(
                    &a,
                    candidate(NEW, MemoryType::Profile, "Maya"),
                    (FactOperation::Update, Some(foreign.id)),
                    Meanwhile::Nothing,
                )
                .await;
            let after = w.row(&b, OLD).await.unwrap();
            assert_eq!(
                after, foreign,
                "{matcher:?} {org_b}/{user_b}: foreign fact unchanged"
            );
            assert_eq!(w.active(&b).await, vec![OLD.to_string()]);
            match matcher {
                // Legacy: the tenant-scoped store finds no such fact; the request fails and the
                // journal-less engine compensates (nothing of this request remains).
                RelatedFactMatcher::Legacy => {
                    assert!(result.is_err(), "legacy: NotFound fails the request");
                    assert_eq!(w.active(&a).await, vec![OLD.to_string()]);
                }
                _ => {
                    result.unwrap();
                    assert_eq!(w.active(&a).await, vec![NEW.to_string(), OLD.to_string()]);
                }
            }
        }
    }
}

/// A target id that does not exist: legacy fails the request; the candidate path stores an Add.
#[tokio::test]
async fn nonexistent_targets() {
    for matcher in [RelatedFactMatcher::Legacy, RelatedFactMatcher::SemanticV23] {
        let w = World::new(matcher);
        let a = tenant("org_t01", "user_a");
        w.seed(&a, OLD).await;
        let result = w
            .add(
                &a,
                candidate(NEW, MemoryType::Profile, "Maya"),
                (FactOperation::Delete, Some(Uuid::new_v4())),
                Meanwhile::Nothing,
            )
            .await;
        if matcher == RelatedFactMatcher::Legacy {
            assert!(result.is_err());
            assert_eq!(w.active(&a).await, vec![OLD.to_string()]);
        } else {
            result.unwrap();
            assert_eq!(w.active(&a).await, vec![NEW.to_string(), OLD.to_string()]);
        }
    }
}

/// A same-user fact that was NOT offered (unrelated): the legacy path applies the model's Update to
/// it (overwriting it in place; pinned as today's default behaviour, a documented weakness); the
/// candidate path refuses it (`target_not_offered`) and stores an Add.
#[tokio::test]
async fn unrelated_targets_that_were_not_offered() {
    const UNRELATED: &str = "Maya enjoys listening to jazz records.";
    for matcher in [RelatedFactMatcher::Legacy, RelatedFactMatcher::SemanticV23] {
        let w = World::new(matcher);
        let a = tenant("org_t01", "user_a");
        w.seed(&a, OLD).await;
        let unrelated = w.seed(&a, UNRELATED).await;
        w.add(
            &a,
            candidate(NEW, MemoryType::Profile, "Maya"),
            (FactOperation::Update, Some(unrelated.id)),
            Meanwhile::Nothing,
        )
        .await
        .unwrap();
        let offered = w.llm.offered.lock().unwrap().last().cloned().unwrap();
        assert!(
            !offered.contains(&unrelated.id),
            "the unrelated fact was not offered"
        );
        if matcher == RelatedFactMatcher::Legacy {
            assert!(
                w.row(&a, UNRELATED).await.is_none(),
                "legacy overwrote the unrelated fact"
            );
        } else {
            assert_eq!(w.row(&a, UNRELATED).await.unwrap(), unrelated);
            assert_eq!(
                w.active(&a).await,
                vec![UNRELATED.to_string(), NEW.to_string(), OLD.to_string()]
            );
        }
    }
}

/// The offered target was deleted by the user while the model was deciding: nothing is
/// superseded, the candidate is an Add, the deleted row is not touched.
#[tokio::test]
async fn a_target_deleted_after_the_offer_is_not_superseded() {
    let w = World::new(RelatedFactMatcher::SemanticV23);
    let a = tenant("org_t01", "user_a");
    let old = w.seed(&a, OLD).await;
    w.add(
        &a,
        candidate(NEW, MemoryType::Profile, "Maya"),
        (FactOperation::Update, Some(old.id)),
        Meanwhile::DeleteTarget,
    )
    .await
    .unwrap();
    let offered = w.llm.offered.lock().unwrap().last().cloned().unwrap();
    assert!(offered.contains(&old.id), "the target was offered");
    assert_eq!(w.active(&a).await, vec![NEW.to_string()]);
    let row = w.row(&a, OLD).await.unwrap();
    assert!(row.superseded_at.is_none() && row.superseded_by.is_none());
}

/// The offered target was superseded by another (committed) request while the model was deciding:
/// its history link must not be overwritten. Previously the engine superseded it again and
/// replaced `superseded_by`.
#[tokio::test]
async fn a_target_superseded_after_the_offer_keeps_its_history_link() {
    let w = World::new(RelatedFactMatcher::SemanticV23);
    let a = tenant("org_t01", "user_a");
    let old = w.seed(&a, OLD).await;
    let first_replacement = Uuid::new_v4();
    w.add(
        &a,
        candidate(NEW, MemoryType::Profile, "Maya"),
        (FactOperation::Update, Some(old.id)),
        Meanwhile::SupersedeTarget(first_replacement),
    )
    .await
    .unwrap();
    let row = w.row(&a, OLD).await.unwrap();
    assert_eq!(
        row.superseded_by,
        Some(first_replacement),
        "history link kept"
    );
    assert_eq!(w.active(&a).await, vec![NEW.to_string()]);
}

/// Without interference the candidate path supersedes the offered target and keeps it as history.
#[tokio::test]
async fn an_offered_active_target_is_superseded_with_history() {
    let w = World::new(RelatedFactMatcher::SemanticV23);
    let a = tenant("org_t01", "user_a");
    let old = w.seed(&a, OLD).await;
    w.add(
        &a,
        candidate(NEW, MemoryType::Profile, "Maya"),
        (FactOperation::Update, Some(old.id)),
        Meanwhile::Nothing,
    )
    .await
    .unwrap();
    let new = w.row(&a, NEW).await.unwrap();
    let row = w.row(&a, OLD).await.unwrap();
    assert_eq!(row.superseded_by, Some(new.id));
    assert_eq!(w.active(&a).await, vec![NEW.to_string()]);
}

// ------------------------------------------------------------------------- event policy ------

fn fact_from(c: &CandidateFact, t: &TenantContext) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        t.org_id.clone(),
        t.user_id.clone(),
        c.memory_type,
        c.content.clone(),
        None,
        MemorySource::UserMessage,
        0.9,
        0.9,
        None,
        None,
        now,
        now,
        json!({}),
    )
    .unwrap()
    .with_structure(c.structure.clone())
}

fn dated(c: CandidateFact) -> CandidateFact {
    let start = Utc::now() - chrono::Duration::days(400);
    let mut structure = c.structure.clone();
    structure.event_time =
        Some(TemporalValue::new(start, start, TemporalGranularity::Day, Utc::now()).unwrap());
    c.with_structure(structure)
}

fn update(target: Uuid) -> FactOperationDecision {
    FactOperationDecision {
        operation: FactOperation::Update,
        target_fact_id: Some(target),
        reason: None,
        confidence: 0.9,
    }
}

#[test]
fn change_markers_are_whole_words() {
    for yes in [
        "The choir now rehearses in the library.",
        "Kofi no longer cycles to work.",
        "Ines doesn't swim anymore.",
        "The meetup moved to Thursdays.",
        "We switched to the evening class.",
        "He used to row on Sundays.",
        "Rehearsals are in the library from now on.",
    ] {
        assert!(states_a_change(yes), "{yes}");
    }
    for no in [
        "Jonas knows the Paris marathon route.",
        "The user ran the Paris marathon.",
        "Snowboarding is the user's winter hobby.",
        "The choir rehearses nowhere near the station.",
        "The user is moving boxes.",
    ] {
        assert!(!states_a_change(no), "{no}");
    }
}

#[test]
fn the_event_policy_lets_only_an_undated_standing_fact_change() {
    let t = tenant("org_t01", "user_a");
    let old_c = candidate(
        "The choir rehearses in the parish church hall.",
        MemoryType::Event,
        "choir",
    );
    let old = fact_from(&old_c, &t);
    let changed = candidate(
        "The choir now rehearses in the parish library.",
        MemoryType::Event,
        "choir",
    );
    let repeat = candidate(
        "The choir rehearses in the parish library.",
        MemoryType::Event,
        "choir",
    );
    let offered = vec![old.clone()];
    let refused = |r: RelatedDecision| {
        matches!(
            r,
            RelatedDecision::Apply {
                downgraded: Some("event_is_history"),
                ..
            }
        )
    };

    // Default (history): never superseded.
    assert!(refused(guard_related_decision_v23_with_policy(
        &update(old.id),
        &offered,
        &changed,
        EventUpdatePolicy::History
    )));
    // change_marker: an undated change of an undated event supersedes ...
    assert_eq!(
        guard_related_decision_v23_with_policy(
            &update(old.id),
            &offered,
            &changed,
            EventUpdatePolicy::ChangeMarker
        ),
        RelatedDecision::Supersede { target: old.id }
    );
    // ... but not without a change word, not with a dated candidate, not a dated target.
    assert!(refused(guard_related_decision_v23_with_policy(
        &update(old.id),
        &offered,
        &repeat,
        EventUpdatePolicy::ChangeMarker
    )));
    assert!(refused(guard_related_decision_v23_with_policy(
        &update(old.id),
        &offered,
        &dated(changed.clone()),
        EventUpdatePolicy::ChangeMarker
    )));
    let dated_old = fact_from(&dated(old_c), &t);
    assert!(refused(guard_related_decision_v23_with_policy(
        &update(dated_old.id),
        std::slice::from_ref(&dated_old),
        &changed,
        EventUpdatePolicy::ChangeMarker
    )));
    // Another subject is still refused first.
    let other = candidate(
        "The band now rehearses in the parish library.",
        MemoryType::Event,
        "band",
    );
    assert!(matches!(
        guard_related_decision_v23_with_policy(
            &update(old.id),
            &offered,
            &other,
            EventUpdatePolicy::ChangeMarker
        ),
        RelatedDecision::Apply {
            downgraded: Some("different_subject"),
            ..
        }
    ));
}

// ------------------------------------------------------------------------- floor -----

#[tokio::test]
async fn the_similarity_floor_is_configurable() {
    let facts = MockFactStore::new();
    let vectors = MockVectorStore::new();
    let t = tenant("org_t01", "user_a");
    let stored = fact_from(
        &candidate("Maya drives a city bus.", MemoryType::Profile, "Maya"),
        &t,
    );
    facts.insert_fact(&t, stored.clone()).await.unwrap();
    vectors
        .write_vector(
            &t,
            VectorRecord {
                id: Uuid::new_v4(),
                fact_id: stored.id,
                org_id: t.org_id.clone(),
                user_id: t.user_id.clone(),
                embedding: lexical(&stored.content),
                content: stored.content.clone(),
                memory_type: stored.memory_type,
                metadata: json!({}),
            },
            memcore_core::VectorWriteIntent::FreshFact,
        )
        .await
        .unwrap();
    let probe = candidate(
        "Maya is a dispatcher in the control room.",
        MemoryType::Profile,
        "Maya",
    );
    let embedding = lexical(&probe.content);
    let score: f32 = lexical(&stored.content)
        .iter()
        .zip(&embedding)
        .map(|(x, y)| x * y)
        .sum();
    assert!(
        score > 0.0 && score < 0.5,
        "probe below the default floor: {score}"
    );
    let at = |floor| {
        let (facts, vectors, t, probe, embedding) = (&facts, &vectors, &t, &probe, &embedding);
        async move {
            find_related_facts_semantic_v23_with_floor(facts, vectors, t, probe, embedding, floor)
                .await
                .unwrap()
                .len()
        }
    };
    assert_eq!(at(0.5).await, 0);
    assert_eq!(at(score - 0.01).await, 1);
}
