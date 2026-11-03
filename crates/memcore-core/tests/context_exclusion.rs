//! Cross-user exact episode exclusion in the server-side context.
//!
//! A conversation stored for both participants keeps each message as an episode of both memory
//! users, so an answer prompt that concatenates both users' contexts repeats those episodes verbatim
//! (a sizeable share of the context in realistic conversations).
//! `exclude_message_ids` removes, from one user's candidates, the episodes of client messages already
//! present in another context: exact message-id identity, facts never removed, order unchanged, then
//! the same packing rule.

use memcore_core::candidates::{Candidate, CandidateKind};
use memcore_core::d76_context::{
    D76Line, count_tokens, exclude_episodes_by_message_id, item_provenance, render,
};
use serde_json::Value;
use uuid::Uuid;

fn cand(
    kind: CandidateKind,
    n: u128,
    content: &str,
    msg: Option<&str>,
    sources: &[&str],
) -> Candidate {
    Candidate {
        kind,
        id: Uuid::from_u128(n),
        content: content.to_string(),
        vector_score: 1.0 - n as f32 / 100.0,
        rank: n as usize,
        source_message_ids: sources.iter().map(|s| s.to_string()).collect(),
        message_id: msg.map(str::to_string),
        session_id: None,
        speaker: (kind == CandidateKind::Episode).then(|| "Avery".to_string()),
        metadata: Value::Null,
        event_time: None,
        observed_at: None,
        query_index: 0,
        rerank_score: None,
    }
}

fn sample() -> Vec<Candidate> {
    vec![
        cand(
            CandidateKind::Fact,
            1,
            "Avery keeps a pottery wheel.",
            None,
            &["m1"],
        ),
        cand(
            CandidateKind::Episode,
            2,
            "I keep a pottery wheel in the garage.",
            Some("m1"),
            &[],
        ),
        cand(
            CandidateKind::Episode,
            3,
            "Pebble the rabbit ate my notes.",
            Some("m2"),
            &[],
        ),
        cand(
            CandidateKind::Fact,
            4,
            "Avery adopted a rabbit named Pebble.",
            None,
            &["m2"],
        ),
        cand(
            CandidateKind::Episode,
            5,
            "Same words, different message.",
            Some("m3"),
            &[],
        ),
    ]
}

#[test]
fn no_exclusion_is_the_identity() {
    let (kept, removed) = exclude_episodes_by_message_id(sample(), &[]);
    assert_eq!(removed, 0);
    assert_eq!(kept, sample());
}

#[test]
fn only_episodes_with_an_excluded_message_id_are_removed() {
    let (kept, removed) =
        exclude_episodes_by_message_id(sample(), &["m1".to_string(), "m9".to_string()]);
    assert_eq!(removed, 1);
    let ids: Vec<u128> = kept.iter().map(|c| c.id.as_u128()).collect();
    // the fact citing m1 stays; order is unchanged
    assert_eq!(ids, vec![1, 3, 4, 5]);
}

#[test]
fn facts_are_never_removed_even_when_their_source_is_excluded() {
    let all: Vec<String> = ["m1", "m2", "m3"].iter().map(|s| s.to_string()).collect();
    let (kept, removed) = exclude_episodes_by_message_id(sample(), &all);
    assert_eq!(removed, 3);
    assert!(kept.iter().all(|c| c.kind == CandidateKind::Fact));
    assert_eq!(kept.len(), 2);
}

#[test]
fn exclusion_is_exact_not_normalised() {
    let (_, removed) =
        exclude_episodes_by_message_id(sample(), &["M1".to_string(), " m1".to_string()]);
    assert_eq!(removed, 0, "ids are compared exactly");
}

#[test]
fn freed_budget_is_backfilled_by_the_same_prefix_rule_and_provenance_is_kept() {
    let base: Vec<D76Line> = sample().iter().map(D76Line::from_candidate).collect();
    // a budget that holds three lines
    let three = base[..3]
        .iter()
        .map(|l| l.line())
        .collect::<Vec<_>>()
        .join("\n");
    let budget = count_tokens(&three);
    let before = render(&base, budget);
    assert_eq!(before.items.len(), 3);
    let (kept, _) = exclude_episodes_by_message_id(sample(), &["m1".to_string()]);
    let lines: Vec<D76Line> = kept.iter().map(D76Line::from_candidate).collect();
    let after = render(&lines, budget);
    let after_ids: Vec<u128> = after.items.iter().map(|i| i.id.as_u128()).collect();
    assert!(after_ids.starts_with(&[1, 3]), "{after_ids:?}");
    assert!(after.token_count <= budget);
    let prov = item_provenance(&after, &kept);
    assert_eq!(prov.len(), after.items.len());
    assert_eq!(prov[0].source_message_ids, vec!["m1".to_string()]);
    assert_eq!(prov[1].message_id.as_deref(), Some("m2"));
    // deterministic
    assert_eq!(render(&lines, budget), after);
}
