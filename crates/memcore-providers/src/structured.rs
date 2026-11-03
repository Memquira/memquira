//! The request a provider sends and the response it parses.
//!
//! Provider-neutral, so other providers can reuse it. The legacy path in
//! `openai/types.rs` is untouched: with `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` off, nothing here
//! runs.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Timelike, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::temporal::parse_lenient_timestamp;
use memcore_core::{
    CandidateFact, CandidateSkipReason, FactExtractionOutput, FactStructure, MemoryType,
    TemporalGranularity, TemporalValue,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::inputs::{MemoryMessage, MessageRole};
use crate::redaction::sanitize_for_log;

/// One input window, as sent to the model, with the bookkeeping needed to read the reply back.
#[derive(Debug, Clone)]
pub struct StructuredWindow {
    payload: Value,
    /// Prompt ID -> the client's message ID, when the client supplied one.
    client_ids: HashMap<String, Option<String>>,
    /// Prompt ID -> the message's time.
    times: HashMap<String, Option<DateTime<Utc>>>,
    /// The latest message time in the window, used as the anchor when a fact cites nothing.
    latest_time: Option<DateTime<Utc>>,
}

impl StructuredWindow {
    /// Builds the request payload. Only what extraction needs is sent — no tenant IDs, no request
    /// metadata. A message without a client ID gets a placeholder (`m1`, `m2`, …) so the model can
    /// cite it; placeholders are never stored as provenance.
    pub fn new(messages: &[MemoryMessage]) -> Self {
        let mut client_ids = HashMap::new();
        let mut times = HashMap::new();
        let mut used: HashSet<String> = messages
            .iter()
            .filter_map(|message| message.message_id.clone())
            .collect();
        let mut items = Vec::with_capacity(messages.len());
        for (index, message) in messages.iter().enumerate() {
            let prompt_id = match &message.message_id {
                Some(id) => id.clone(),
                None => {
                    let mut candidate = format!("m{}", index + 1);
                    while used.contains(&candidate) {
                        candidate.push('_');
                    }
                    used.insert(candidate.clone());
                    candidate
                }
            };
            client_ids.insert(prompt_id.clone(), message.message_id.clone());
            times.insert(prompt_id.clone(), message.timestamp);
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
            };
            items.push(json!({
                "id": prompt_id,
                "speaker": message.name.as_deref().unwrap_or(role),
                "role": role,
                "time": message.timestamp.map(|time| time.to_rfc3339()),
                "text": message.content,
            }));
        }
        let latest_time = messages
            .iter()
            .filter_map(|message| message.timestamp)
            .max();
        Self {
            payload: json!({ "messages": items }),
            client_ids,
            times,
            latest_time,
        }
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

#[derive(Debug, Deserialize)]
struct StructuredResponse {
    facts: Vec<StructuredFact>,
}

#[derive(Debug, Deserialize)]
struct StructuredFact {
    content: String,
    memory_type: String,
    #[serde(default)]
    subject: Option<String>,
    /// Untyped for the same reason as Part A's `valid_at`: one malformed time must cost only that
    /// fact its time, never the whole batch.
    #[serde(default)]
    event_time: Value,
    #[serde(default)]
    source_message_ids: Vec<Value>,
    confidence: f32,
    importance: f32,
}

/// Rebuilds the final run of closing brackets of a reply from the nesting the rest of the reply
/// opened. Models without constrained decoding sometimes end a complete reply with the closers in
/// the wrong order or one short (`…"importance":0.85}]]` for `…}]}`). Nothing but that trailing
/// run is changed, and only when it differs from the correct closers by at most one bracket, so a
/// reply cut off mid-way is still rejected. Returns `None` when the reply does not have that shape.
fn repair_trailing_closers(text: &str) -> Option<String> {
    let body = text.trim_end();
    let tail_start = body
        .char_indices()
        .rev()
        .find(|(_, c)| !matches!(c, '}' | ']') && !c.is_whitespace())
        .map(|(index, c)| index + c.len_utf8())?;
    let (prefix, tail) = body.split_at(tail_start);
    let closers = tail.chars().filter(|c| matches!(c, '}' | ']')).count();

    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in prefix.chars() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(c) {
                    return None;
                }
            }
            _ => {}
        }
    }
    if in_string || stack.is_empty() || closers.abs_diff(stack.len()) > 1 {
        return None;
    }
    let mut repaired = prefix.to_string();
    repaired.extend(stack.iter().rev());
    (repaired != body).then_some(repaired)
}

