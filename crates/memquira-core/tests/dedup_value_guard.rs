//! Value-aware deduplication (`MEMQUIRA_DEDUP_VALUE_GUARD`) through the engine.
//! Invented data; no benchmark text; no model calls.
//!
//! The embedding here is deliberately VALUE-BLIND, like the real one on these sentences: numbers,
//! number words, ordinals and month names are left out of the vector, so "…14th of June" and
//! "…16th of June" get the same vector (cosine 1.0 >= 0.92). The classifier is scripted and
//! records what it was offered. The path under test is the opt-in update-quality path: semantic_v23
//! lookup, classification skip when nothing is related, history guard for events.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memquira_common::MemquiraResult;
use memquira_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider,
    SummarizationInput,
};
use memquira_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, DedupValueGuard, EmbeddingDeduplicationConfig,
    Fact, FactExtractionOutput, FactOperation, FactOperationDecision, FactSearchQuery, FactStore,
    FactStructure, IngestJournalComponents, MemoryEngine, MemoryMessage, MemoryType, MessageRole,
    RelatedFactMatcher, TemporalGranularity, TemporalValue, TenantContext,
};
use memquira_storage::{MockFactStore, MockIngestJournalStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 256;

const VALUE_WORDS: &[&str] = &[
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "twelve",
    "hundred",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// A bag-of-words vector without values: digits, number words and months are dropped.
fn value_blind(text: &str) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIMS];
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .filter(|w| !w.chars().any(|c| c.is_ascii_digit()) && !VALUE_WORDS.contains(&w.as_str()))
    {
        let mut h: u64 = 1_469_598_103_934_665_603;
        for b in word.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(1_099_511_628_211);
        }
        v[(h % DIMS as u64) as usize] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / norm).collect()
}

/// Paraphrase groups embed identically (an equivalent restatement scores as a near-duplicate).
struct Embed {
    same_as: Vec<(&'static str, &'static str)>,
}

impl Embed {
    fn vector(&self, text: &str) -> Vec<f32> {
        let canonical = self
            .same_as
            .iter()
            .find(|(a, _)| *a == text)
            .map(|(_, b)| *b)
            .unwrap_or(text);
        value_blind(canonical)
    }
}

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemquiraResult<Vec<f32>> {
        Ok(self.vector(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemquiraResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        DIMS
    }
}

struct Llm {
    queue: Mutex<Vec<CandidateFact>>,
    decision: Mutex<(FactOperation, bool)>,
    /// Per classification: the offered facts' contents.
    offered: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemquiraResult<FactExtractionOutput> {
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
    ) -> MemquiraResult<FactOperationDecision> {
        self.offered.lock().unwrap().push(
            input
                .existing_facts
                .iter()
                .map(|f| f.content.clone())
                .collect(),
        );
        let (operation, target_first) = *self.decision.lock().unwrap();
        Ok(FactOperationDecision {
            operation,
            target_fact_id: target_first
                .then(|| input.existing_facts.first().map(|f| f.id))
                .flatten(),
            reason: Some("scripted test classifier".into()),
            confidence: 0.9,
        })
    }

    async fn summarize_memory(&self, _: SummarizationInput) -> MemquiraResult<String> {
        Ok(String::new())
    }
}

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_r1", user).unwrap()
}

fn day(y: i32, m: u32, d: u32) -> TemporalValue {
    let start = Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();
    TemporalValue {
        start,
        end: start,
        granularity: TemporalGranularity::Day,
        anchor: start,
    }
}

fn candidate(
    text: &str,
    kind: MemoryType,
    subject: &str,
    time: Option<TemporalValue>,
) -> CandidateFact {
    CandidateFact::new(text, kind, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some(subject.to_string()),
            event_time: time,
            ..Default::default()
        })
}

struct World {
    facts: Arc<MockFactStore>,
    llm: Arc<Llm>,
    engine: MemoryEngine,
}

struct Opts {
    guard: DedupValueGuard,
    embedding_dedup: bool,
    journal: bool,
    same_as: Vec<(&'static str, &'static str)>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            guard: DedupValueGuard::On,
            embedding_dedup: true,
            journal: false,
            same_as: Vec::new(),
        }
    }
}

