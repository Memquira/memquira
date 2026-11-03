//! Consolidation v2 decision prompt, shared by every provider.
//!
//! The rules restate the frozen consolidation design. The example names are invented;
//! nothing here may come from a third-party dataset.

use std::str::FromStr;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{ConsolidationDecision, ConsolidationInput, ConsolidationOperation};
use memcore_core::{CandidateFact, Fact, FactStructure};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// System instructions for a consolidation decision.
pub const CONSOLIDATION_INSTRUCTIONS: &str = r#"You maintain a person's long-term memory. Decide how ONE new candidate fact relates to the stored facts listed as existing_facts.
Choose exactly one operation:
- ADD: the candidate is new information. This includes a fact about a different person or thing, another item of a list (another hobby, pet, language or place), the same kind of event happening again at a different time, and anything unrelated.
- UPDATE: the candidate refines, enriches or corrects ONE existing fact about the same subject and the same thing (added detail, or a correction of something stated wrongly). Put the complete new fact in merged_content, keeping every detail of the old fact that is still true.
- INVALIDATE: the candidate says that ONE existing fact about the same subject has stopped being true because something changed over time (a changed preference, a new job or home, a habit or membership that ended). The old fact is kept as history and the candidate is stored as the new fact. Give invalid_at as the date the change happened (YYYY-MM-DD) when the facts say or imply it, else null.
- NOOP: an existing fact already states the same information and the candidate adds nothing new.
Rules:
- target_fact_id is the id of exactly one existing fact for UPDATE, INVALIDATE and NOOP, and null for ADD.
- Never UPDATE or INVALIDATE a fact about a different person, animal or thing than the candidate's subject.
- A correction of a mistake is UPDATE; a real change over time is INVALIDATE.
- Two facts that can both be true (two hobbies, two trips in different years) are never UPDATE or NOOP.
- Example: stored "Orla lives in Galway.", candidate "Orla moved to Cork last spring." -> INVALIDATE the Galway fact.
- Example: stored "Orla keeps bees.", candidate "Orla keeps bees in three hives behind her house." -> UPDATE with the merged sentence.
Return valid JSON only matching the schema."#;

/// JSON schema of the decision.
pub fn consolidation_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": { "type": "string", "enum": ["ADD", "UPDATE", "INVALIDATE", "NOOP"] },
            "target_fact_id": { "type": ["string", "null"] },
            "merged_content": { "type": ["string", "null"] },
            "invalid_at": { "type": ["string", "null"] },
            "reason": { "type": "string" }
        },
        "required": ["operation", "target_fact_id", "merged_content", "invalid_at", "reason"],
        "additionalProperties": false
    })
}

fn date(value: Option<DateTime<Utc>>) -> Value {
    value.map_or(Value::Null, |time| {
        json!(time.format("%Y-%m-%d").to_string())
    })
}

fn structure_json(structure: &FactStructure) -> (Value, Value, Value) {
    let event_time = structure.event_time.as_ref().map_or(Value::Null, |time| {
        json!({
            "start": time.start.format("%Y-%m-%d").to_string(),
            "end": time.end.format("%Y-%m-%d").to_string(),
        })
    });
    (
        structure.subject.clone().map_or(Value::Null, Value::String),
        event_time,
        date(structure.observed_at),
    )
}

fn candidate_json(candidate: &CandidateFact) -> Value {
    let (subject, event_time, stated_at) = structure_json(&candidate.structure);
    json!({
        "content": candidate.content,
        "memory_type": candidate.memory_type,
        "subject": subject,
        "event_time": event_time,
        "stated_at": stated_at,
    })
}

fn existing_json(fact: &Fact) -> Value {
    let (subject, event_time, stated_at) = structure_json(&fact.structure);
    json!({
        "id": fact.id,
        "content": fact.content,
        "memory_type": fact.memory_type,
        "subject": subject,
        "event_time": event_time,
        "stated_at": stated_at,
    })
}

/// The user payload of a decision request. The top-level keys match the legacy classification
/// payload (`candidate_fact`, `existing_facts`).
pub fn consolidation_payload(input: &ConsolidationInput) -> Value {
    json!({
        "tenant": { "org_id": input.tenant.org_id, "user_id": input.tenant.user_id },
        "candidate_fact": candidate_json(&input.candidate_fact),
        "existing_facts": input.existing_facts.iter().map(existing_json).collect::<Vec<_>>(),
    })
}

