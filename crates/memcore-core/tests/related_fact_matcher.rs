//! The opt-in semantic related-fact matcher on an independent,
//! labelled set (invented data; no benchmark text). Every case runs the legacy matcher (default)
//! and the semantic matcher on the same input.
//!
//! - **Oracle classifier:** applies the labelled decision when its labelled target is offered
//!   (a classifier cannot update a fact it was not shown) and otherwise adds. This measures what
//!   the lookup lets a correct classifier do: related-fact recall, false-positive offers and the
//!   final memory state.
//! - **Adversarial classifiers:** always Update / Delete the first offered fact, or name a fact
//!   that was never offered. This measures what the guard lets a wrong classifier change.
//!
//! Embeddings are a deterministic bag-of-words vector (cosine = word overlap), a lexical stand-in
//! for a real embedding model: paraphrases sharing few words are out of its reach by design. Set
//! `MEMORY_INTEL_V1_METRICS=1` to print the metrics as JSON.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::MemcoreResult;
use memcore_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider,
    SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, Fact, FactExtractionOutput, FactOperation,
    FactOperationDecision, FactSearchQuery, FactStore, FactStructure, MemoryEngine, MemoryMessage,
    MemoryType, MessageRole, RelatedFactMatcher, SearchMemoryInput, TemporalGranularity,
    TemporalValue, TenantContext,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 256;

/// Bag-of-words embedding: each lower-cased word (punctuation stripped) adds 1 to a hashed slot.
struct LexicalEmbedding;

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

type Decide = Box<dyn Fn(&FactClassificationInput) -> FactOperationDecision + Send + Sync>;