impl World {
    fn new(opts: Opts) -> Self {
        let facts = Arc::new(MockFactStore::new());
        let llm = Arc::new(Llm {
            queue: Mutex::new(Vec::new()),
            decision: Mutex::new((FactOperation::Add, false)),
            offered: Mutex::new(Vec::new()),
        });
        let engine = MemoryEngine::new(
            facts.clone(),
            Arc::new(MockVectorStore::new()),
            llm.clone(),
            Arc::new(Embed {
                same_as: opts.same_as,
            }),
        )
        .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
            enabled: opts.embedding_dedup,
            ..Default::default()
        })
        .with_related_fact_matcher(RelatedFactMatcher::SemanticV23)
        .with_classification_skip_when_no_related(true)
        .with_dedup_value_guard(opts.guard)
        .with_ingest_journal(
            opts.journal
                .then(|| IngestJournalComponents::new(Arc::new(MockIngestJournalStore::new()))),
        );
        Self { facts, llm, engine }
    }

    fn decide(&self, operation: FactOperation, target_first: bool) {
        *self.llm.decision.lock().unwrap() = (operation, target_first);
    }

    async fn add(&self, t: &TenantContext, c: CandidateFact) -> AddMemoryOutput {
        self.add_with_id(t, c, None).await
    }

    async fn add_with_id(
        &self,
        t: &TenantContext,
        c: CandidateFact,
        id: Option<&str>,
    ) -> AddMemoryOutput {
        let text = c.content.clone();
        self.llm.queue.lock().unwrap().push(c);
        self.engine
            .add_memory(AddMemoryInput {
                tenant: t.clone(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: text,
                    message_id: id.map(str::to_string),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .expect("add_memory")
    }

    async fn all(&self, t: &TenantContext) -> Vec<Fact> {
        self.facts
            .search_facts(FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 100,
                cursor: None,
                include_deleted: true,
            })
            .await
            .unwrap()
    }

    async fn active(&self, t: &TenantContext) -> Vec<String> {
        let mut v: Vec<String> = self
            .all(t)
            .await
            .into_iter()
            .filter(Fact::is_active)
            .map(|f| f.content)
            .collect();
        v.sort();
        v
    }

    fn classifications(&self) -> usize {
        self.llm.offered.lock().unwrap().len()
    }
}

const OLD_BIRTHDAY: &str = "Rhea's birthday is on the 14th of June.";
const NEW_BIRTHDAY: &str = "Rhea's birthday is on the 16th of June.";

#[tokio::test]
async fn guard_off_pins_the_legacy_loss_of_a_changed_value() {
    let w = World::new(Opts {
        guard: DedupValueGuard::Off,
        ..Default::default()
    });
    let t = tenant("u1");
    w.add(
        &t,
        candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    let out = w
        .add(
            &t,
            candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
        )
        .await;
    assert_eq!(
        out.discarded.duplicate_embedding, 1,
        "legacy: the new value is a silent NOOP"
    );
    assert_eq!(w.classifications(), 0);
    assert_eq!(w.active(&t).await, vec![OLD_BIRTHDAY.to_string()]);
}

#[tokio::test]
async fn a_changed_value_reaches_the_classifier_and_its_decision_is_applied_as_usual() {
    // Add: both kept, nothing superseded (a different number never causes an update by itself)
    let w = World::new(Opts::default());
    let t = tenant("u1");
    w.add(
        &t,
        candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    let out = w
        .add(
            &t,
            candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
        )
        .await;
    assert_eq!(out.discarded.total(), 0);
    assert_eq!(w.classifications(), 1);
    assert_eq!(
        w.llm.offered.lock().unwrap()[0],
        vec![OLD_BIRTHDAY.to_string()]
    );
    assert_eq!(
        w.active(&t).await,
        vec![OLD_BIRTHDAY.to_string(), NEW_BIRTHDAY.to_string()]
    );
    assert!(
        w.all(&t)
            .await
            .iter()
            .all(|f| f.superseded_at.is_none() && f.invalid_at.is_none())
    );

    // Update: the existing update path supersedes the old value and keeps it as history
    let w = World::new(Opts::default());
    w.add(
        &t,
        candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    w.decide(FactOperation::Update, true);
    w.add(
        &t,
        candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    assert_eq!(w.active(&t).await, vec![NEW_BIRTHDAY.to_string()]);
    let all = w.all(&t).await;
    assert!(
        all.iter()
            .any(|f| f.content == OLD_BIRTHDAY && f.superseded_at.is_some())
    );

    // NoOp from the classifier is still a NoOp
    let w = World::new(Opts::default());
    w.add(
        &t,
        candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    w.decide(FactOperation::NoOp, false);
    let out = w
        .add(
            &t,
            candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
        )
        .await;
    assert_eq!(out.discarded.no_op_decision, 1);
    assert_eq!(w.active(&t).await, vec![OLD_BIRTHDAY.to_string()]);
}

#[tokio::test]
async fn true_duplicates_stay_deduplicated() {
    let meeting_a = "The meeting is on June 16.";
    let meeting_b = "The meeting takes place on the 16th of June.";
    let allergy_a = "Yusra is allergic to peanuts.";
    let allergy_b = "Yusra has a peanut allergy.";
    let w = World::new(Opts {
        same_as: vec![
            (meeting_b, meeting_a),
            (allergy_b, allergy_a),
            ("The ridge trail is long.", "The ridge trail is 5 km long."),
        ],
        ..Default::default()
    });
    let t = tenant("u1");
    w.add(&t, candidate(meeting_a, MemoryType::Event, "user", None))
        .await;
    let out = w
        .add(&t, candidate(meeting_b, MemoryType::Event, "user", None))
        .await;
    assert_eq!(
        out.discarded.duplicate_embedding, 1,
        "equivalent date in another format"
    );
    w.add(&t, candidate(allergy_a, MemoryType::Profile, "Yusra", None))
        .await;
    let out = w
        .add(&t, candidate(allergy_b, MemoryType::Profile, "Yusra", None))
        .await;
    assert_eq!(
        out.discarded.duplicate_embedding, 1,
        "paraphrase with no value"
    );
    // equivalent quantity with matching unit, less specific restatement
    let trail_a = "The ridge trail is 5 km long.";
    let trail_b = "The ridge trail is five km long.";
    w.add(
        &t,
        candidate(trail_a, MemoryType::Knowledge, "ridge trail", None),
    )
    .await;
    let out = w
        .add(
            &t,
            candidate(trail_b, MemoryType::Knowledge, "ridge trail", None),
        )
        .await;
    assert_eq!(out.discarded.duplicate_embedding, 1);
    let out = w
        .add(
            &t,
            candidate(
                "The ridge trail is long.",
                MemoryType::Knowledge,
                "ridge trail",
                None,
            ),
        )
        .await;
    assert_eq!(
        out.discarded.duplicate_embedding, 1,
        "omitting the value is still a duplicate"
    );
    assert_eq!(w.classifications(), 0);
    assert_eq!(w.active(&t).await.len(), 3);
}

#[tokio::test]
async fn exact_retries_and_replayed_requests_stay_noops() {
    // exact text: never guarded
    let w = World::new(Opts::default());
    let t = tenant("u1");
    w.add(
        &t,
        candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    let out = w
        .add(
            &t,
            candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
        )
        .await;
    assert_eq!(out.discarded.duplicate_content, 1);
    // an event with a different event time but the same text is still an exact duplicate
    let on_march = candidate(
        "Bea donated blood.",
        MemoryType::Event,
        "Bea",
        Some(day(2024, 3, 5)),
    );
    let on_july = candidate(
        "Bea donated blood.",
        MemoryType::Event,
        "Bea",
        Some(day(2024, 7, 9)),
    );
    w.add(&t, on_march).await;
    let out = w.add(&t, on_july).await;
    assert_eq!(
        out.discarded.duplicate_content, 1,
        "exact-text rule unchanged"
    );

    // a replayed request (same message id) with the ingest journal is answered from its receipt
    let w = World::new(Opts {
        journal: true,
        ..Default::default()
    });
    w.add(
        &t,
        candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    let first = w
        .add_with_id(
            &t,
            candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
            Some("m-2"),
        )
        .await;
    let replay = w
        .add_with_id(
            &t,
            candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
            Some("m-2"),
        )
        .await;
    assert_eq!(first.added, 1);
    assert_eq!(replay.added, first.added);
    assert_eq!(w.active(&t).await.len(), 2, "the replay wrote nothing");
    assert_eq!(
        w.classifications(),
        1,
        "the replay made no second classification"
    );
}

#[tokio::test]
async fn repeated_events_on_different_dates_are_kept_and_never_superseded() {
    let first = "On 5 March 2024, Bea donated blood.";
    let again = "On 9 July 2024, Bea donated blood.";
    // legacy: the second occurrence is lost
    let w = World::new(Opts {
        guard: DedupValueGuard::Off,
        ..Default::default()
    });
    let t = tenant("u1");
    w.add(
        &t,
        candidate(first, MemoryType::Event, "Bea", Some(day(2024, 3, 5))),
    )
    .await;
    let out = w
        .add(
            &t,
            candidate(again, MemoryType::Event, "Bea", Some(day(2024, 7, 9))),
        )
        .await;
    assert_eq!(out.discarded.duplicate_embedding, 1);

    // guarded: it reaches the classifier; an Update of an earlier event is refused by the
    // unchanged history guard, so both occurrences stay active
    let w = World::new(Opts::default());
    w.add(
        &t,
        candidate(first, MemoryType::Event, "Bea", Some(day(2024, 3, 5))),
    )
    .await;
    w.decide(FactOperation::Update, true);
    w.add(
        &t,
        candidate(again, MemoryType::Event, "Bea", Some(day(2024, 7, 9))),
    )
    .await;
    assert_eq!(w.classifications(), 1);
    assert_eq!(
        w.active(&t).await,
        vec![first.to_string(), again.to_string()]
    );

    // the same occurrence restated the same day is still a duplicate of it
    let restated = "On 9 July 2024, Bea donated blood at the clinic.";
    let w2 = World::new(Opts {
        same_as: vec![(restated, again)],
        ..Default::default()
    });
    w2.add(
        &t,
        candidate(again, MemoryType::Event, "Bea", Some(day(2024, 7, 9))),
    )
    .await;
    let out = w2
        .add(
            &t,
            candidate(restated, MemoryType::Event, "Bea", Some(day(2024, 7, 9))),
        )
        .await;
    assert_eq!(out.discarded.duplicate_embedding, 1);
}

#[tokio::test]
async fn the_token_overlap_rule_is_guarded_too() {
    // 25 words, one number differs: Jaccard 24/26 = 0.92 >= 0.90. Embedding dedup off isolates the rule.
    let a = "The shared studio on the north side of the old river warehouse holds exactly 12 drawing desks for the whole design team this winter season";
    let b = "The shared studio on the north side of the old river warehouse holds exactly 14 drawing desks for the whole design team this winter season";
    let w = World::new(Opts {
        guard: DedupValueGuard::Off,
        embedding_dedup: false,
        ..Default::default()
    });
    let t = tenant("u1");
    w.add(&t, candidate(a, MemoryType::Knowledge, "studio", None))
        .await;
    let out = w
        .add(&t, candidate(b, MemoryType::Knowledge, "studio", None))
        .await;
    assert_eq!(
        out.discarded.duplicate_content, 1,
        "legacy: high token overlap"
    );

    let w = World::new(Opts {
        embedding_dedup: false,
        ..Default::default()
    });
    w.add(&t, candidate(a, MemoryType::Knowledge, "studio", None))
        .await;
    let out = w
        .add(&t, candidate(b, MemoryType::Knowledge, "studio", None))
        .await;
    assert_eq!(out.discarded.total(), 0);
    assert_eq!(w.active(&t).await.len(), 2);
}

#[tokio::test]
async fn different_values_of_many_kinds_are_not_silently_discarded() {
    let pairs: &[(&str, &str, MemoryType)] = &[
        (
            "The rent is $1,620 a month.",
            "The rent is $1,700 a month.",
            MemoryType::Knowledge,
        ),
        (
            "The discount is 20% this year.",
            "The discount is 25% this year.",
            MemoryType::Knowledge,
        ),
        (
            "Ines is 182 cm tall.",
            "Ines is 178 cm tall.",
            MemoryType::Profile,
        ),
        (
            "The team has 12 engineers.",
            "The team has 14 engineers.",
            MemoryType::Knowledge,
        ),
        (
            "Pell's phone number is 555-0142.",
            "Pell's phone number is 555-0199.",
            MemoryType::Profile,
        ),
        (
            "The project started in 2019.",
            "The project started in 2023.",
            MemoryType::Project,
        ),
        (
            "The standup is at 9:30.",
            "The standup is at 10:15.",
            MemoryType::Event,
        ),
    ];
    for (old, new, kind) in pairs {
        let w = World::new(Opts::default());
        let t = tenant("u1");
        w.add(&t, candidate(old, *kind, "x", None)).await;
        let out = w.add(&t, candidate(new, *kind, "x", None)).await;
        assert_eq!(out.discarded.total(), 0, "{new}");
        assert_eq!(w.classifications(), 1, "{new}: reaches the classifier");
        assert_eq!(
            w.active(&t).await.len(),
            2,
            "{new}: Add keeps both; no update is inferred"
        );
    }
}

#[tokio::test]
async fn the_guard_never_looks_across_tenants_or_users() {
    let w = World::new(Opts::default());
    let a = tenant("user_a");
    let b = TenantContext::new("org_other", "user_a").unwrap();
    let c = tenant("user_c");
    w.add(
        &a,
        candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    for t in [&b, &c] {
        let out = w
            .add(
                t,
                candidate(OLD_BIRTHDAY, MemoryType::Profile, "Rhea", None),
            )
            .await;
        assert_eq!(out.added, 1);
        assert_eq!(out.discarded.total(), 0);
    }
    w.add(
        &c,
        candidate(NEW_BIRTHDAY, MemoryType::Profile, "Rhea", None),
    )
    .await;
    assert_eq!(w.active(&a).await, vec![OLD_BIRTHDAY.to_string()]);
    let ids: Vec<Uuid> = w.all(&a).await.iter().map(|f| f.id).collect();
    assert_eq!(ids.len(), 1, "user A untouched");
}