/// Reads the reply through `Value` first: a model that repeats a key within one fact (seen with
/// `event_time`) then gets the last value, as other JSON readers give it, instead of failing the
/// whole reply on serde's duplicate-field check.
fn read_structured_response(text: &str) -> serde_json::Result<StructuredResponse> {
    serde_json::from_str::<Value>(text).and_then(serde_json::from_value)
}

/// Parses a structured extraction reply against the window it was produced from.
pub fn parse_structured_fact_extraction_response(
    text: &str,
    window: &StructuredWindow,
) -> MemcoreResult<FactExtractionOutput> {
    let parsed: StructuredResponse = match read_structured_response(text) {
        Ok(parsed) => parsed,
        Err(error) => {
            let repaired = repair_trailing_closers(text)
                .and_then(|repaired| read_structured_response(&repaired).ok());
            match repaired {
                Some(parsed) => {
                    tracing::warn!(
                        event = "structured_extraction_closers_repaired",
                        "the model closed the reply's brackets in the wrong order; rebuilt them"
                    );
                    parsed
                }
                None => {
                    return Err(MemcoreError::ProviderError(format!(
                        "invalid structured fact extraction JSON from model: {error}"
                    )));
                }
            }
        }
    };

    let mut output = FactExtractionOutput::default();
    for fact in parsed.facts {
        let memory_type = match MemoryType::parse_label(&fact.memory_type) {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(
                    event = "fact_candidate_skipped",
                    reason = CandidateSkipReason::UnknownMemoryType.as_str(),
                    memory_type = %sanitize_for_log(&fact.memory_type),
                    "discarding an extracted fact whose memory type is not in the vocabulary"
                );
                output
                    .skipped
                    .record(CandidateSkipReason::UnknownMemoryType);
                continue;
            }
        };

        // Provenance: keep cited IDs that exist in this window, in citation order, once each.
        let mut cited_prompt_ids = Vec::new();
        for raw in &fact.source_message_ids {
            let Some(id) = raw.as_str().map(str::trim) else {
                output.invalid_source_message_ids += 1;
                continue;
            };
            if window.client_ids.contains_key(id) {
                if !cited_prompt_ids.iter().any(|seen: &String| seen == id) {
                    cited_prompt_ids.push(id.to_string());
                }
            } else {
                output.invalid_source_message_ids += 1;
            }
        }
        let source_message_ids: Vec<String> = cited_prompt_ids
            .iter()
            .filter_map(|id| window.client_ids.get(id).cloned().flatten())
            .collect();

        // The anchor is the time of the first message the fact cites, else the latest in the window.
        let anchor = cited_prompt_ids
            .iter()
            .find_map(|id| window.times.get(id).copied().flatten())
            .or(window.latest_time);

        let (event_time, event_time_text, unreadable) = parse_event_time(&fact.event_time, anchor);
        if unreadable {
            output.event_time_unparsed += 1;
        }

        let subject = fact
            .subject
            .map(|subject| subject.trim().to_string())
            .filter(|subject| !subject.is_empty());

        match CandidateFact::new(
            fact.content,
            memory_type,
            clamp_unit_interval(fact.confidence),
            clamp_unit_interval(fact.importance),
            None,
            json!({}),
        ) {
            Ok(candidate) => output
                .candidates
                .push(candidate.with_structure(FactStructure {
                    subject,
                    event_time,
                    event_time_text,
                    observed_at: anchor,
                    session_id: None,
                    source_message_ids,
                })),
            Err(error) => {
                tracing::warn!(
                    event = "fact_candidate_skipped",
                    reason = CandidateSkipReason::InvalidCandidate.as_str(),
                    error = %error,
                    "discarding an extracted fact that failed validation"
                );
                output.skipped.record(CandidateSkipReason::InvalidCandidate);
            }
        }
    }
    Ok(output)
}

