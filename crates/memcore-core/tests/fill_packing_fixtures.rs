//! The opt-in `fill` packing against the reference `prefix` on
//! independent, deterministic fixtures. No benchmark question, answer or text is used: each fixture is
//! a seeded synthetic ranked candidate list (the depth, 100 facts + 100 episodes merged by
//! score) with labelled relevant items. Line lengths follow a realistic shape of stored
//! lines (facts about 20-60 tokens; about half of the episodes over 400 tokens), which is the
//! failure class under study: a long line ending the rank-order prefix early.
//!
//! Families: `A` relevant items in the top 40, 75 % of them facts; `B` relevant items anywhere in
//! the top 100, half of them facts. Set `QUALITY_V1_METRICS=1` to print the metrics as JSON.

use std::time::Instant;

use memcore_core::CandidateKind;
use memcore_core::d76_context::{D76Line, D76Packing, count_tokens, render_packed};
use uuid::Uuid;

const BUDGET: usize = 1_500;
const DEPTH: usize = 200;
const CASES: u64 = 60;
const RELEVANT: usize = 3;

/// A small fixed LCG (the fixtures must not depend on a library's sampler).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// A fact (18-60 words) or an episode (half 60-400 words, half 400-1,600 words).
    fn length(&mut self, fact: bool) -> u64 {
        if fact {
            18 + self.below(43)
        } else if self.chance(50) {
            60 + self.below(341)
        } else {
            400 + self.below(1_201)
        }
    }
}

struct Case {
    lines: Vec<D76Line>,
    relevant: Vec<Uuid>,
}

fn kind(fact: bool) -> CandidateKind {
    if fact {
        CandidateKind::Fact
    } else {
        CandidateKind::Episode
    }
}

fn words(n: u64) -> String {
    "alpha ".repeat(n as usize).trim_end().to_string()
}

fn case(seed: u64, top: u64, fact_percent: u64) -> Case {
    let mut rng = Lcg(seed);
    let mut lines = Vec::with_capacity(DEPTH);
    for rank in 0..DEPTH {
        let fact = rng.chance(50);
        let length = rng.length(fact);
        lines.push(D76Line {
            kind: kind(fact),
            id: Uuid::from_u128(u128::from(seed) * 1_000 + rank as u128),
            vector_score: 0.9 - rank as f32 / 1_000.0,
            timestamp: "9:15 am on 3 March, 2031".to_string(),
            text: format!("c{rank} {}", words(length)),
        });
    }
    // Relevant items at distinct ranks in the top `top`; each one's kind and length are redrawn so
    // that `fact_percent` of them are facts.
    let mut relevant = Vec::new();
    while relevant.len() < RELEVANT {
        let rank = rng.below(top) as usize;
        let id = lines[rank].id;
        if relevant.contains(&id) {
            continue;
        }
        let fact = rng.chance(fact_percent);
        let length = rng.length(fact);
        lines[rank].kind = kind(fact);
        lines[rank].text = format!("c{rank} relevant {}", words(length));
        relevant.push(id);
    }
    Case { lines, relevant }
}

#[derive(Default)]
struct Totals {
    relevant: usize,
    relevant_in_candidates: usize,
    relevant_in_context: usize,
    irrelevant_in_context: usize,
    tokens: usize,
    contexts: usize,
    micros: u128,
    contexts_with_all_relevant: usize,
}

fn evaluate(family: &str, top: u64, fact_percent: u64) -> (Totals, Totals) {
    let (mut prefix, mut fill) = (Totals::default(), Totals::default());
    for seed in 1..=CASES {
        let case = case(seed * 7_919 + top, top, fact_percent);
        let in_candidates = case
            .relevant
            .iter()
            .filter(|id| case.lines.iter().any(|line| line.id == **id))
            .count();
        let mut ids = Vec::new();
        for (packing, totals) in [
            (D76Packing::Prefix, &mut prefix),
            (D76Packing::Fill, &mut fill),
        ] {
            let started = Instant::now();
            let (context, _) = render_packed(&case.lines, BUDGET, packing);
            totals.micros += started.elapsed().as_micros();
            assert!(context.token_count <= BUDGET, "{family}: budget");
            assert_eq!(
                context.token_count,
                count_tokens(&context.text),
                "{family}: count"
            );
            let hits = context
                .items
                .iter()
                .filter(|item| case.relevant.contains(&item.id))
                .count();
            totals.relevant += case.relevant.len();
            totals.relevant_in_candidates += in_candidates;
            totals.relevant_in_context += hits;
            totals.irrelevant_in_context += context.items.len() - hits;
            totals.tokens += context.token_count;
            totals.contexts += 1;
            totals.contexts_with_all_relevant += usize::from(hits == case.relevant.len());
            ids.push(context.items.iter().map(|item| item.id).collect::<Vec<_>>());
        }
        // Per case: fill keeps every prefix item, in order, so it never loses a relevant item.
        assert_eq!(
            &ids[1][..ids[0].len()],
            ids[0].as_slice(),
            "{family}: prefix kept"
        );
    }
    (prefix, fill)
}

fn metrics(t: &Totals) -> serde_json::Value {
    let per = |x: usize| x as f64 / t.contexts as f64;
    serde_json::json!({
        "relevant_evidence_recall": t.relevant_in_context as f64 / t.relevant as f64,
        "candidate_retrieval_coverage": t.relevant_in_candidates as f64 / t.relevant as f64,
        "context_inclusion_coverage": t.relevant_in_context as f64 / t.relevant_in_candidates as f64,
        "contexts_with_all_relevant": per(t.contexts_with_all_relevant),
        "irrelevant_items_per_context": per(t.irrelevant_in_context),
        "context_tokens_mean": per(t.tokens),
        "render_micros_mean_debug_build": t.micros as f64 / t.contexts as f64,
    })
}

#[test]
fn fill_packing_on_independent_fixtures() {
    let mut report = serde_json::Map::new();
    for (family, top, fact_percent) in [("A", 40, 75), ("B", 100, 50)] {
        let (prefix, fill) = evaluate(family, top, fact_percent);
        assert!(
            fill.relevant_in_context >= prefix.relevant_in_context,
            "{family}: fill never lowers recall"
        );
        let delta = (fill.tokens as f64 - prefix.tokens as f64) / fill.contexts as f64;
        report.insert(
            family.to_string(),
            serde_json::json!({
                "cases": CASES, "budget": BUDGET, "depth": DEPTH, "relevant_per_case": RELEVANT,
                "relevant_rank_top": top, "relevant_fact_percent": fact_percent,
                "prefix": metrics(&prefix), "fill": metrics(&fill),
                "answer_input_tokens_delta_per_question": delta,
                "answer_cost_delta_per_1000_questions_nova_lite_usd": delta * 1_000.0 * 0.06 / 1e6,
            }),
        );
    }
    if std::env::var("QUALITY_V1_METRICS").is_ok() {
        println!("QUALITY_V1_METRICS {}", serde_json::Value::Object(report));
    }
}
