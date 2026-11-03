//! Controlled memory-lifecycle suite. Invented data; no benchmark text;
//! no model calls.
//!
//! Every scenario runs through `MemoryEngine::add_memory` / `search_memory_with_options` with a
//! scripted extractor (the candidates of each request are fixed) and an **oracle classifier** (it
//! applies the labelled correct decision when its labelled target is among the facts it is offered,
//! and adds otherwise - a classifier cannot update what it was not shown). So the suite measures the
//! deterministic lifecycle - related-fact lookup, decision guard, persistence, retrieval - and NOT the
//! decision quality of a real model, which stays unmeasured here.
//!
//! Arms: related-fact matcher {legacy (default), semantic (opt-in)} x classification skip
//! {off (default), on}. Embedding dedup is exact-text only, as in the OpenAI profile (uncalibrated
//! model). Embeddings are a deterministic bag-of-words vector (cosine = word overlap).
//!
//! The test asserts invariants that must hold in every arm (tenant isolation, no deletion without a
//! labelled decision, S1 on == S1 off, deterministic replay) and pins the measured pass/fail table,
//! so a behaviour change is a visible test change. `MEMCORE_V2_LIFECYCLE_OUT=<path>` writes the
//! per-case results as JSON.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider,
    SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, Fact, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, FactStructure, MemoryEngine,
    MemoryMessage, MemoryType, MessageRole, RelatedFactMatcher, RetrievalMode, SearchMemoryInput,
    SearchOptions, TemporalGranularity, TemporalValue, TenantContext,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::{Value, json};

const DIMS: usize = 256;

struct LexicalEmbedding;