/// Reads the model's `event_time`. Returns the value (if usable), the original expression (kept
/// even when the dates are unusable, so the deterministic resolver can still try it), and whether a
/// time was given but could not be read.
fn parse_event_time(
    value: &Value,
    anchor: Option<DateTime<Utc>>,
) -> (Option<TemporalValue>, Option<String>, bool) {
    let Some(object) = value.as_object() else {
        return (None, None, !value.is_null());
    };
    let text = object
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);

    let start = object.get("start").and_then(Value::as_str);
    let end = object.get("end").and_then(Value::as_str).or(start);
    let granularity = object
        .get("granularity")
        .and_then(Value::as_str)
        .and_then(|label| label.parse::<TemporalGranularity>().ok());

    let parsed = match (start, end, granularity) {
        (Some(start), Some(end), Some(granularity)) => {
            match (parse_lenient_timestamp(start), parse_lenient_timestamp(end)) {
                (Some(start), Some(end)) => {
                    let end = end_of_day_if_date_only(end);
                    TemporalValue::new(start, end, granularity, anchor.unwrap_or(start)).ok()
                }
                _ => None,
            }
        }
        _ => None,
    };
    let unreadable = parsed.is_none();
    (parsed, text, unreadable)
}

/// An `end` given as a bare date means the whole of that day.
fn end_of_day_if_date_only(value: DateTime<Utc>) -> DateTime<Utc> {
    if value.hour() == 0 && value.minute() == 0 && value.second() == 0 {
        value
            .with_hour(23)
            .and_then(|v| v.with_minute(59))
            .and_then(|v| v.with_second(59))
            .unwrap_or(value)
    } else {
        value
    }
}