#[derive(Debug, Deserialize)]
struct DecisionResponse {
    operation: String,
    #[serde(default)]
    target_fact_id: Option<String>,
    #[serde(default)]
    merged_content: Option<String>,
    #[serde(default)]
    invalid_at: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

fn parse_invalid_at(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Some(time.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d").ok()?;
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?).into()
}

/// Parses a decision. Unknown operations and malformed JSON are provider errors. A target that is
/// not a UUID is dropped (memcore's validation then decides). An unreadable `invalid_at` is kept
/// only as text in `invalid_at_unparsed`, so memcore rejects and records it.
pub fn parse_consolidation_response(text: &str) -> MemcoreResult<ConsolidationDecision> {
    let parsed: DecisionResponse = serde_json::from_str(text.trim()).map_err(|error| {
        MemcoreError::ProviderError(format!(
            "invalid consolidation decision JSON from model: {error}"
        ))
    })?;
    let operation = match parsed
        .operation
        .trim()
        .to_ascii_uppercase()
        .replace(['-', ' '], "_")
        .as_str()
    {
        "ADD" => ConsolidationOperation::Add,
        "UPDATE" => ConsolidationOperation::Update,
        "INVALIDATE" => ConsolidationOperation::Invalidate,
        "NOOP" | "NO_OP" | "NONE" => ConsolidationOperation::NoOp,
        other => {
            return Err(MemcoreError::ProviderError(format!(
                "unknown consolidation operation from model: {:.40}",
                other
            )));
        }
    };
    let raw_invalid_at = parsed
        .invalid_at
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && !raw.eq_ignore_ascii_case("null"));
    let invalid_at = raw_invalid_at.and_then(parse_invalid_at);
    let invalid_at_unparsed = raw_invalid_at
        .filter(|_| invalid_at.is_none())
        .map(str::to_string);
    Ok(ConsolidationDecision {
        operation,
        target_fact_id: parsed
            .target_fact_id
            .as_deref()
            .and_then(|id| Uuid::from_str(id.trim()).ok()),
        merged_content: parsed
            .merged_content
            .map(|content| content.trim().to_string())
            .filter(|content| !content.is_empty()),
        invalid_at,
        invalid_at_unparsed,
        reason: parsed.reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_operation_and_normalises_labels() {
        let id = Uuid::new_v4();
        for (label, expected) in [
            ("ADD", ConsolidationOperation::Add),
            ("update", ConsolidationOperation::Update),
            ("Invalidate", ConsolidationOperation::Invalidate),
            ("NOOP", ConsolidationOperation::NoOp),
            ("no-op", ConsolidationOperation::NoOp),
        ] {
            let text = format!(
                r#"{{"operation":"{label}","target_fact_id":"{id}","merged_content":null,"invalid_at":null,"reason":"r"}}"#
            );
            let decision = parse_consolidation_response(&text).unwrap();
            assert_eq!(decision.operation, expected);
            assert_eq!(decision.target_fact_id, Some(id));
        }
    }

    #[test]
    fn reads_dates_and_drops_unreadable_values() {
        let text = r#"{"operation":"INVALIDATE","target_fact_id":"not-a-uuid","merged_content":"  ","invalid_at":"2024-03-01","reason":"moved"}"#;
        let decision = parse_consolidation_response(text).unwrap();
        assert_eq!(decision.target_fact_id, None);
        assert_eq!(decision.merged_content, None);
        assert_eq!(
            decision.invalid_at.unwrap().format("%Y-%m-%d").to_string(),
            "2024-03-01"
        );
        let text = r#"{"operation":"INVALIDATE","target_fact_id":null,"merged_content":null,"invalid_at":"sometime","reason":"r"}"#;
        let decision = parse_consolidation_response(text).unwrap();
        assert_eq!(decision.invalid_at, None);
        assert_eq!(decision.invalid_at_unparsed.as_deref(), Some("sometime"));
        // An impossible date is unreadable too; a readable one leaves nothing unparsed.
        let text = r#"{"operation":"INVALIDATE","target_fact_id":null,"merged_content":null,"invalid_at":"2024-02-30","reason":"r"}"#;
        let decision = parse_consolidation_response(text).unwrap();
        assert_eq!(decision.invalid_at, None);
        assert_eq!(decision.invalid_at_unparsed.as_deref(), Some("2024-02-30"));
        let text = r#"{"operation":"INVALIDATE","target_fact_id":null,"merged_content":null,"invalid_at":null,"reason":"r"}"#;
        assert_eq!(
            parse_consolidation_response(text)
                .unwrap()
                .invalid_at_unparsed,
            None
        );
    }

    #[test]
    fn rejects_unknown_operations_and_bad_json() {
        assert!(parse_consolidation_response(r#"{"operation":"DELETE","reason":"r"}"#).is_err());
        assert!(parse_consolidation_response("not json").is_err());
    }

    #[test]
    fn schema_requires_every_field_and_lists_four_operations() {
        let schema = consolidation_json_schema();
        assert_eq!(schema["required"].as_array().unwrap().len(), 5);
        assert_eq!(
            schema["properties"]["operation"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
}
