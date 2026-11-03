//! Regression tests for two early extraction bugs, on responses in the shape models return
//!(invented text).
//!
//! Bug 1 — vocabulary mismatch. `FACT_EXTRACTION_INSTRUCTIONS` asked the model for
//! `Preference | Profile | Goal | Relationship | Event | Knowledge | Instruction | Other` while
//! `parse_memory_type_label` accepted only
//! `profile | preference | project | conversation | task | entity | skill | system`. Six of the
//! eight labels the prompt requested were rejected, and the rejection was a bare
//! `Err(_) => continue`: no log, no metric, no error. Most extracted facts were
//! lost.
//!
//! Bug 2 — one bad date discarded the whole batch. `valid_at` deserialized as a strict
//! `DateTime<Utc>`, so a single fact carrying a date in any other shape failed `serde` for the
//! entire response and every sibling fact was lost with it. This affected a noticeable share
//! of batches, more with conversational timestamp formats.

use memcore_core::{CandidateSkipReason, MemoryType};
use memcore_providers::parse_fact_extraction_response;

/// A response of the shape that used to be lost. `Knowledge` is a label the old parser threw
/// away, so this whole response used to yield nothing.
const CAPTURED_KNOWLEDGE_RESPONSE: &str = r#"{"facts":[{"content":"Priya finds motivation in seeing her students succeed and appreciates Tomas's support.","confidence":0.95,"importance":0.9,"memory_type":"Knowledge","metadata":{"conversation_index":1,"session_id":"session_7"},"valid_at":null}]}"#;

#[test]
fn bug_1_captured_response_with_a_knowledge_label_is_no_longer_dropped() {
    let output = parse_fact_extraction_response(CAPTURED_KNOWLEDGE_RESPONSE)
        .expect("a captured production response should parse");

    assert_eq!(
        output.candidates.len(),
        1,
        "the Knowledge fact should survive extraction"
    );
    assert_eq!(output.candidates[0].memory_type, MemoryType::Knowledge);
    assert_eq!(output.skipped.total(), 0);
}

#[test]
fn bug_1_every_label_the_prompt_offers_survives_extraction() {
    // One fact per label the extraction prompt asks the model for. Before the fix only the first
    // two of these came back.
    let facts: Vec<String> = [
        "Preference",
        "Profile",
        "Goal",
        "Relationship",
        "Event",
        "Knowledge",
        "Instruction",
        "Other",
    ]
    .iter()
    .map(|label| {
        format!(
            r#"{{"content":"A fact labelled {label}.","memory_type":"{label}","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{{}}}}"#
        )
    })
    .collect();
    let json = format!(r#"{{"facts":[{}]}}"#, facts.join(","));

    let output = parse_fact_extraction_response(&json).expect("response should parse");

    assert_eq!(
        output.candidates.len(),
        8,
        "every label the prompt offers must produce a candidate"
    );
    assert_eq!(output.skipped.total(), 0);
}

#[test]
fn bug_1_an_unknown_label_is_counted_and_reported_rather_than_vanishing() {
    let json = r#"{"facts":[
        {"content":"Kept.","memory_type":"Knowledge","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}},
        {"content":"Lost.","memory_type":"Sandwich","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}}
    ]}"#;

    let output = parse_fact_extraction_response(json).expect("response should parse");

    assert_eq!(output.candidates.len(), 1);
    assert_eq!(
        output.skipped.get(CandidateSkipReason::UnknownMemoryType),
        1,
        "an unrecognised label must be counted, not silently skipped"
    );
    // The caller can always reconcile what the model produced against what memcore kept.
    assert_eq!(output.returned_by_model(), 2);
}

#[test]
fn bug_2_a_conversational_date_no_longer_destroys_its_whole_batch() {
    // The layout that failed 8 of 18 batches in the controlled test, alongside two
    // ordinary facts. Under the old strict `DateTime<Utc>` field, all three were lost.
    let json = r#"{"facts":[
        {"content":"First fact.","memory_type":"Knowledge","confidence":0.9,"importance":0.8,"valid_at":"2023-05-07T13:56:00Z","metadata":{}},
        {"content":"Second fact.","memory_type":"Event","confidence":0.9,"importance":0.8,"valid_at":"4:15 pm on 12 March, 2024","metadata":{}},
        {"content":"Third fact.","memory_type":"Preference","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}}
    ]}"#;

    let output = parse_fact_extraction_response(json).expect("response should parse");

    assert_eq!(
        output.candidates.len(),
        3,
        "no fact may be lost because a sibling carried an unusual date"
    );
    assert!(output.candidates[0].valid_at.is_some());
    assert!(
        output.candidates[1].valid_at.is_some(),
        "the conversational date should be understood, not merely tolerated"
    );
    assert!(output.candidates[2].valid_at.is_none());
    assert_eq!(output.event_time_unparsed, 0);
    assert_eq!(output.skipped.total(), 0);
}

