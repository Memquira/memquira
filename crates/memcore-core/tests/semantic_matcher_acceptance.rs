//! Semantic matcher acceptance gate: historical-memory correctness and the four matcher × S1
//! configurations, on independent invented fixtures (no benchmark data).
//!
//! For every case and configuration the stored state is checked three ways:
//! - **current state:** the default search (no history) must return the current statements of the
//!   subject and never a superseded one;
//! - **history:** a search with `include_history` and the stored rows must still hold every earlier
//!   statement, and a superseded row must link to the fact that replaced it;
//! - **suppression:** a statement that is still true (a separate event, another period, another
//!   subject or tenant) must never be superseded.
//!
//! Classifiers: `oracle` applies the labelled decision when its target is offered; `update_first`
//! always updates the first offered fact (a wrong classifier). Embeddings are a deterministic
//! bag-of-words stand-in. `SEMANTIC_ACCEPTANCE_METRICS=1` prints the metrics as JSON.

use std::collections::BTreeMap;
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
    MemoryType, MessageRole, RelatedFactMatcher, SearchMemoryInput, SearchOptions,
    TemporalGranularity, TemporalValue, TenantContext,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

const DIMS: usize = 256;

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

#[derive(Clone, Copy, PartialEq)]
enum Classifier {
    Oracle,
    UpdateFirst,
}

/// The decision a correct classifier makes for one ingested statement.
#[derive(Clone, Copy, PartialEq)]
enum Label {
    Add,
    Supersedes(usize),
}