/// Extraction returns the next queued candidate; every classification records what it was offered.
struct Llm {
    queue: Mutex<Vec<CandidateFact>>,
    decide: Decide,
    offered: Mutex<Vec<Vec<String>>>,
    /// While true (the case's prior facts), every candidate is simply added.
    seeding: Mutex<bool>,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut queue = self.queue.lock().unwrap();
        let next = if queue.is_empty() {
            Vec::new()
        } else {
            vec![queue.remove(0)]
        };
        Ok(FactExtractionOutput::from_candidates(next))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        if *self.seeding.lock().unwrap() {
            return Ok(FactOperationDecision {
                operation: FactOperation::Add,
                target_fact_id: None,
                reason: Some("seed".to_string()),
                confidence: 1.0,
            });
        }
        self.offered.lock().unwrap().push(
            input
                .existing_facts
                .iter()
                .map(|f| f.content.clone())
                .collect(),
        );
        Ok((self.decide)(&input))
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

#[derive(Clone, Copy)]
struct F {
    user: &'static str,
    subject: &'static str,
    text: &'static str,
    year: Option<i32>,
}

const fn f(user: &'static str, subject: &'static str, text: &'static str) -> F {
    F {
        user,
        subject,
        text,
        year: None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Label {
    Add,
    NoOp,
    Update,
    Delete,
}

struct Case {
    name: &'static str,
    category: &'static str,
    prior: Vec<F>,
    candidate: F,
    /// Index into `prior` of the fact a correct lookup offers (`None`: nothing is related).
    related: Option<usize>,
    /// What a correct classifier decides when it sees `related`.
    label: Label,
    /// Texts that must be active afterwards, and texts that must remain stored but superseded.
    active: Vec<&'static str>,
    superseded: Vec<&'static str>,
}

fn cases() -> Vec<Case> {
    let a = "u_alex";
    vec![
        Case {
            name: "unrelated",
            category: "new_unrelated",
            prior: vec![f(a, "Alex", "Alex keeps bees on the roof.")],
            candidate: f(a, "Alex", "Alex is learning the cello."),
            related: None,
            label: Label::Add,
            active: vec![
                "Alex keeps bees on the roof.",
                "Alex is learning the cello.",
            ],
            superseded: vec![],
        },
        Case {
            name: "identical",
            category: "identical",
            prior: vec![f(a, "Alex", "Alex lives in Berlin.")],
            candidate: f(a, "Alex", "Alex lives in Berlin."),
            related: None,
            label: Label::NoOp,
            active: vec!["Alex lives in Berlin."],
            superseded: vec![],
        },
        Case {
            name: "paraphrase",
            category: "paraphrase",
            prior: vec![f(a, "Alex", "Alex has two cats named Miso and Tofu.")],
            candidate: f(a, "Alex", "Alex owns two cats, Miso and Tofu."),
            related: Some(0),
            label: Label::NoOp,
            active: vec!["Alex has two cats named Miso and Tofu."],
            superseded: vec![],
        },
        Case {
            name: "updated_value",
            category: "update",
            prior: vec![f(a, "Alex", "Alex lives in Berlin.")],
            candidate: f(a, "Alex", "Alex lives in Lisbon now."),
            related: Some(0),
            label: Label::Update,
            active: vec!["Alex lives in Lisbon now."],
            superseded: vec!["Alex lives in Berlin."],
        },
        Case {
            name: "changed_preference",
            category: "preference",
            prior: vec![f(a, "Alex", "Alex prefers tea in the morning.")],
            candidate: f(a, "Alex", "Alex prefers coffee in the morning these days."),
            related: Some(0),
            label: Label::Update,
            active: vec!["Alex prefers coffee in the morning these days."],
            superseded: vec!["Alex prefers tea in the morning."],
        },
        Case {
            name: "contradiction",
            category: "contradiction",
            prior: vec![f(a, "Alex", "Alex is a vegetarian.")],
            candidate: f(a, "Alex", "Alex is not a vegetarian and eats fish."),
            related: Some(0),
            label: Label::Delete,
            active: vec!["Alex is not a vegetarian and eats fish."],
            superseded: vec!["Alex is a vegetarian."],
        },
        Case {
            name: "correction",
            category: "correction",
            prior: vec![f(a, "Alex", "Alex's sister is called Mara.")],
            candidate: f(
                a,
                "Alex",
                "Correction: Alex's sister is called Maren, not Mara.",
            ),
            related: Some(0),
            label: Label::Update,
            active: vec!["Correction: Alex's sister is called Maren, not Mara."],
            superseded: vec!["Alex's sister is called Mara."],
        },
        Case {
            name: "invalidation",
            category: "invalidation",
            prior: vec![f(a, "Alex", "Alex owns a red road bike.")],
            candidate: f(a, "Alex", "Alex no longer owns the red road bike."),
            related: Some(0),
            label: Label::Delete,
            active: vec!["Alex no longer owns the red road bike."],
            superseded: vec!["Alex owns a red road bike."],
        },
        Case {
            name: "historical_job",
            category: "historical",
            prior: vec![F {
                year: Some(2022),
                ..f(a, "Alex", "Alex worked at Acme as an engineer.")
            }],
            candidate: F {
                year: Some(2026),
                ..f(a, "Alex", "Alex works at Globex as an engineer.")
            },
            related: Some(0),
            label: Label::Add,
            active: vec![
                "Alex worked at Acme as an engineer.",
                "Alex works at Globex as an engineer.",
            ],
            superseded: vec![],
        },
        Case {
            name: "other_subject",
            category: "cross_subject",
            prior: vec![f(a, "Jordan", "Jordan lives in Lisbon.")],
            candidate: f(a, "Alex", "Alex lives in Lisbon."),
            related: None,
            label: Label::Add,
            active: vec!["Jordan lives in Lisbon.", "Alex lives in Lisbon."],
            superseded: vec![],
        },
        Case {
            name: "other_tenant",
            category: "cross_tenant",
            prior: vec![f("u_other", "Alex", "Alex lives in Berlin.")],
            candidate: f(a, "Alex", "Alex lives in Lisbon now."),
            related: None,
            label: Label::Add,
            active: vec!["Alex lives in Lisbon now."],
            superseded: vec![],
        },
        Case {
            name: "relative_vs_absolute",
            category: "temporal_restatement",
            prior: vec![F {
                year: Some(2024),
                ..f(a, "Alex", "Alex started a pottery course in March.")
            }],
            candidate: F {
                year: Some(2024),
                ..f(a, "Alex", "Alex started a pottery course last month.")
            },
            related: Some(0),
            label: Label::NoOp,
            active: vec!["Alex started a pottery course in March."],
            superseded: vec![],
        },
        Case {
            name: "same_subject_other_relative",
            category: "similar_attribute",
            prior: vec![f(a, "Alex", "Alex's brother lives in Porto.")],
            candidate: f(a, "Alex", "Alex's sister lives in Porto."),
            related: None,
            label: Label::Add,
            active: vec![
                "Alex's brother lives in Porto.",
                "Alex's sister lives in Porto.",
            ],
            superseded: vec![],
        },
        Case {
            name: "update_among_distractors",
            category: "update",
            prior: vec![
                f(a, "Alex", "Alex plays tennis on Sundays."),
                f(a, "Alex", "Alex's phone number ends in 4471."),
                f(a, "Alex", "Alex drives a blue hatchback."),
            ],
            candidate: f(a, "Alex", "Alex drives a green electric hatchback now."),
            related: Some(2),
            label: Label::Update,
            active: vec![
                "Alex plays tennis on Sundays.",
                "Alex's phone number ends in 4471.",
                "Alex drives a green electric hatchback now.",
            ],
            superseded: vec!["Alex drives a blue hatchback."],
        },
    ]
}

fn candidate(fact: &F) -> CandidateFact {
    let mut c =
        CandidateFact::new(fact.text, MemoryType::Knowledge, 0.9, 0.9, None, json!({})).unwrap();
    let event_time = fact.year.map(|y| {
        TemporalValue::new(
            Utc.with_ymd_and_hms(y, 3, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(y, 3, 31, 23, 59, 59).unwrap(),
            TemporalGranularity::Month,
            Utc.with_ymd_and_hms(y, 4, 10, 0, 0, 0).unwrap(),
        )
        .unwrap()
    });
    c = c.with_structure(FactStructure {
        subject: Some(fact.subject.to_string()),
        event_time,
        ..Default::default()
    });
    c
}

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_mi1", user).unwrap()
}

struct World {
    facts: Arc<MockFactStore>,
    llm: Arc<Llm>,
    engine: MemoryEngine,
}

impl World {
    fn new(matcher: RelatedFactMatcher, decide: Decide) -> Self {
        Self::with_s1(matcher, decide, false)
    }

    fn with_s1(matcher: RelatedFactMatcher, decide: Decide, s1: bool) -> Self {
        let facts = Arc::new(MockFactStore::new());
        let llm = Arc::new(Llm {
            queue: Mutex::new(Vec::new()),
            decide,
            offered: Mutex::new(Vec::new()),
            seeding: Mutex::new(true),
        });
        let engine = MemoryEngine::new(
            facts.clone(),
            Arc::new(MockVectorStore::new()),
            llm.clone(),
            Arc::new(LexicalEmbedding),
        )
        .with_related_fact_matcher(matcher)
        .with_classification_skip_when_no_related(s1);
        Self { facts, llm, engine }
    }

    async fn ingest(&self, fact: &F) -> MemcoreResult<()> {
        self.llm.queue.lock().unwrap().push(candidate(fact));
        self.engine
            .add_memory(AddMemoryInput {
                tenant: tenant(fact.user),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: fact.text.to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .map(|_| ())
    }

    /// (content, active) of every stored fact of `user`, deleted ones included.
    async fn state(&self, user: &str) -> (BTreeMap<String, bool>, usize) {
        let query = |include_deleted| FactSearchQuery {
            tenant: tenant(user),
            memory_types: None,
            query_text: None,
            limit: 100,
            cursor: None,
            include_deleted,
        };
        let all = self.facts.search_facts(query(true)).await.unwrap();
        let live = self.facts.search_facts(query(false)).await.unwrap();
        let state = all
            .iter()
            .map(|fact: &Fact| (fact.content.clone(), fact.is_active()))
            .collect();
        (state, all.len() - live.len())
    }
}

fn oracle(case_label: Label, related_text: Option<&'static str>) -> Decide {
    Box::new(move |input| {
        let target = related_text.and_then(|text| {
            input
                .existing_facts
                .iter()
                .find(|fact| fact.content == text)
        });
        let (operation, target) = match (case_label, target) {
            (Label::Add, _) | (_, None) => (FactOperation::Add, None),
            (Label::NoOp, Some(t)) => (FactOperation::NoOp, Some(t.id)),
            (Label::Update, Some(t)) => (FactOperation::Update, Some(t.id)),
            (Label::Delete, Some(t)) => (FactOperation::Delete, Some(t.id)),
        };
        FactOperationDecision {
            operation,
            target_fact_id: target,
            reason: Some("oracle".to_string()),
            confidence: 0.9,
        }
    })
}

fn adversarial(kind: &'static str) -> Decide {
    Box::new(move |input| {
        let first = input.existing_facts.first().map(|fact| fact.id);
        let (operation, target) = match kind {
            "update_first" => (FactOperation::Update, first),
            "delete_first" => (FactOperation::Delete, first),
            _ => (FactOperation::Update, Some(Uuid::new_v4())),
        };
        FactOperationDecision {
            operation,
            target_fact_id: target,
            reason: Some(kind.to_string()),
            confidence: 0.9,
        }
    })
}

#[derive(Default, Debug)]
struct Tally {
    related_expected: usize,
    related_offered: usize,
    offers_total: usize,
    offers_false: usize,
    cases: usize,
    final_state_correct: usize,
    per_category: BTreeMap<&'static str, (usize, usize)>,
    hard_deletes: usize,
    cross_subject_or_tenant_changes: usize,
    failed_requests: usize,
    /// Candidates the classifier was called for (S1 skips the others).
    classified: usize,
    /// Cases whose final memory state was not the labelled one.
    incorrect_cases: Vec<&'static str>,
    /// Same-subject, similar-attribute fact changed by a wrong classifier (the guard cannot tell).
    similar_attribute_changes: usize,
}

async fn run_oracle(matcher: RelatedFactMatcher) -> Tally {
    run_oracle_s1(matcher, false).await
}

async fn run_oracle_s1(matcher: RelatedFactMatcher, s1: bool) -> Tally {
    let mut tally = Tally::default();
    for case in cases() {
        let related_text = case.related.map(|i| case.prior[i].text);
        let world = World::with_s1(matcher, oracle(case.label, related_text), s1);
        for prior in &case.prior {
            world.ingest(prior).await.expect("prior");
        }
        *world.llm.seeding.lock().unwrap() = false;
        let seen_before = world.llm.offered.lock().unwrap().len();
        world.ingest(&case.candidate).await.expect("candidate");
        let offered: Vec<String> = world
            .llm
            .offered
            .lock()
            .unwrap()
            .get(seen_before)
            .cloned()
            .unwrap_or_default();
        tally.classified += usize::from(world.llm.offered.lock().unwrap().len() > seen_before);
        tally.offers_total += offered.len();
        tally.offers_false += offered
            .iter()
            .filter(|text| Some(text.as_str()) != related_text)
            .count();
        if let Some(text) = related_text {
            tally.related_expected += 1;
            tally.related_offered += usize::from(offered.iter().any(|o| o == text));
        }
        let (state, deleted) = world.state(case.candidate.user).await;
        let mut ok = deleted == 0;
        for text in &case.active {
            ok &= state.get(*text) == Some(&true);
        }
        for text in &case.superseded {
            ok &= state.get(*text) == Some(&false);
        }
        // Nothing else is stored for the user.
        ok &= state.len() == case.active.len() + case.superseded.len();
        if case.category == "cross_tenant" {
            let (other, _) = world.state("u_other").await;
            ok &= other.get("Alex lives in Berlin.") == Some(&true) && other.len() == 1;
        }
        tally.cases += 1;
        tally.final_state_correct += usize::from(ok);
        if !ok {
            tally.incorrect_cases.push(case.name);
        }
        let entry = tally.per_category.entry(case.category).or_default();
        entry.0 += usize::from(ok);
        entry.1 += 1;
        tally.hard_deletes += deleted;
    }
    tally
}

/// A wrong classifier must not change another subject's or tenant's fact, nor delete anything.
async fn run_adversarial(matcher: RelatedFactMatcher, kind: &'static str) -> Tally {
    let mut tally = Tally::default();
    for case in cases() {
        let world = World::new(matcher, adversarial(kind));
        for prior in &case.prior {
            world.ingest(prior).await.expect("prior");
        }
        *world.llm.seeding.lock().unwrap() = false;
        tally.failed_requests += usize::from(world.ingest(&case.candidate).await.is_err());
        let (state, deleted) = world.state(case.candidate.user).await;
        tally.hard_deletes += deleted;
        if case.category == "cross_subject" {
            tally.cross_subject_or_tenant_changes +=
                usize::from(state.get("Jordan lives in Lisbon.") != Some(&true));
        }
        if case.category == "similar_attribute" {
            tally.similar_attribute_changes +=
                usize::from(state.get("Alex's brother lives in Porto.") != Some(&true));
        }
        if case.category == "historical" {
            tally.cross_subject_or_tenant_changes +=
                usize::from(state.get("Alex worked at Acme as an engineer.") != Some(&true));
        }
        let (other, other_deleted) = world.state("u_other").await;
        if case.category == "cross_tenant" {
            tally.cross_subject_or_tenant_changes +=
                usize::from(other.get("Alex lives in Berlin.") != Some(&true));
        }
        tally.hard_deletes += other_deleted;
        tally.cases += 1;
    }
    tally
}

fn rate(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

#[tokio::test]
async fn related_fact_matcher_on_the_labelled_set() {
    let mut report = HashMap::new();
    for (name, matcher) in [
        ("legacy", RelatedFactMatcher::Legacy),
        ("semantic", RelatedFactMatcher::Semantic),
        ("semantic_v23", RelatedFactMatcher::SemanticV23),
    ] {
        let t = run_oracle(matcher).await;
        report.insert(
            name,
            json!({
                "cases": t.cases,
                "related_fact_recall": rate(t.related_offered, t.related_expected),
                "related_expected": t.related_expected,
                "offers_total": t.offers_total,
                "false_positive_offer_rate": rate(t.offers_false, t.offers_total),
                "final_state_correct": t.final_state_correct,
                "final_state_correct_rate": rate(t.final_state_correct, t.cases),
                "per_category_correct": t.per_category,
                "incorrect_cases": t.incorrect_cases,
                "hard_deletes": t.hard_deletes,
            }),
        );
        // Safety holds on both paths with a correct classifier.
        assert_eq!(t.hard_deletes, 0, "{name}: hard deletes");
        if matcher.is_semantic() {
            assert!(t.related_offered > 0);
        }
    }
    for kind in ["update_first", "delete_first", "unoffered_target"] {
        let v23 = run_adversarial(RelatedFactMatcher::SemanticV23, kind).await;
        assert_eq!(v23.hard_deletes, 0, "semantic_v23 {kind}: no deletion");
        assert_eq!(
            v23.cross_subject_or_tenant_changes, 0,
            "semantic_v23 {kind}: no other-subject, historical or other-tenant change"
        );
        assert_eq!(
            v23.failed_requests, 0,
            "semantic_v23 {kind}: no failed request"
        );
        report.insert(
            Box::leak(format!("semantic_v23_adversarial_{kind}").into_boxed_str()),
            json!({"hard_deletes": v23.hard_deletes, "failed_requests": v23.failed_requests,
                   "same_subject_similar_attribute_changed": v23.similar_attribute_changes,
                   "cross_subject_historical_or_tenant_changes": v23.cross_subject_or_tenant_changes}),
        );
        let t = run_adversarial(RelatedFactMatcher::Semantic, kind).await;
        assert_eq!(t.hard_deletes, 0, "semantic {kind}: no deletion");
        assert_eq!(
            t.cross_subject_or_tenant_changes, 0,
            "semantic {kind}: no other-subject, historical or other-tenant change"
        );
        assert_eq!(t.failed_requests, 0, "semantic {kind}: no failed request");
        report.insert(
            Box::leak(format!("semantic_adversarial_{kind}").into_boxed_str()),
            json!({"hard_deletes": t.hard_deletes, "failed_requests": t.failed_requests,
                   "same_subject_similar_attribute_changed": t.similar_attribute_changes,
                   "cross_subject_historical_or_tenant_changes": t.cross_subject_or_tenant_changes}),
        );
        let legacy = run_adversarial(RelatedFactMatcher::Legacy, kind).await;
        report.insert(
            Box::leak(format!("legacy_adversarial_{kind}").into_boxed_str()),
            json!({"hard_deletes": legacy.hard_deletes, "failed_requests": legacy.failed_requests,
                   "same_subject_similar_attribute_changed": legacy.similar_attribute_changes,
                   "cross_subject_historical_or_tenant_changes": legacy.cross_subject_or_tenant_changes}),
        );
    }
    // S1 on with the semantic matcher: same final states; the classifier is skipped exactly for the
    // candidates nothing was offered to.
    let off = run_oracle(RelatedFactMatcher::Semantic).await;
    let on = run_oracle_s1(RelatedFactMatcher::Semantic, true).await;
    assert_eq!(on.final_state_correct, off.final_state_correct);
    assert_eq!(on.per_category, off.per_category);
    assert!(on.classified < off.classified);
    report.insert(
        "semantic_s1_on",
        json!({"final_state_correct": on.final_state_correct, "classified": on.classified,
               "classified_with_s1_off": off.classified, "cases": on.cases}),
    );
    if std::env::var("MEMORY_INTEL_V1_METRICS").is_ok() {
        println!(
            "MEMORY_INTEL_V1_METRICS {}",
            serde_json::to_string(&report).unwrap()
        );
    }
}

/// After a semantic-matcher update, search (and the server-side context, which uses it) returns the current
/// statement only; the superseded one stays stored as history. The legacy matcher returns both.
#[tokio::test]
async fn search_returns_the_current_value_after_an_update() {
    let prior = f("u_alex", "Alex", "Alex lives in Berlin.");
    let update = f("u_alex", "Alex", "Alex lives in Lisbon now.");
    let mut seen = Vec::new();
    for matcher in [
        RelatedFactMatcher::Legacy,
        RelatedFactMatcher::Semantic,
        RelatedFactMatcher::SemanticV23,
    ] {
        let world = World::new(
            matcher,
            oracle(Label::Update, Some("Alex lives in Berlin.")),
        );
        world.ingest(&prior).await.unwrap();
        *world.llm.seeding.lock().unwrap() = false;
        world.ingest(&update).await.unwrap();
        let found = world
            .engine
            .search_memory(SearchMemoryInput {
                tenant: tenant("u_alex"),
                query: "where does Alex live".to_string(),
                limit: 10,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .unwrap();
        let mut texts: Vec<String> = found.results.iter().map(|r| r.content.clone()).collect();
        texts.sort();
        let (stored, _) = world.state("u_alex").await;
        seen.push((texts, stored));
    }
    assert_eq!(
        seen[0].0,
        vec!["Alex lives in Berlin.", "Alex lives in Lisbon now."]
    );
    for semantic in &seen[1..] {
        assert_eq!(semantic.0, vec!["Alex lives in Lisbon now."]);
        // History kept: the superseded statement is still stored, inactive.
        assert_eq!(semantic.1.get("Alex lives in Berlin."), Some(&false));
    }
}

/// After an update, a current-state search returns only the current
/// value; a search or context that asks for history also returns the previous value, marked as no
/// longer current (with what replaced it), never as an equal alternative. Two events at different
/// times are both current history-wise: neither replaces the other.
#[tokio::test]
async fn current_and_historical_retrieval_after_updates() {
    use memcore_core::{BuildContextInput, HISTORY_MARKER, SearchOptions};
    const UPDATES: [(&str, &str, &str, &str); 5] = [
        (
            "workplace",
            "Alex works at Northwind as an engineer.",
            "Alex works at Contoso as an engineer now.",
            "where does Alex work",
        ),
        (
            "preference",
            "Alex prefers green tea in the morning.",
            "Alex prefers black coffee in the morning now.",
            "what does Alex drink in the morning",
        ),
        (
            "corrected_date",
            "Alex's vet appointment is on 4 June.",
            "Alex's vet appointment is on 11 June instead.",
            "when is Alex's vet appointment",
        ),
        (
            "project_status",
            "Alex's Atlas project status is in review.",
            "Alex's Atlas project status is shipped.",
            "what is the Atlas project status",
        ),
        (
            "address",
            "Alex lives at 12 Elm Street in Leeds.",
            "Alex lives at 40 Oak Road in Leeds now.",
            "where does Alex live",
        ),
    ];
    for matcher in [
        RelatedFactMatcher::Semantic,
        RelatedFactMatcher::SemanticV23,
    ] {
        for (name, prior, update, query) in UPDATES {
            let world = World::new(matcher, oracle(Label::Update, Some(prior)));
            world.ingest(&f("u_alex", "Alex", prior)).await.unwrap();
            *world.llm.seeding.lock().unwrap() = false;
            world.ingest(&f("u_alex", "Alex", update)).await.unwrap();
            let search = |include_history| {
                world.engine.search_memory_with_options(
                    SearchMemoryInput {
                        tenant: tenant("u_alex"),
                        query: query.to_string(),
                        limit: 10,
                        memory_types: None,
                        metadata_filter: None,
                    },
                    SearchOptions {
                        include_history,
                        ..SearchOptions::default()
                    },
                )
            };
            let current = search(false).await.unwrap();
            let texts: Vec<&str> = current.results.iter().map(|r| r.content.as_str()).collect();
            assert_eq!(texts, vec![update], "{matcher:?} {name}: current state");
            assert!(current.history.is_empty(), "{matcher:?} {name}");

            let with_history = search(true).await.unwrap();
            let mut texts: Vec<&str> = with_history
                .results
                .iter()
                .map(|r| r.content.as_str())
                .collect();
            texts.sort();
            let mut expected = vec![prior, update];
            expected.sort();
            assert_eq!(texts, expected, "{matcher:?} {name}: history kept");
            let new_id = with_history
                .results
                .iter()
                .find(|r| r.content == update)
                .unwrap()
                .fact_id;
            let old_id = with_history
                .results
                .iter()
                .find(|r| r.content == prior)
                .unwrap()
                .fact_id;
            assert_eq!(with_history.history.len(), 1, "{matcher:?} {name}");
            let entry = &with_history.history[0];
            assert_eq!(entry.fact_id, old_id);
            assert_eq!(entry.status(), "superseded");
            assert_eq!(entry.superseded_by, Some(new_id));

            for include_history in [false, true] {
                let context = world
                    .engine
                    .build_context(BuildContextInput {
                        tenant: tenant("u_alex"),
                        query: query.to_string(),
                        include_history,
                        ..BuildContextInput::default()
                    })
                    .await
                    .unwrap()
                    .context;
                assert!(context.contains(update), "{matcher:?} {name}: {context}");
                assert!(
                    !context.contains(&format!("{HISTORY_MARKER}{update}")),
                    "{matcher:?} {name}: the current value is never marked"
                );
                assert_eq!(
                    context.contains(&format!("{HISTORY_MARKER}{prior}")),
                    include_history,
                    "{matcher:?} {name}: {context}"
                );
                if !include_history {
                    assert!(!context.contains(prior), "{matcher:?} {name}: {context}");
                }
            }
        }

        // Two events at different times: the guard keeps both; neither is history.
        let first = F {
            user: "u_alex",
            subject: "Alex",
            text: "Alex ran the Berlin half marathon.",
            year: Some(2022),
        };
        let second = F {
            user: "u_alex",
            subject: "Alex",
            text: "Alex ran the Porto half marathon.",
            year: Some(2026),
        };
        let world = World::new(matcher, oracle(Label::Update, Some(first.text)));
        world.ingest(&first).await.unwrap();
        *world.llm.seeding.lock().unwrap() = false;
        world.ingest(&second).await.unwrap();
        let found = world
            .engine
            .search_memory_with_options(
                SearchMemoryInput {
                    tenant: tenant("u_alex"),
                    query: "when did Alex run the half marathon".to_string(),
                    limit: 10,
                    memory_types: None,
                    metadata_filter: None,
                },
                SearchOptions {
                    include_history: true,
                    ..SearchOptions::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(found.results.len(), 2, "{matcher:?}: both events kept");
        assert!(
            found.history.is_empty(),
            "{matcher:?}: neither event is history"
        );
        let (state, _) = world.state("u_alex").await;
        assert!(
            state.values().all(|active| *active),
            "{matcher:?}: {state:?}"
        );
    }
}

/// Another user's history never appears, with or without history.
#[tokio::test]
async fn history_is_scoped_to_its_tenant() {
    use memcore_core::SearchOptions;
    let prior = "Alex lives at 12 Elm Street in Leeds.";
    let update = "Alex lives at 40 Oak Road in Leeds now.";
    let world = World::new(
        RelatedFactMatcher::SemanticV23,
        oracle(Label::Update, Some(prior)),
    );
    world.ingest(&f("u_alex", "Alex", prior)).await.unwrap();
    *world.llm.seeding.lock().unwrap() = false;
    world.ingest(&f("u_alex", "Alex", update)).await.unwrap();
    for include_history in [false, true] {
        let other = world
            .engine
            .search_memory_with_options(
                SearchMemoryInput {
                    tenant: tenant("u_other"),
                    query: "where does Alex live".to_string(),
                    limit: 10,
                    memory_types: None,
                    metadata_filter: None,
                },
                SearchOptions {
                    include_history,
                    ..SearchOptions::default()
                },
            )
            .await
            .unwrap();
        assert!(other.results.is_empty() && other.history.is_empty());
    }
}