#[test]
fn bug_2_an_unreadable_date_costs_one_fact_its_time_and_nothing_else() {
    let json = r#"{"facts":[
        {"content":"Kept with its date.","memory_type":"Event","confidence":0.9,"importance":0.8,"valid_at":"2023-05-07","metadata":{}},
        {"content":"Kept without a date.","memory_type":"Event","confidence":0.9,"importance":0.8,"valid_at":"last Saturday","metadata":{}}
    ]}"#;

    let output = parse_fact_extraction_response(json).expect("response should parse");

    assert_eq!(output.candidates.len(), 2, "both facts must survive");
    assert!(output.candidates[0].valid_at.is_some());
    assert!(
        output.candidates[1].valid_at.is_none(),
        "an unreadable date becomes None rather than an error"
    );
    assert_eq!(
        output.event_time_unparsed, 1,
        "the dropped date must be counted so the accepted formats can be kept up to date"
    );
    assert_eq!(
        output.skipped.total(),
        0,
        "an unreadable date is not a reason to skip a fact"
    );
}

#[test]
fn valid_at_shapes_a_model_actually_emits_are_all_handled() {
    for (raw, expect_time) in [
        ("null", false),
        (r#""""#, false),
        (r#""2023-05-07T13:56:00Z""#, true),
        (r#""2023-05-07T19:26:00+05:30""#, true),
        (r#""2023-05-07 13:56:00""#, true),
        (r#""2023-05-07""#, true),
        (r#""7 May 2023""#, true),
        (r#""May 7, 2023""#, true),
        (r#""4:15 pm on 12 March, 2024""#, true),
        (r#""sometime last spring""#, false),
        (r#"{"day":7}"#, false),
    ] {
        let json = format!(
            r#"{{"facts":[{{"content":"A fact.","memory_type":"Event","confidence":0.9,"importance":0.8,"valid_at":{raw},"metadata":{{}}}}]}}"#
        );
        let output = parse_fact_extraction_response(&json)
            .unwrap_or_else(|error| panic!("valid_at {raw} should not fail the response: {error}"));
        assert_eq!(output.candidates.len(), 1, "valid_at {raw} lost its fact");
        assert_eq!(
            output.candidates[0].valid_at.is_some(),
            expect_time,
            "valid_at {raw}"
        );
    }
}

/// The guarantee behind acceptance criterion P1-12b: for valid model output, nothing is skipped.
#[test]
fn valid_model_output_produces_no_skips_at_all() {
    let json = r#"{"facts":[
        {"content":"A preference.","memory_type":"Preference","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}},
        {"content":"A goal.","memory_type":"Goal","confidence":0.9,"importance":0.8,"valid_at":"2023-05-07","metadata":{}},
        {"content":"An instruction.","memory_type":"Instruction","confidence":1.0,"importance":1.0,"valid_at":null,"metadata":{}}
    ]}"#;

    let output = parse_fact_extraction_response(json).expect("response should parse");

    assert_eq!(output.candidates.len(), 3);
    assert_eq!(output.skipped.total(), 0);
    assert_eq!(output.event_time_unparsed, 0);
    for reason in CandidateSkipReason::ALL {
        assert_eq!(output.skipped.get(reason), 0, "{}", reason.as_str());
    }
}

/// Every fact the model returned ends in exactly one bucket. This is what makes the
/// `memcore_fact_candidates_skipped_total` metric trustworthy.
#[test]
fn candidates_and_skips_always_account_for_the_full_model_response() {
    let json = r#"{"facts":[
        {"content":"Good.","memory_type":"Knowledge","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}},
        {"content":"","memory_type":"Knowledge","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}},
        {"content":"Bad label.","memory_type":"Casserole","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}}
    ]}"#;

    let output = parse_fact_extraction_response(json).expect("response should parse");

    assert_eq!(output.candidates.len(), 1);
    assert_eq!(output.skipped.get(CandidateSkipReason::InvalidCandidate), 1);
    assert_eq!(
        output.skipped.get(CandidateSkipReason::UnknownMemoryType),
        1
    );
    assert_eq!(output.returned_by_model(), 3);
    assert_eq!(
        output.candidates.len() + output.skipped.total(),
        output.returned_by_model()
    );
}