struct Llm {
    queue: Mutex<Vec<(CandidateFact, Label)>>,
    current: Mutex<Option<Label>>,
    texts: Mutex<Vec<String>>,
    classifier: Classifier,
    calls: Mutex<usize>,
    offers: Mutex<Vec<usize>>,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut queue = self.queue.lock().unwrap();
        let (candidate, label) = queue.remove(0);
        *self.current.lock().unwrap() = Some(label);
        Ok(FactExtractionOutput::from_candidates(vec![candidate]))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        *self.calls.lock().unwrap() += 1;
        self.offers.lock().unwrap().push(input.existing_facts.len());
        let label = self.current.lock().unwrap().unwrap();
        let texts = self.texts.lock().unwrap().clone();
        let target = match (self.classifier, label) {
            (Classifier::UpdateFirst, _) => input.existing_facts.first().map(|f| f.id),
            (Classifier::Oracle, Label::Supersedes(i)) => input
                .existing_facts
                .iter()
                .find(|f| f.content == texts[i])
                .map(|f| f.id),
            (Classifier::Oracle, Label::Add) => None,
        };
        Ok(FactOperationDecision {
            operation: if target.is_some() {
                FactOperation::Update
            } else {
                FactOperation::Add
            },
            target_fact_id: target,
            reason: Some("scripted".to_string()),
            confidence: 0.9,
        })
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

#[derive(Clone, Copy)]
struct S {
    user: &'static str,
    subject: &'static str,
    text: &'static str,
    year: Option<i32>,
    label: Label,
    kind: MemoryType,
}

const fn s(subject: &'static str, text: &'static str, label: Label) -> S {
    S {
        user: "u_alex",
        subject,
        text,
        year: None,
        label,
        kind: MemoryType::Knowledge,
    }
}

const fn sy(subject: &'static str, text: &'static str, year: i32, label: Label) -> S {
    S {
        user: "u_alex",
        subject,
        text,
        year: Some(year),
        label,
        kind: MemoryType::Knowledge,
    }
}

struct Case {
    name: &'static str,
    kind: &'static str,
    statements: Vec<S>,
    /// Statements (by index) that are current afterwards.
    current: Vec<usize>,
    /// Statements correctly superseded (current-state claims replaced, corrections, invalidations).
    superseded: Vec<usize>,
    /// Statements that stay true and must never be superseded, whatever the classifier says.
    protected: Vec<usize>,
}

use Label::{Add, Supersedes};

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "current_vs_previous_employer",
            kind: "superseded_current_state",
            statements: vec![
                s("Alex", "Alex works at Acme as a designer.", Add),
                s(
                    "Alex",
                    "Alex works at Globex as a designer now.",
                    Supersedes(0),
                ),
            ],
            current: vec![1],
            superseded: vec![0],
            protected: vec![],
        },
        Case {
            name: "updated_preference",
            kind: "superseded_current_state",
            statements: vec![
                s("Alex", "Alex prefers window seats on flights.", Add),
                s(
                    "Alex",
                    "Alex prefers aisle seats on flights these days.",
                    Supersedes(0),
                ),
            ],
            current: vec![1],
            superseded: vec![0],
            protected: vec![],
        },
        Case {
            name: "correction",
            kind: "explicit_invalidation",
            statements: vec![
                s("Alex", "Alex's daughter is named Ines.", Add),
                s(
                    "Alex",
                    "Correction: Alex's daughter is named Inez.",
                    Supersedes(0),
                ),
            ],
            current: vec![1],
            superseded: vec![0],
            protected: vec![],
        },
        Case {
            name: "valid_facts_from_different_periods",
            kind: "historical_true",
            statements: vec![
                sy("Alex", "Alex lived in Berlin for work.", 2019, Add),
                sy("Alex", "Alex lived in Lisbon for work.", 2023, Add),
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![0],
        },
        Case {
            name: "same_attribute_different_events",
            kind: "historical_true",
            statements: vec![
                S {
                    kind: MemoryType::Event,
                    ..s("Alex", "Alex ran the Berlin marathon.", Add)
                },
                S {
                    kind: MemoryType::Event,
                    ..s("Alex", "Alex ran the Paris marathon.", Add)
                },
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![0],
        },
        Case {
            name: "same_attribute_different_events_dated",
            kind: "historical_true",
            statements: vec![
                S {
                    kind: MemoryType::Event,
                    ..sy("Alex", "Alex ran the city marathon.", 2022, Add)
                },
                S {
                    kind: MemoryType::Event,
                    ..sy("Alex", "Alex ran the city marathon.", 2024, Add)
                },
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![0],
        },
        Case {
            name: "same_attribute_different_events_dated_prefixed",
            kind: "historical_true",
            statements: vec![
                S {
                    kind: MemoryType::Event,
                    ..sy("Alex", "In 2022, Alex ran the city marathon.", 2022, Add)
                },
                S {
                    kind: MemoryType::Event,
                    ..sy("Alex", "In 2024, Alex ran the city marathon.", 2024, Add)
                },
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![0],
        },
        Case {
            name: "contradiction_without_authority",
            kind: "ambiguous",
            statements: vec![
                s("Alex", "Alex is allergic to peanuts.", Add),
                s("Alex", "Alex is not allergic to peanuts.", Add),
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![],
        },
        Case {
            name: "superseded_needed_for_multi_hop",
            kind: "superseded_current_state",
            statements: vec![
                s("Alex", "Alex lives in Berlin with two flatmates.", Add),
                s("Alex", "Alex lives in Lisbon alone now.", Supersedes(0)),
                s(
                    "Alex",
                    "Alex adopted a cat after leaving the flatmates.",
                    Add,
                ),
            ],
            current: vec![1, 2],
            superseded: vec![0],
            protected: vec![2],
        },
        Case {
            name: "other_subject_similar_fact",
            kind: "cross_subject",
            statements: vec![
                s("Jordan", "Jordan works at Globex as a designer.", Add),
                s("Alex", "Alex works at Globex as a designer.", Add),
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![0],
        },
        Case {
            name: "other_tenant_similar_fact",
            kind: "cross_tenant",
            statements: vec![
                S {
                    user: "u_other",
                    ..s("Alex", "Alex works at Acme as a designer.", Add)
                },
                s("Alex", "Alex works at Globex as a designer now.", Add),
            ],
            current: vec![0, 1],
            superseded: vec![],
            protected: vec![0],
        },
    ]
}

fn candidate(st: &S) -> CandidateFact {
    let event_time = st.year.map(|y| {
        TemporalValue::new(
            Utc.with_ymd_and_hms(y, 6, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(y, 6, 30, 23, 59, 59).unwrap(),
            TemporalGranularity::Month,
            Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap(),
        )
        .unwrap()
    });
    CandidateFact::new(st.text, st.kind, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some(st.subject.to_string()),
            event_time,
            ..Default::default()
        })
}

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_sma", user).unwrap()
}

#[derive(Default, Clone, serde::Serialize)]
struct Tally {
    cases: usize,
    statements: usize,
    classification_calls: usize,
    candidates_offered_any: usize,
    offered_facts_total: usize,
    correct_supersessions: usize,
    expected_supersessions: usize,
    incorrect_supersessions: usize,
    protected_suppressed: usize,
    current_state_correct: usize,
    history_complete: usize,
    supersession_linked: usize,
    hard_deletes: usize,
    other_tenant_changed: usize,
    final_state_correct: usize,
    by_kind: BTreeMap<&'static str, (usize, usize)>,
    /// Cases whose final state is wrong, with what was observed.
    failures: Vec<String>,
}

async fn run(matcher: RelatedFactMatcher, s1: bool, classifier: Classifier) -> Tally {
    let mut t = Tally::default();
    for case in cases() {
        let facts = Arc::new(MockFactStore::new());
        let llm = Arc::new(Llm {
            queue: Mutex::new(
                case.statements
                    .iter()
                    .map(|st| (candidate(st), st.label))
                    .collect(),
            ),
            current: Mutex::new(None),
            texts: Mutex::new(
                case.statements
                    .iter()
                    .map(|st| st.text.to_string())
                    .collect(),
            ),
            classifier,
            calls: Mutex::new(0),
            offers: Mutex::new(Vec::new()),
        });
        let engine = MemoryEngine::new(
            facts.clone(),
            Arc::new(MockVectorStore::new()),
            llm.clone(),
            Arc::new(LexicalEmbedding),
        )
        .with_related_fact_matcher(matcher)
        .with_classification_skip_when_no_related(s1);
        for st in &case.statements {
            engine
                .add_memory(AddMemoryInput {
                    tenant: tenant(st.user),
                    messages: vec![MemoryMessage {
                        role: MessageRole::User,
                        content: st.text.to_string(),
                        ..Default::default()
                    }],
                    metadata: json!({}),
                    session: Default::default(),
                })
                .await
                .expect("ingest");
        }
        // Stored rows of every user in the case (deleted included).
        let mut rows: BTreeMap<(String, String), Fact> = BTreeMap::new();
        let mut deleted = 0;
        for user in ["u_alex", "u_other"] {
            let q = |d| FactSearchQuery {
                tenant: tenant(user),
                memory_types: None,
                query_text: None,
                limit: 100,
                cursor: None,
                include_deleted: d,
            };
            let all = facts.search_facts(q(true)).await.unwrap();
            deleted += all.len() - facts.search_facts(q(false)).await.unwrap().len();
            for f in all {
                rows.insert((user.to_string(), f.content.clone()), f);
            }
        }
        let row = |i: usize| {
            rows.get(&(
                case.statements[i].user.to_string(),
                case.statements[i].text.to_string(),
            ))
        };
        let search = |history: bool| {
            let engine = &engine;
            async move {
                let mut out: Vec<String> = engine
                    .search_memory_with_options(
                        SearchMemoryInput {
                            tenant: tenant("u_alex"),
                            query: "Alex".to_string(),
                            limit: 50,
                            memory_types: None,
                            metadata_filter: None,
                        },
                        SearchOptions {
                            include_history: history,
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap()
                    .results
                    .into_iter()
                    .map(|r| r.content)
                    .collect();
                out.sort();
                out
            }
        };
        let current_search = search(false).await;
        let history_search = search(true).await;

        let superseded_now: Vec<usize> = (0..case.statements.len())
            .filter(|&i| row(i).is_some_and(|f| f.superseded_at.is_some()))
            .collect();
        let correct = superseded_now
            .iter()
            .filter(|i| case.superseded.contains(i))
            .count();
        let incorrect = superseded_now.len() - correct;
        let protected_hit = case
            .protected
            .iter()
            .filter(|i| superseded_now.contains(i))
            .count();
        let mut expect_current: Vec<String> = case
            .current
            .iter()
            .filter(|&&i| case.statements[i].user == "u_alex")
            .map(|&i| case.statements[i].text.to_string())
            .collect();
        expect_current.sort();
        // Stored, still-true statements must be what current search returns for the user.
        let current_ok = current_search == expect_current;
        let alex_texts: Vec<String> = {
            let mut v: Vec<String> = case
                .statements
                .iter()
                .filter(|st| st.user == "u_alex")
                .map(|st| st.text.to_string())
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let history_ok = alex_texts.iter().all(|text| history_search.contains(text));
        let linked = superseded_now
            .iter()
            .filter(|&&i| row(i).and_then(|f| f.superseded_by).is_some())
            .count();
        let other_changed = case
            .statements
            .iter()
            .enumerate()
            .filter(|(i, st)| st.user == "u_other" && row(*i).is_none_or(|f| !f.is_active()))
            .count();
        let final_ok =
            superseded_now == case.superseded && current_ok && history_ok && deleted == 0;

        t.cases += 1;
        t.statements += case.statements.len();
        t.classification_calls += *llm.calls.lock().unwrap();
        let offers = llm.offers.lock().unwrap().clone();
        t.candidates_offered_any += offers.iter().filter(|&&n| n > 0).count();
        t.offered_facts_total += offers.iter().sum::<usize>();
        t.correct_supersessions += correct;
        t.expected_supersessions += case.superseded.len();
        t.incorrect_supersessions += incorrect;
        t.protected_suppressed += protected_hit;
        t.current_state_correct += usize::from(current_ok);
        t.history_complete += usize::from(history_ok);
        t.supersession_linked += linked;
        t.hard_deletes += deleted;
        t.other_tenant_changed += other_changed;
        t.final_state_correct += usize::from(final_ok);
        let e = t.by_kind.entry(case.kind).or_default();
        e.0 += usize::from(final_ok);
        e.1 += 1;
        if !final_ok {
            t.failures.push(format!(
                "{}: superseded {:?} (expected {:?}), current_ok {}, history_ok {}, current {:?}",
                case.name, superseded_now, case.superseded, current_ok, history_ok, current_search
            ));
        }
    }
    t
}

#[tokio::test]
async fn semantic_matcher_acceptance_matrix() {
    let mut report = BTreeMap::new();
    for (name, matcher, s1) in [
        ("1_legacy_s1_off", RelatedFactMatcher::Legacy, false),
        ("2_legacy_s1_on", RelatedFactMatcher::Legacy, true),
        ("3_semantic_s1_off", RelatedFactMatcher::Semantic, false),
        ("4_semantic_s1_on", RelatedFactMatcher::Semantic, true),
    ] {
        for classifier in [Classifier::Oracle, Classifier::UpdateFirst] {
            if matcher == RelatedFactMatcher::Legacy && classifier == Classifier::UpdateFirst {
                continue; // nothing is ever offered: an Update without a target fails the request
            }
            let t = run(matcher, s1, classifier).await;
            // Never, in any configuration: a delete, another tenant's fact changed, a superseded
            // row without its replacement link, or history lost.
            assert_eq!(t.hard_deletes, 0, "{name}");
            assert_eq!(t.other_tenant_changed, 0, "{name}");
            assert_eq!(
                t.supersession_linked,
                t.correct_supersessions + t.incorrect_supersessions,
                "{name}"
            );
            assert_eq!(
                t.history_complete, t.cases,
                "{name}: history must stay complete"
            );
            let key = format!(
                "{name}_{}",
                if classifier == Classifier::Oracle {
                    "oracle"
                } else {
                    "update_first"
                }
            );
            report.insert(key, t);
        }
    }
    let o3 = &report["3_semantic_s1_off_oracle"];
    let o4 = &report["4_semantic_s1_on_oracle"];
    // A correct classifier never supersedes wrongly or suppresses a still-true statement; misses
    // (an update below the similarity floor) fall back to the legacy outcome and are reported.
    assert_eq!(o3.incorrect_supersessions, 0, "{:#?}", o3.failures);
    assert_eq!(o3.protected_suppressed, 0, "{:#?}", o3.failures);
    assert!(o3.final_state_correct > report["1_legacy_s1_off_oracle"].final_state_correct);
    assert_eq!(
        o4.final_state_correct, o3.final_state_correct,
        "S1 on keeps the semantic states"
    );
    let l1 = &report["1_legacy_s1_off_oracle"];
    let l2 = &report["2_legacy_s1_on_oracle"];
    assert_eq!(
        l1.final_state_correct, l2.final_state_correct,
        "S1 on keeps the legacy states"
    );
    assert_eq!(l1.correct_supersessions, 0, "legacy never supersedes");
    if std::env::var("SEMANTIC_ACCEPTANCE_METRICS").is_ok() {
        println!(
            "SEMANTIC_ACCEPTANCE_METRICS {}",
            serde_json::to_string(&report).unwrap()
        );
    }
}
