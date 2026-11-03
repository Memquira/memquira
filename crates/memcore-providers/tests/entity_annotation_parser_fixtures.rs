//! Every invented annotation output in
//! `fixtures/entity_annotation_parser_fixtures.json` goes through the real parser and must end as
//! the fixture expects: parsed strictly, repaired with exactly the listed repairs, or
//! unusable with the listed error class.

use memcore_core::ports::unusable_annotation_class;
use memcore_providers::prompts::parse_entity_annotation_response;
use serde_json::Value;

const FIXTURES: &str = include_str!("fixtures/entity_annotation_parser_fixtures.json");

#[test]
fn every_parser_fixture_ends_as_expected() {
    let fixtures: Value = serde_json::from_str(FIXTURES).expect("fixture file is valid JSON");
    let cases = fixtures["cases"].as_array().expect("cases");
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let text = case["text"].as_str().unwrap();
        let result = parse_entity_annotation_response(text);
        match case["expect"].as_str().unwrap() {
            expect @ ("parsed" | "repaired") => {
                let output = result.unwrap_or_else(|error| panic!("{id}: {error}"));
                let repairs: Vec<&str> = output.repairs.iter().map(|r| r.as_str()).collect();
                let expected: Vec<&str> = case["repairs"]
                    .as_array()
                    .map(|list| list.iter().map(|r| r.as_str().unwrap()).collect())
                    .unwrap_or_default();
                assert_eq!(repairs, expected, "{id}: repairs");
                assert_eq!(repairs.is_empty(), expect == "parsed", "{id}");
                let names: Vec<Vec<String>> = output
                    .facts
                    .iter()
                    .map(|fact| fact.entities.iter().map(|m| m.name.clone()).collect())
                    .collect();
                let expected: Vec<Vec<String>> =
                    serde_json::from_value(case["names"].clone()).expect("names");
                assert_eq!(names, expected, "{id}: names");
            }
            "unusable" => {
                let error = result.err().unwrap_or_else(|| panic!("{id}: parsed"));
                assert_eq!(
                    unusable_annotation_class(&error),
                    case["error_class"].as_str(),
                    "{id}: {error}"
                );
            }
            other => panic!("{id}: unknown expectation {other}"),
        }
    }
}

#[test]
fn every_measured_class_has_a_fixture() {
    let fixtures: Value = serde_json::from_str(FIXTURES).unwrap();
    let cases = fixtures["cases"].as_array().unwrap();
    for class in fixtures["measured_classes"].as_array().unwrap() {
        assert!(
            cases.iter().any(|case| case["class"] == *class),
            "no fixture for {class}"
        );
    }
}
