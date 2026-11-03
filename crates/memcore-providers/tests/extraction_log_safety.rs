//! Extraction diagnostics must be observable without leaking what was memorised.
//!
//! A skip counter that says only "something was rejected" is not diagnosable, so the logs name the
//! value that failed. That value is untrusted model output: it can echo conversation text into any
//! field, be steered there by prompt injection, or simply be malformed. These tests pin the
//! boundary — enough detail to diagnose, never the content itself, and never an unbounded value.

use memcore_core::{CandidateSkipReason, FactDiscardReason};
use memcore_providers::parse_fact_extraction_response;

/// A sentence that looks like something a user would actually say, used wherever a test needs to
/// prove that private text does not survive into a diagnostic.
const PRIVATE_SENTENCE: &str = "Priya said her mother lives at 14 Elmwood Terrace and that the \
                                biopsy results are due on Tuesday, and she asked me not to repeat it.";

fn extraction_json(memory_type: &str, valid_at: &str, content: &str) -> String {
    format!(
        r#"{{"facts":[{{"content":{content:?},"memory_type":{memory_type:?},"confidence":0.9,"importance":0.8,"valid_at":{valid_at},"metadata":{{}}}}]}}"#
    )
}

/// Every label that can reach Prometheus comes from a closed enum. Metric cardinality is therefore
/// a property of the code, not of what a model happens to return: a model inventing a thousand
/// memory-type labels adds a thousand log lines but not one new time series.
#[test]
fn metric_label_values_are_a_closed_bounded_set() {
    let skip_labels: Vec<&str> = CandidateSkipReason::ALL
        .iter()
        .map(|reason| reason.as_str())
        .collect();
    assert_eq!(
        skip_labels,
        vec!["unknown_memory_type", "invalid_candidate"]
    );

    let discard_labels: Vec<&str> = FactDiscardReason::ALL
        .iter()
        .map(|reason| reason.as_str())
        .collect();
    assert_eq!(
        discard_labels,
        vec![
            "low_importance",
            "duplicate_content",
            "duplicate_embedding",
            "no_op_decision",
            "invalid_classification",
        ]
    );

    for label in skip_labels.iter().chain(discard_labels.iter()) {
        assert!(
            label.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
            "{label} is not a stable low-cardinality label"
        );
    }
}

/// The counters must still be exact when a model returns something abusive, because the whole
/// point of the accounting is that no fact goes missing unrecorded.
#[test]
fn a_hostile_response_is_still_counted_exactly() {
    let json = format!(
        r#"{{"facts":[
            {{"content":"Good.","memory_type":"Knowledge","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{{}}}},
            {{"content":"Bad label.","memory_type":{PRIVATE_SENTENCE:?},"confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{{}}}},
            {{"content":"","memory_type":"Knowledge","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{{}}}}
        ]}}"#
    );

    let output = parse_fact_extraction_response(&json).expect("response should parse");

    assert_eq!(output.candidates.len(), 1);
    assert_eq!(
        output.skipped.get(CandidateSkipReason::UnknownMemoryType),
        1
    );
    assert_eq!(output.skipped.get(CandidateSkipReason::InvalidCandidate), 1);
    assert_eq!(output.returned_by_model(), 3);
}

/// A rejected fact's own content is never an input to any diagnostic — only its memory type and
/// its `valid_at` are, and both are bounded. This checks the values that carry through to the
/// caller, which is what the API response and the metrics are built from.
#[test]
fn nothing_returned_to_the_caller_carries_rejected_content() {
    let json = extraction_json("Casserole", "null", PRIVATE_SENTENCE);

    let output = parse_fact_extraction_response(&json).expect("response should parse");

    assert!(output.candidates.is_empty());
    let rendered = format!("{output:?}");
    assert!(
        !rendered.contains("Elmwood"),
        "a skipped fact's content must not survive in the extraction output"
    );
    assert!(!rendered.contains("biopsy"));
}

/// An oversized or structured `valid_at` must not become a channel for content, whatever shape the
/// model uses.
#[test]
fn unusable_valid_at_values_of_every_shape_are_handled_without_failing() {
    for valid_at in [
        format!("{PRIVATE_SENTENCE:?}"),
        format!(r#"{{"note":{PRIVATE_SENTENCE:?}}}"#),
        format!(r#"[{PRIVATE_SENTENCE:?}]"#),
        "true".to_string(),
        format!("{:?}", "x".repeat(10_000)),
    ] {
        let json = extraction_json("Knowledge", &valid_at, "A fact.");
        let output = parse_fact_extraction_response(&json)
            .unwrap_or_else(|error| panic!("valid_at {valid_at:.40} should not fail: {error}"));

        assert_eq!(
            output.candidates.len(),
            1,
            "the fact must survive an unusable valid_at"
        );
        assert!(output.candidates[0].valid_at.is_none());
        assert_eq!(output.event_time_unparsed, 1);
        assert_eq!(output.skipped.total(), 0);
    }
}

/// Content the model returns in the field it is supposed to — `content` — is of course kept: it is
/// the memory. This test exists so the one above cannot be satisfied by accidentally dropping
/// everything.
#[test]
fn accepted_facts_still_carry_their_content() {
    let json = extraction_json("Knowledge", "null", PRIVATE_SENTENCE);

    let output = parse_fact_extraction_response(&json).expect("response should parse");

    assert_eq!(output.candidates.len(), 1);
    assert_eq!(output.candidates[0].content, PRIVATE_SENTENCE);
}