fn clamp_unit_interval(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2023, 5, d, h, 0, 0).unwrap()
    }

    fn message(id: Option<&str>, name: &str, time: DateTime<Utc>, text: &str) -> MemoryMessage {
        MemoryMessage {
            role: MessageRole::User,
            content: text.to_string(),
            name: Some(name.to_string()),
            timestamp: Some(time),
            message_id: id.map(str::to_string),
        }
    }

    fn window() -> StructuredWindow {
        StructuredWindow::new(&[
            message(
                Some("D1:1"),
                "Priya",
                at(8, 10),
                "I adopted a dog yesterday.",
            ),
            message(Some("D1:2"), "Tomas", at(8, 11), "Congrats!"),
        ])
    }

    #[test]
    fn payload_carries_ids_speakers_and_times_but_no_tenant_or_metadata() {
        let payload = window().payload().clone();
        let first = &payload["messages"][0];
        assert_eq!(first["id"], "D1:1");
        assert_eq!(first["speaker"], "Priya");
        assert_eq!(first["time"], "2023-05-08T10:00:00+00:00");
        assert!(payload.get("tenant").is_none());
        assert!(payload.get("metadata").is_none());
    }

    #[test]
    fn messages_without_ids_get_placeholders_that_are_never_stored() {
        let window = StructuredWindow::new(&[message(None, "Priya", at(8, 10), "Hi")]);
        assert_eq!(window.payload()["messages"][0]["id"], "m1");
        let reply = r#"{"facts":[{"content":"Priya said hi.","memory_type":"Other","subject":"Priya","event_time":null,"source_message_ids":["m1"],"confidence":0.9,"importance":0.6}]}"#;
        let output = parse_structured_fact_extraction_response(reply, &window).unwrap();
        assert!(output.candidates[0].structure.source_message_ids.is_empty());
        assert_eq!(
            output.invalid_source_message_ids, 0,
            "a placeholder is a valid citation"
        );
        assert_eq!(output.candidates[0].structure.observed_at, Some(at(8, 10)));
    }

    #[test]
    fn a_full_structured_fact_parses_with_subject_time_and_provenance() {
        let reply = r#"{"facts":[{"content":"On 7 May 2023, Priya adopted a dog.","memory_type":"Event","subject":"Priya","event_time":{"start":"2023-05-07","end":"2023-05-07","granularity":"day","text":"yesterday"},"source_message_ids":["D1:1"],"confidence":0.95,"importance":0.8}]}"#;
        let output = parse_structured_fact_extraction_response(reply, &window()).unwrap();
        let fact = &output.candidates[0];
        assert_eq!(fact.structure.subject.as_deref(), Some("Priya"));
        assert_eq!(fact.structure.source_message_ids, vec!["D1:1"]);
        assert_eq!(fact.structure.event_time_text.as_deref(), Some("yesterday"));
        let time = fact.structure.event_time.as_ref().unwrap();
        assert_eq!(
            time.start,
            Utc.with_ymd_and_hms(2023, 5, 7, 0, 0, 0).unwrap()
        );
        assert_eq!(
            time.end,
            Utc.with_ymd_and_hms(2023, 5, 7, 23, 59, 59).unwrap()
        );
        assert_eq!(
            time.anchor,
            at(8, 10),
            "anchored to the cited message, not to now"
        );
        assert_eq!(
            fact.valid_at,
            Some(time.start),
            "valid_at follows the event start"
        );
    }

    #[test]
    fn source_ids_not_in_the_window_are_dropped_and_counted() {
        let reply = r#"{"facts":[{"content":"Priya adopted a dog.","memory_type":"Event","subject":"Priya","event_time":null,"source_message_ids":["D1:1","D9:9",7,"D1:1"],"confidence":0.9,"importance":0.8}]}"#;
        let output = parse_structured_fact_extraction_response(reply, &window()).unwrap();
        assert_eq!(
            output.candidates[0].structure.source_message_ids,
            vec!["D1:1"]
        );
        assert_eq!(output.invalid_source_message_ids, 2);
    }

    #[test]
    fn missing_optional_fields_are_tolerated() {
        let reply = r#"{"facts":[{"content":"Someone likes tea.","memory_type":"Preference","confidence":0.9,"importance":0.8}]}"#;
        let output = parse_structured_fact_extraction_response(reply, &window()).unwrap();
        let fact = &output.candidates[0];
        assert!(fact.structure.subject.is_none());
        assert!(fact.structure.event_time.is_none());
        assert!(fact.structure.source_message_ids.is_empty());
        assert_eq!(output.event_time_unparsed, 0);
        assert_eq!(
            fact.structure.observed_at,
            Some(at(8, 11)),
            "falls back to the latest message"
        );
    }

    #[test]
    fn an_unreadable_time_keeps_the_fact_and_its_expression() {
        let reply = r#"{"facts":[
            {"content":"Priya adopted a dog.","memory_type":"Event","subject":"Priya","event_time":{"start":"sometime","end":"later","granularity":"day","text":"yesterday"},"source_message_ids":["D1:1"],"confidence":0.9,"importance":0.8},
            {"content":"Priya likes dogs.","memory_type":"Preference","subject":"Priya","event_time":{"start":"2023-05-09","end":"2023-05-01","granularity":"day","text":null},"source_message_ids":["D1:1"],"confidence":0.9,"importance":0.8},
            {"content":"Tomas congratulated Priya.","memory_type":"Event","subject":"Tomas","event_time":null,"source_message_ids":["D1:2"],"confidence":0.9,"importance":0.8}
        ]}"#;
        let output = parse_structured_fact_extraction_response(reply, &window()).unwrap();
        assert_eq!(
            output.candidates.len(),
            3,
            "no sibling may be lost to a bad date"
        );
        assert!(output.candidates[0].structure.event_time.is_none());
        assert_eq!(
            output.candidates[0].structure.event_time_text.as_deref(),
            Some("yesterday")
        );
        assert!(
            output.candidates[1].structure.event_time.is_none(),
            "end before start is rejected"
        );
        assert_eq!(output.event_time_unparsed, 2);
    }

    #[test]
    fn unknown_types_and_invalid_facts_are_counted_like_legacy() {
        let reply = r#"{"facts":[
            {"content":"x","memory_type":"Casserole","subject":null,"event_time":null,"source_message_ids":[],"confidence":0.9,"importance":0.8},
            {"content":"","memory_type":"Event","subject":null,"event_time":null,"source_message_ids":[],"confidence":0.9,"importance":0.8}
        ]}"#;
        let output = parse_structured_fact_extraction_response(reply, &window()).unwrap();
        assert!(output.candidates.is_empty());
        assert_eq!(
            output.skipped.get(CandidateSkipReason::UnknownMemoryType),
            1
        );
        assert_eq!(output.skipped.get(CandidateSkipReason::InvalidCandidate), 1);
    }

    #[test]
    fn malformed_json_is_a_provider_error() {
        let error =
            parse_structured_fact_extraction_response("{\"facts\":[", &window()).unwrap_err();
        assert!(matches!(error, MemcoreError::ProviderError(_)));
    }

    #[test]
    fn mis_ordered_trailing_closers_are_rebuilt() {
        for (reply, expected) in [
            (
                r#"{"facts":[{"a":1,"t":{"x":"y"}]}"#,
                r#"{"facts":[{"a":1,"t":{"x":"y"}}]}"#,
            ),
            (r#"{"facts":[{"a":0.85}]]"#, r#"{"facts":[{"a":0.85}]}"#),
            (
                r#"{"facts":[{"t":{"x":"y"}]}]"#,
                r#"{"facts":[{"t":{"x":"y"}}]}"#,
            ),
            (
                r#"{"facts":[{"s":"a ]} b"}]]  "#,
                r#"{"facts":[{"s":"a ]} b"}]}"#,
            ),
        ] {
            assert_eq!(
                repair_trailing_closers(reply).as_deref(),
                Some(expected),
                "{reply}"
            );
        }
    }

    #[test]
    fn other_malformed_replies_are_not_repaired() {
        for reply in [
            r#"{"facts":[{"a":1}]}"#,           // already valid: nothing to do
            r#"{"facts":[{"a":"cut off"#,       // truncated inside a string
            r#"{"facts":[{"a":1},{"b":2"#,      // truncated, closers missing entirely
            r#"{"facts":[{"a":1]},{"b":2}]}"#,  // mismatch before the tail
            r#"{"facts":[{"t":{"x":"y"}]]]]]"#, // tail too far from the nesting
        ] {
            assert_eq!(repair_trailing_closers(reply), None, "{reply}");
        }
    }

    #[test]
    fn a_repeated_key_takes_the_last_value() {
        let window =
            StructuredWindow::new(&[MemoryMessage::new(MessageRole::User, "I adopted a dog.")]);
        let reply = r#"{"facts":[{"content":"Priya adopted a dog.","event_time":{"start":"2023-05-01","end":"2023-05-01","granularity":"day","text":"last Monday"},"memory_type":"event","subject":"Priya","source_message_ids":["m1"],"confidence":0.9,"importance":0.8,"event_time":{"start":"2023-05-06","end":"2023-05-06","granularity":"day","text":"last Saturday"}}]]"#;
        let output = parse_structured_fact_extraction_response(reply, &window).unwrap();
        assert_eq!(output.candidates.len(), 1);
        assert_eq!(
            output.candidates[0].structure.event_time_text.as_deref(),
            Some("last Saturday")
        );
    }

    #[test]
    fn a_reply_with_mis_ordered_closers_still_parses() {
        let window =
            StructuredWindow::new(&[MemoryMessage::new(MessageRole::User, "I adopted a dog.")]);
        let reply = r#"{"facts":[{"content":"Priya adopted a dog.","memory_type":"event","subject":"Priya","event_time":null,"source_message_ids":["m1"],"confidence":0.9,"importance":0.8}]]"#;
        let output = parse_structured_fact_extraction_response(reply, &window).unwrap();
        assert_eq!(output.candidates.len(), 1);
    }
}