/// The candidate embedding of this text fails once: a transport fault injected at a stage every
/// candidate passes before any decision (classification itself may be skipped by S1).
static EMBED_FAIL: Mutex<Option<String>> = Mutex::new(None);

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
        {
            let mut fail = EMBED_FAIL.lock().unwrap();
            if fail.as_deref() == Some(text) {
                *fail = None;
                return Err(MemcoreError::ProviderError(
                    "connection reset (injected)".to_string(),
                ));
            }
        }
        Ok(lexical(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| lexical(t)).collect())
    }
    fn dimensions(&self) -> usize {
        DIMS
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
#[allow(dead_code)] // Delete is in the oracle vocabulary; no current scenario labels one
enum Label {
    Add,
    NoOp,
    Update,
    Delete,
}

/// One candidate fact of a request: text, memory type, subject, optional event year, and the
/// labelled decision (with the text of its target, if any).
#[derive(Clone)]
struct C {
    text: &'static str,
    kind: MemoryType,
    subject: &'static str,
    year: Option<i32>,
    label: Label,
    target: Option<&'static str>,
}

const fn add(text: &'static str, kind: MemoryType) -> C {
    C {
        text,
        kind,
        subject: "Maya",
        year: None,
        label: Label::Add,
        target: None,
    }
}

const fn upd(text: &'static str, kind: MemoryType, target: &'static str) -> C {
    C {
        text,
        kind,
        subject: "Maya",
        year: None,
        label: Label::Update,
        target: Some(target),
    }
}

const fn noop(text: &'static str, kind: MemoryType, target: &'static str) -> C {
    C {
        text,
        kind,
        subject: "Maya",
        year: None,
        label: Label::NoOp,
        target: Some(target),
    }
}

#[derive(Clone)]
struct Req {
    user: &'static str,
    candidates: Vec<C>,
    /// Classification of this candidate text fails with a transport error (once).
    fail_on: Option<&'static str>,
}

fn req(user: &'static str, candidates: Vec<C>) -> Req {
    Req {
        user,
        candidates,
        fail_on: None,
    }
}

struct Llm {
    queue: Mutex<Vec<Vec<C>>>,
    labels: Mutex<BTreeMap<String, (Label, Option<String>)>>,
    fail_on: Mutex<Option<String>>,
    classify_calls: Mutex<usize>,
    offered: Mutex<usize>,
}

fn candidate(c: &C) -> CandidateFact {
    let event_time = c.year.map(|y| {
        TemporalValue::new(
            Utc.with_ymd_and_hms(y, 6, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(y, 6, 30, 23, 59, 59).unwrap(),
            TemporalGranularity::Month,
            Utc.with_ymd_and_hms(2024, 1, 10, 0, 0, 0).unwrap(),
        )
        .unwrap()
    });
    CandidateFact::new(c.text, c.kind, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some(c.subject.to_string()),
            event_time,
            ..Default::default()
        })
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut q = self.queue.lock().unwrap();
        let next = if q.is_empty() {
            Vec::new()
        } else {
            q.remove(0)
        };
        Ok(FactExtractionOutput::from_candidates(
            next.iter().map(candidate).collect(),
        ))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        *self.classify_calls.lock().unwrap() += 1;
        if !input.existing_facts.is_empty() {
            *self.offered.lock().unwrap() += 1;
        }
        let text = input.candidate_fact.content.clone();
        {
            let mut fail = self.fail_on.lock().unwrap();
            if fail.as_deref() == Some(text.as_str()) {
                *fail = None;
                return Err(MemcoreError::ProviderError(
                    "connection reset (injected)".to_string(),
                ));
            }
        }
        let (label, target) = self
            .labels
            .lock()
            .unwrap()
            .get(&text)
            .cloned()
            .unwrap_or((Label::Add, None));
        let found = target.and_then(|t| input.existing_facts.iter().find(|f| f.content == t));
        let (operation, target) = match (label, found) {
            (Label::Add, _) | (_, None) => (FactOperation::Add, None),
            (Label::NoOp, Some(f)) => (FactOperation::NoOp, Some(f.id)),
            (Label::Update, Some(f)) => (FactOperation::Update, Some(f.id)),
            (Label::Delete, Some(f)) => (FactOperation::Delete, Some(f.id)),
        };
        Ok(FactOperationDecision {
            operation,
            target_fact_id: target,
            reason: Some("oracle".into()),
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

struct World {
    facts: Arc<MockFactStore>,
    llm: Arc<Llm>,
    engine: MemoryEngine,
}

impl World {
    fn new(matcher: RelatedFactMatcher, s1: bool) -> Self {
        let facts = Arc::new(MockFactStore::new());
        let llm = Arc::new(Llm {
            queue: Mutex::new(Vec::new()),
            labels: Mutex::new(BTreeMap::new()),
            fail_on: Mutex::new(None),
            classify_calls: Mutex::new(0),
            offered: Mutex::new(0),
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
        .with_related_fact_matcher(matcher)
        .with_classification_skip_when_no_related(s1);
        Self { facts, llm, engine }
    }

    async fn send(&self, org: &str, r: &Req) -> MemcoreResult<()> {
        {
            let mut labels = self.llm.labels.lock().unwrap();
            for c in &r.candidates {
                labels.insert(c.text.to_string(), (c.label, c.target.map(str::to_string)));
            }
        }
        *EMBED_FAIL.lock().unwrap() = r.fail_on.map(str::to_string);
        self.llm.queue.lock().unwrap().push(r.candidates.clone());
        self.engine
            .add_memory(AddMemoryInput {
                tenant: tenant(org, r.user),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: r
                        .candidates
                        .iter()
                        .map(|c| c.text)
                        .collect::<Vec<_>>()
                        .join(" "),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .map(|_| ())
    }

    /// Every stored row of a user: content -> (active, superseded, deleted), plus row count.
    async fn rows(&self, org: &str, user: &str) -> Vec<Fact> {
        self.facts
            .search_facts(FactSearchQuery {
                tenant: tenant(org, user),
                memory_types: None,
                query_text: None,
                limit: 200,
                cursor: None,
                include_deleted: true,
            })
            .await
            .unwrap()
    }

    /// Active facts: in the live query (not deleted; the mock keeps deletions apart from the row)
    /// and not superseded / invalidated.
    async fn active(&self, org: &str, user: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .facts
            .search_facts(FactSearchQuery {
                tenant: tenant(org, user),
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

    async fn search(&self, org: &str, user: &str, query: &str, history: bool) -> Vec<String> {
        let out = self
            .engine
            .search_memory_with_options(
                SearchMemoryInput {
                    tenant: tenant(org, user),
                    query: query.to_string(),
                    limit: 10,
                    memory_types: None,
                    metadata_filter: None,
                },
                SearchOptions {
                    mode: Some(RetrievalMode::Semantic),
                    user_names: Vec::new(),
                    include_history: history,
                },
            )
            .await
            .unwrap();
        out.results.into_iter().map(|r| r.content).collect()
    }
}

const ORG: &str = "org_v2_life";
const P: MemoryType = MemoryType::Profile;
const PREF: MemoryType = MemoryType::Preference;

#[allow(clippy::type_complexity)]
struct Scenario {
    name: &'static str,
    property: &'static str,
    requests: Vec<Req>,
    /// (check name, passed)
    check: for<'a> fn(
        &'a World,
        Vec<MemcoreResult<()>>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Vec<(&'static str, bool)>> + 'a>,
    >,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "new_fact",
            property: "fact is stored and retrievable",
            requests: vec![req(
                "maya",
                vec![add("Maya adopted a grey cat named Pixel.", P)],
            )],
            check: |w, r| {
                Box::pin(async move {
                    vec![
                        ("request ok", r.iter().all(|x| x.is_ok())),
                        (
                            "stored active",
                            w.active(ORG, "maya").await
                                == vec!["Maya adopted a grey cat named Pixel.".to_string()],
                        ),
                        (
                            "retrievable",
                            w.search(ORG, "maya", "Maya cat Pixel", false)
                                .await
                                .first()
                                .map(String::as_str)
                                == Some("Maya adopted a grey cat named Pixel."),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "exact_repetition",
            property: "no unnecessary duplicate",
            requests: vec![
                req("maya", vec![add("Maya plays the cello on Sundays.", PREF)]),
                req("maya", vec![add("Maya plays the cello on Sundays.", PREF)]),
            ],
            check: |w, _| {
                Box::pin(async move {
                    vec![("one active fact", w.active(ORG, "maya").await.len() == 1)]
                })
            },
        },
        Scenario {
            name: "paraphrased_repetition",
            property: "existing fact preserved, no duplicate",
            requests: vec![
                req("maya", vec![add("Maya lives in Paris.", P)]),
                req(
                    "maya",
                    vec![noop(
                        "Maya lives in the city of Paris.",
                        P,
                        "Maya lives in Paris.",
                    )],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    vec![
                        (
                            "original kept",
                            w.active(ORG, "maya")
                                .await
                                .contains(&"Maya lives in Paris.".to_string()),
                        ),
                        ("no duplicate", w.active(ORG, "maya").await.len() == 1),
                    ]
                })
            },
        },
        Scenario {
            name: "corrected_fact",
            property: "current value reflects the correction",
            requests: vec![
                req("maya", vec![add("Maya lives in Paris.", P)]),
                req(
                    "maya",
                    vec![upd("Maya lives in Berlin now.", P, "Maya lives in Paris.")],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let a = w.active(ORG, "maya").await;
                    vec![
                        (
                            "new value active",
                            a.contains(&"Maya lives in Berlin now.".to_string()),
                        ),
                        (
                            "old value not active",
                            !a.contains(&"Maya lives in Paris.".to_string()),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "contradiction",
            property: "the later statement wins; one current value",
            requests: vec![
                req("maya", vec![add("Maya is a vegetarian.", PREF)]),
                req(
                    "maya",
                    vec![upd(
                        "Maya is no longer a vegetarian.",
                        PREF,
                        "Maya is a vegetarian.",
                    )],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let a = w.active(ORG, "maya").await;
                    vec![
                        (
                            "latest active",
                            a.contains(&"Maya is no longer a vegetarian.".to_string()),
                        ),
                        (
                            "contradicted not active",
                            !a.contains(&"Maya is a vegetarian.".to_string()),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "historical_fact",
            property: "previous value remains historically interpretable",
            requests: vec![
                req("maya", vec![add("Maya works at Acme Labs.", P)]),
                req(
                    "maya",
                    vec![upd(
                        "Maya works at Globex Labs now.",
                        P,
                        "Maya works at Acme Labs.",
                    )],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let rows = w.rows(ORG, "maya").await;
                    vec![
                        (
                            "old value kept as an inactive row",
                            rows.iter()
                                .any(|f| f.content == "Maya works at Acme Labs." && !f.is_active()),
                        ),
                        (
                            "new value active",
                            rows.iter().any(|f| {
                                f.content == "Maya works at Globex Labs now." && f.is_active()
                            }),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "latest_value_query",
            property: "retrieval returns the current value, not the stale one",
            requests: vec![
                req("maya", vec![add("Maya lives in Paris.", P)]),
                req(
                    "maya",
                    vec![upd("Maya lives in Berlin now.", P, "Maya lives in Paris.")],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let s = w.search(ORG, "maya", "where does Maya live", false).await;
                    vec![
                        (
                            "current value retrieved",
                            s.contains(&"Maya lives in Berlin now.".to_string()),
                        ),
                        (
                            "stale value not retrieved",
                            !s.contains(&"Maya lives in Paris.".to_string()),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "past_state_query",
            property: "history retrieval returns the earlier value",
            requests: vec![
                req("maya", vec![add("Maya lives in Paris.", P)]),
                req(
                    "maya",
                    vec![upd("Maya lives in Berlin now.", P, "Maya lives in Paris.")],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let s = w.search(ORG, "maya", "where did Maya live", true).await;
                    vec![(
                        "earlier value retrievable with history",
                        s.contains(&"Maya lives in Paris.".to_string()),
                    )]
                })
            },
        },
        Scenario {
            name: "changed_preference",
            property: "latest preference without erasing history",
            requests: vec![
                req("maya", vec![add("Maya prefers tea in the morning.", PREF)]),
                req(
                    "maya",
                    vec![upd(
                        "Maya prefers coffee in the morning now.",
                        PREF,
                        "Maya prefers tea in the morning.",
                    )],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let rows = w.rows(ORG, "maya").await;
                    let a = w.active(ORG, "maya").await;
                    vec![
                        (
                            "latest preference active",
                            a.contains(&"Maya prefers coffee in the morning now.".to_string()),
                        ),
                        (
                            "old preference not active",
                            !a.contains(&"Maya prefers tea in the morning.".to_string()),
                        ),
                        (
                            "old preference kept as history",
                            rows.iter()
                                .any(|f| f.content == "Maya prefers tea in the morning."),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "explicit_negation",
            property: "a negation is not stored as a positive fact",
            requests: vec![req("maya", vec![add("Maya does not eat meat.", PREF)])],
            check: |w, _| {
                Box::pin(async move {
                    let a = w.active(ORG, "maya").await;
                    vec![(
                        "negation text intact",
                        a == vec!["Maya does not eat meat.".to_string()],
                    )]
                })
            },
        },
        Scenario {
            name: "multiple_related_facts",
            property: "one update does not corrupt unrelated facts",
            requests: vec![
                req(
                    "maya",
                    vec![
                        add("Maya lives in Paris.", P),
                        add("Maya works at Acme Labs.", P),
                        add("Maya has a sister named Ines.", P),
                    ],
                ),
                req(
                    "maya",
                    vec![upd("Maya lives in Berlin now.", P, "Maya lives in Paris.")],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let a = w.active(ORG, "maya").await;
                    vec![
                        (
                            "work fact intact",
                            a.contains(&"Maya works at Acme Labs.".to_string()),
                        ),
                        (
                            "family fact intact",
                            a.contains(&"Maya has a sister named Ines.".to_string()),
                        ),
                        (
                            "city updated",
                            a.contains(&"Maya lives in Berlin now.".to_string())
                                && !a.contains(&"Maya lives in Paris.".to_string()),
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "out_of_order_events",
            property: "event time and observation time distinguished; an older event does not replace a newer one",
            requests: vec![
                req(
                    "maya",
                    vec![C {
                        year: Some(2022),
                        ..add(
                            "Maya ran the Berlin marathon in June 2022.",
                            MemoryType::Event,
                        )
                    }],
                ),
                req(
                    "maya",
                    vec![C {
                        year: Some(2019),
                        label: Label::Update,
                        target: Some("Maya ran the Berlin marathon in June 2022."),
                        ..add(
                            "Maya ran the Paris marathon in June 2019.",
                            MemoryType::Event,
                        )
                    }],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    let rows = w.rows(ORG, "maya").await;
                    let a = w.active(ORG, "maya").await;
                    vec![
                        ("both events active", a.len() == 2),
                        (
                            "event times preserved",
                            rows.iter()
                                .filter(|f| f.structure.event_time.is_some())
                                .count()
                                == 2,
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "idempotent_replay",
            property: "a repeated request does not duplicate memory",
            requests: vec![
                req(
                    "maya",
                    vec![
                        add("Maya bought a red bicycle.", P),
                        add("Maya named the bicycle Comet.", P),
                    ],
                ),
                req(
                    "maya",
                    vec![
                        add("Maya bought a red bicycle.", P),
                        add("Maya named the bicycle Comet.", P),
                    ],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    vec![("two active facts", w.active(ORG, "maya").await.len() == 2)]
                })
            },
        },
        Scenario {
            name: "failed_request_and_retry",
            property: "state consistent after a failure and a retry; no duplicate",
            requests: vec![
                Req {
                    fail_on: Some("Maya repairs old wooden canoes."),
                    ..req(
                        "maya",
                        vec![
                            add("Maya keeps a pottery wheel in the garage.", P),
                            add("Maya repairs old wooden canoes.", PREF),
                        ],
                    )
                },
                // the retry re-extracts: the first candidate comes back paraphrased, as a real model may
                req(
                    "maya",
                    vec![
                        noop(
                            "Maya keeps a pottery wheel in her garage.",
                            P,
                            "Maya keeps a pottery wheel in the garage.",
                        ),
                        add("Maya repairs old wooden canoes.", PREF),
                    ],
                ),
            ],
            check: |w, r| {
                Box::pin(async move {
                    let a = w.active(ORG, "maya").await;
                    vec![
                        ("failure reported", r[0].is_err()),
                        ("retry ok", r[1].is_ok()),
                        (
                            "pottery fact stored once",
                            a.iter().filter(|t| t.contains("pottery wheel")).count() == 1,
                        ),
                        (
                            "canoe fact stored once",
                            a.iter().filter(|t| t.contains("canoes")).count() == 1,
                        ),
                    ]
                })
            },
        },
        Scenario {
            name: "cross_tenant_isolation",
            property: "one tenant's update never affects another",
            requests: vec![
                req("maya", vec![add("Maya lives in Paris.", P)]),
                req("maya_b", vec![add("Maya lives in Paris.", P)]),
                req(
                    "maya_b",
                    vec![upd("Maya lives in Berlin now.", P, "Maya lives in Paris.")],
                ),
            ],
            check: |w, _| {
                Box::pin(async move {
                    vec![
                        (
                            "tenant A unchanged",
                            w.active(ORG, "maya").await == vec!["Maya lives in Paris.".to_string()],
                        ),
                        (
                            "tenant A search unchanged",
                            !w.search(ORG, "maya", "Maya Berlin", false)
                                .await
                                .contains(&"Maya lives in Berlin now.".to_string()),
                        ),
                    ]
                })
            },
        },
    ]
}

async fn run(
    matcher: RelatedFactMatcher,
    s1: bool,
) -> (
    Vec<Value>,
    usize,
    usize,
    BTreeMap<String, Vec<(String, bool)>>,
) {
    let mut out = Vec::new();
    let (mut calls, mut offered) = (0, 0);
    let mut states = BTreeMap::new();
    for sc in scenarios() {
        let w = World::new(matcher, s1);
        let mut results = Vec::new();
        for r in &sc.requests {
            results.push(w.send(ORG, r).await);
        }
        let checks = (sc.check)(&w, results).await;
        let mut state: Vec<(String, bool)> = Vec::new();
        for user in ["maya", "maya_b"] {
            let live = w.active(ORG, user).await;
            for f in w.rows(ORG, user).await {
                let active = live.contains(&f.content);
                state.push((format!("{user}|{}", f.content), active));
            }
        }
        state.sort();
        states.insert(sc.name.to_string(), state);
        calls += *w.llm.classify_calls.lock().unwrap();
        offered += *w.llm.offered.lock().unwrap();
        out.push(json!({"scenario": sc.name, "property": sc.property,
                        "checks": checks.iter().map(|(n, p)| json!({"check": n, "passed": p})).collect::<Vec<_>>(),
                        "passed": checks.iter().all(|(_, p)| *p)}));
    }
    (out, calls, offered, states)
}

#[tokio::test]
async fn controlled_lifecycle_suite() {
    let mut arms = serde_json::Map::new();
    let mut summary = BTreeMap::new();
    let mut states = BTreeMap::new();
    for (mname, matcher) in [
        ("legacy", RelatedFactMatcher::Legacy),
        ("semantic", RelatedFactMatcher::Semantic),
        ("semantic_v23", RelatedFactMatcher::SemanticV23),
    ] {
        for s1 in [false, true] {
            let (cases, calls, offered, st) = run(matcher, s1).await;
            let passed = cases.iter().filter(|c| c["passed"] == true).count();
            let arm = format!("{mname}/s1={}", if s1 { "on" } else { "off" });
            summary.insert(
                arm.clone(),
                cases
                    .iter()
                    .map(|c| {
                        (
                            c["scenario"].as_str().unwrap().to_string(),
                            c["passed"] == true,
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
            );
            states.insert(arm.clone(), st);
            arms.insert(arm, json!({"cases": cases, "passed": passed, "total": cases.len(), "classify_calls": calls,
                                     "classify_calls_with_related_offered": offered}));
        }
    }
    // S1 must not change any final state, under either matcher.
    for m in ["legacy", "semantic", "semantic_v23"] {
        assert_eq!(
            states[&format!("{m}/s1=off")],
            states[&format!("{m}/s1=on")],
            "S1 changed the memory state ({m})"
        );
    }
    // Deterministic: a second run gives the same states.
    let (_, _, _, again) = run(RelatedFactMatcher::Semantic, false).await;
    assert_eq!(again, states["semantic/s1=off"]);
    // Invariants in every arm.
    for (arm, table) in &summary {
        assert!(table["cross_tenant_isolation"], "{arm}: tenant isolation");
        assert!(table["explicit_negation"], "{arm}: negation integrity");
        assert!(
            table["idempotent_replay"],
            "{arm}: replay duplicated memory"
        );
        assert!(
            table["exact_repetition"],
            "{arm}: exact repetition duplicated memory"
        );
    }
    if let Ok(dest) = std::env::var("MEMCORE_V2_LIFECYCLE_OUT") {
        std::fs::write(
            dest,
            serde_json::to_string_pretty(&Value::Object(arms)).unwrap(),
        )
        .unwrap();
    }
    for (arm, table) in &summary {
        let failed: Vec<&String> = table.iter().filter(|(_, p)| !**p).map(|(n, _)| n).collect();
        eprintln!(
            "{arm}: {}/{} passed; failed: {failed:?}",
            table.len() - failed.len(),
            table.len()
        );
    }
}
