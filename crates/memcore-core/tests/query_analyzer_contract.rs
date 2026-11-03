//! The frozen fixture `phase4_analyzer_contract_v1.json`.
//!
//! The fixture is the specification. Its hash is pinned, so it cannot be edited to match the
//! implementation; a genuine specification defect needs maintainer review and a versioned fixture (v2).

use chrono::DateTime;
use memcore_core::{analyze, stable_sha256_hex};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/phase4_analyzer_contract_v1.json");
/// SHA-256 of the fixture with LF line endings (contract `phase4-analyzer-contract-v1`; the cases are
/// frozen, the descriptive metadata fields were reworded for publication).
const FROZEN_SHA256: &str = "abe6dc893dddfd3c739d9400c5b0f6068ac0242e598ff0d5b93f7196cc477352";

#[test]
fn fixture_is_the_frozen_v1_contract() {
    let normalised = FIXTURE.replace("\r\n", "\n");
    assert_eq!(stable_sha256_hex(&normalised), FROZEN_SHA256);
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture["contract"], "phase4-analyzer-contract-v1");
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 85);
}

#[test]
fn analyzer_meets_every_contract_case() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let question = case["question"].as_str().unwrap();
        let reference = case["reference_time"]
            .as_str()
            .map(|text| DateTime::parse_from_rfc3339(text).expect("fixture reference time"));
        let analysis = analyze(question, reference);
        let actual = json!({
            "question_type": analysis.question_type,
            "temporal_constraint": analysis.temporal_constraint,
            "relative_expression": analysis.relative_expression,
            "temporal_ordinal": analysis.temporal_ordinal,
            "sub_queries": analysis.sub_queries,
        });
        if actual != case["expected"] {
            failures.push(format!(
                "#{} {question}\n  expected {}\n  actual   {}",
                case["id"], case["expected"], actual
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 85 cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
