use std::str::FromStr;

use crate::redaction::sanitize_for_log;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::temporal::parse_model_timestamp_value;
use memcore_core::{
    CandidateFact, CandidateSkipReason, FactExtractionOutput, FactOperation, FactOperationDecision,
    MemoryType,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Parses memory type labels from model JSON (PascalCase or snake_case).
///
/// Delegates to `MemoryType::parse_label` so this parser and the extraction prompt cannot describe
/// different vocabularies, which is what silently discarded most extracted facts in early versions.
pub fn parse_memory_type_label(value: &str) -> MemcoreResult<MemoryType> {
    MemoryType::parse_label(value)
}

/// Parses fact operation labels from model JSON (PascalCase or snake_case).
pub fn parse_fact_operation_label(value: &str) -> MemcoreResult<FactOperation> {
    let normalized = value.trim().replace(' ', "_");
    let snake = to_snake_case(&normalized);

    match snake.as_str() {
        "add" => Ok(FactOperation::Add),
        "update" => Ok(FactOperation::Update),
        "delete" => Ok(FactOperation::Delete),
        "no_op" | "noop" => Ok(FactOperation::NoOp),
        "archive" => Ok(FactOperation::Archive),
        "summarize" => Ok(FactOperation::Summarize),
        _ => Err(MemcoreError::ValidationError(format!(
            "invalid operation from model: {value}"
        ))),
    }
}

fn to_snake_case(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[derive(Debug, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
pub struct ApiErrorBody {
    pub message: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub r#type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub code: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResponsesCreateRequest {
    pub model: String,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextFormatConfig>,
}

#[derive(Debug, Serialize)]
pub struct TextFormatConfig {
    pub format: TextFormat,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextFormat {
    #[serde(rename = "json_schema")]
    JsonSchema {
        name: String,
        strict: bool,
        schema: Value,
    },
    Text,
}

#[derive(Debug, Deserialize)]
pub struct ResponsesCreateResponse {
    pub status: Option<String>,
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub error: Option<ResponseError>,
    /// Present when `status` is `incomplete`; `reason` is `max_output_tokens` on truncation.
    #[serde(default)]
    pub incomplete_details: Option<ResponseIncompleteDetails>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseIncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

impl ResponsesCreateResponse {
    /// Whether generation stopped at the output-token limit.
    pub fn is_truncated(&self) -> bool {
        self.status.as_deref() == Some("incomplete")
            && self
                .incomplete_details
                .as_ref()
                .and_then(|details| details.reason.as_deref())
                == Some("max_output_tokens")
    }
}

#[derive(Debug, Deserialize)]
pub struct ResponseError {
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseOutputItem {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub content: Vec<ResponseOutputContent>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseOutputContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<ChatCompletionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling settings; only query decomposition sends them, so every other request is
    /// unchanged on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Z.ai GLM thinking control (`{"type":"disabled"}` / `{"type":"enabled"}`).
    /// Omitted for non-Z.ai OpenAI-compatible hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsResponse {
    #[serde(default)]
    pub choices: Vec<ChatCompletionChoice>,
}

impl ChatCompletionsResponse {
    /// Whether generation stopped at the output-token limit (`finish_reason: "length"`).
    pub fn is_truncated(&self) -> bool {
        self.choices
            .first()
            .and_then(|choice| choice.finish_reason.as_deref())
            == Some("length")
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionChoice {
    pub message: ChatCompletionResponseMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
}

pub fn extract_chat_completion_text(response: &ChatCompletionsResponse) -> MemcoreResult<String> {
    let content = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_ref())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    content.ok_or_else(|| {
        MemcoreError::ProviderError("OpenAI chat completion returned empty content".to_string())
    })
}

#[derive(Debug, Serialize)]
pub struct EmbeddingsCreateRequest {
    pub model: String,
    pub input: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingsCreateResponse {
    pub data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingData {
    pub embedding: Vec<f32>,
    /// Some OpenAI-compatible hosts (e.g. Gemini) omit `index` for single inputs.
    #[serde(default)]
    pub index: usize,
}

#[derive(Debug, Deserialize)]
pub struct FactExtractionModelResponse {
    pub facts: Vec<FactExtractionModelFact>,
}

#[derive(Debug, Deserialize)]
pub struct FactExtractionModelFact {
    pub content: String,
    pub memory_type: String,
    pub confidence: f32,
    pub importance: f32,
    /// Deliberately untyped.
    ///
    /// This was `Option<DateTime<Utc>>`, which made `valid_at` a strict RFC 3339 field: a model
    /// writing a date any other way failed `serde` for the whole response and every fact in that
    /// batch was lost. Holding the raw value means a date memcore cannot read costs that one fact
    /// its event time and nothing else.
    #[serde(default)]
    pub valid_at: Value,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Deserialize)]
pub struct ClassificationModelResponse {
    pub operation: String,
    #[serde(default)]
    pub target_fact_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    pub confidence: f32,
}

pub fn classification_json_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "operation": { "type": "string" },
            "target_fact_id": { "type": ["string", "null"] },
            "reason": { "type": ["string", "null"] },
            "confidence": { "type": "number" }
        },
        "required": ["operation", "target_fact_id", "reason", "confidence"],
        "additionalProperties": false
    })
}

/// Turns a model's extraction response into candidates, accounting for everything it could not use.
///
/// Both loops used to end in `Err(_) => continue`. A fact with a memory type the parser did not
/// recognise, or one that failed validation, simply vanished — no log, no metric, no error — which
/// is how memcore came to store 634 memories from 5,882 conversation turns. Every loss is now
/// counted by reason and logged at `WARN`, and the counts travel back to the caller in
/// `FactExtractionOutput` so they can reach the metrics endpoint.
pub fn parse_fact_extraction_response(text: &str) -> MemcoreResult<FactExtractionOutput> {
    let parsed: FactExtractionModelResponse = serde_json::from_str(text).map_err(|err| {
        MemcoreError::ProviderError(format!("invalid fact extraction JSON from model: {err}"))
    })?;

    let mut output = FactExtractionOutput::default();
    for fact in parsed.facts {
        let memory_type = match parse_memory_type_label(&fact.memory_type) {
            Ok(value) => value,
            // The parse error is deliberately discarded: it only restates the same raw label,
            // which is untrusted model output and is logged below bounded and stripped of
            // control characters instead.
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

        let valid_at = parse_model_timestamp_value(&fact.valid_at);
        if valid_at.is_unparsable() {
            output.event_time_unparsed += 1;
            // Knowing which date formats to add needs an excerpt of what arrived, but the field
            // is untrusted: only a bounded, sanitized string form is logged, and a non-string
            // value is reported by JSON type alone rather than by content.
            tracing::warn!(
                event = "fact_event_time_unparsed",
                value_type = json_value_kind(&fact.valid_at),
                raw = %loggable_valid_at(&fact.valid_at),
                "keeping an extracted fact whose valid_at could not be read"
            );
        }

        match CandidateFact::new(
            fact.content,
            memory_type,
            clamp_unit_interval(fact.confidence),
            clamp_unit_interval(fact.importance),
            valid_at.parsed(),
            fact.metadata,
        ) {
            Ok(candidate) => output.candidates.push(candidate),
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

/// The JSON type of a `valid_at` value, as a fixed word. Safe to log and safe as a metric label.
fn json_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A bounded, log-safe rendering of an unreadable `valid_at`.
///
/// Only a string is shown, because that is the shape a date is supposed to arrive in and the one
/// whose format is worth learning from. An array or object in this field means the model went off
/// the schema entirely and could be carrying anything, so its contents are withheld.
fn loggable_valid_at(value: &Value) -> String {
    match value {
        Value::String(text) => sanitize_for_log(text),
        Value::Number(number) => sanitize_for_log(&number.to_string()),
        Value::Bool(flag) => flag.to_string(),
        _ => "<withheld: not a string>".to_string(),
    }
}

fn clamp_unit_interval(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

/// Every defect of the model's classification output is a `ProviderOutputInvalid`: the
/// engine rejects that one candidate fact instead of failing the whole write. A warning names the
/// defect; no model text is logged.
pub fn parse_classification_response(text: &str) -> MemcoreResult<FactOperationDecision> {
    const OP: &str = "classify_fact_operation";
    let invalid = |reason: &'static str| {
        tracing::warn!(
            event = "classification_output_invalid",
            reason,
            "model classification output rejected"
        );
        MemcoreError::provider_output_invalid(OP, reason)
    };
    let parsed: ClassificationModelResponse =
        serde_json::from_str(text).map_err(|_| invalid("invalid_json"))?;

    let operation =
        parse_fact_operation_label(&parsed.operation).map_err(|_| invalid("invalid_operation"))?;
    let target_fact_id = match parsed.target_fact_id {
        Some(id) if !id.trim().is_empty() => match Uuid::from_str(id.trim()) {
            Ok(id) => Some(id),
            // An Add never targets an existing fact, so a target that is not a UUID
            // (small local models invent one, e.g. a slug of the content) is dropped instead of
            // failing the whole ingest. Every other operation still requires a valid UUID.
            Err(_) if operation == FactOperation::Add => {
                tracing::warn!(
                    event = "classification_add_target_ignored",
                    "model returned a non-UUID target_fact_id for an Add decision; ignored"
                );
                None
            }
            Err(_) => return Err(invalid("invalid_target_fact_id")),
        },
        _ => None,
    };

    if !(0.0..=1.0).contains(&parsed.confidence) {
        return Err(invalid("confidence_out_of_range"));
    }

    Ok(FactOperationDecision {
        operation,
        target_fact_id,
        reason: parsed.reason,
        confidence: parsed.confidence,
    })
}

pub fn extract_output_text(response: &ResponsesCreateResponse) -> MemcoreResult<String> {
    if let Some(error) = &response.error {
        let message = error
            .message
            .clone()
            .unwrap_or_else(|| "OpenAI response error".to_string());
        return Err(MemcoreError::ProviderError(message));
    }

    let mut parts = Vec::new();
    for item in &response.output {
        if item.item_type != "message" {
            continue;
        }
        for content in &item.content {
            if content.content_type == "output_text" && !content.text.is_empty() {
                parts.push(content.text.as_str());
            }
        }
    }

    if parts.is_empty() {
        return Err(MemcoreError::ProviderError(
            "OpenAI response contained no output text".to_string(),
        ));
    }

    Ok(parts.join(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-UUID target on an Add is dropped (Add never uses it); on any other
    /// operation it is still an error; a valid UUID is kept exactly as before.
    /// A model can return `"operation": "None"` (rarely seen in practice); every
    /// contract defect is a `ProviderOutputInvalid` with a fixed reason, never a client 400.
    #[test]
    fn invalid_classification_outputs_are_provider_output_invalid() {
        let cases = [
            (
                r#"{"operation":"None","target_fact_id":null,"reason":null,"confidence":0.9}"#,
                "invalid_operation",
            ),
            (
                r#"{"operation":"Delete","target_fact_id":"fact-7","reason":null,"confidence":0.9}"#,
                "invalid_target_fact_id",
            ),
            (
                r#"{"operation":"Add","target_fact_id":null,"reason":null,"confidence":1.5}"#,
                "confidence_out_of_range",
            ),
            (r#"{"operation": "Add", "#, "invalid_json"),
        ];
        for (text, want) in cases {
            match parse_classification_response(text) {
                Err(MemcoreError::ProviderOutputInvalid { operation, reason }) => {
                    assert_eq!(operation, "classify_fact_operation");
                    assert_eq!(reason, want, "{text}");
                }
                other => panic!("{text}: expected ProviderOutputInvalid({want}), got {other:?}"),
            }
        }
    }

    #[test]
    fn classification_ignores_an_invalid_target_only_for_add() {
        let add = parse_classification_response(
            r#"{"operation":"Add","target_fact_id":"new_profile_fact_Id_123","reason":null,"confidence":0.9}"#,
        )
        .expect("add with an invented target");
        assert_eq!(add.operation, FactOperation::Add);
        assert_eq!(add.target_fact_id, None);

        let update = parse_classification_response(
            r#"{"operation":"Update","target_fact_id":"new_profile_fact_Id_123","reason":null,"confidence":0.9}"#,
        );
        assert!(
            update
                .as_ref()
                .is_err_and(MemcoreError::is_provider_output_invalid),
            "update still needs a real target: {update:?}"
        );

        let id = Uuid::new_v4();
        let kept = parse_classification_response(&format!(
            r#"{{"operation":"Add","target_fact_id":"{id}","reason":null,"confidence":0.9}}"#
        ))
        .unwrap();
        assert_eq!(kept.target_fact_id, Some(id));
    }
    use memcore_core::MemoryType;

    #[test]
    fn parses_fact_extraction_json() {
        let json = r#"{
            "facts": [
                {
                    "content": "User is learning Rust.",
                    "memory_type": "Skill",
                    "confidence": 0.95,
                    "importance": 0.82,
                    "valid_at": null,
                    "metadata": {}
                }
            ]
        }"#;

        let output = parse_fact_extraction_response(json).expect("parse should succeed");
        let facts = &output.candidates;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "User is learning Rust.");
        assert_eq!(facts[0].memory_type, MemoryType::Skill);
    }

    #[test]
    fn clamps_out_of_range_confidence_and_importance() {
        let json = r#"{
            "facts": [
                {
                    "content": "User likes green tea.",
                    "memory_type": "Preference",
                    "confidence": 0.9,
                    "importance": 5,
                    "valid_at": null,
                    "metadata": {}
                }
            ]
        }"#;

        let output = parse_fact_extraction_response(json).expect("parse should succeed");
        let facts = &output.candidates;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].importance, 1.0);
        assert!((facts[0].confidence - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn invalid_fact_extraction_json_returns_provider_error() {
        let error = parse_fact_extraction_response("not json").expect_err("should fail");
        assert!(matches!(error, MemcoreError::ProviderError(_)));
    }

    #[test]
    fn skips_invalid_candidate_facts() {
        let json = r#"{
            "facts": [
                {
                    "content": "",
                    "memory_type": "Skill",
                    "confidence": 0.95,
                    "importance": 0.82,
                    "valid_at": null,
                    "metadata": {}
                },
                {
                    "content": "Valid fact",
                    "memory_type": "conversation",
                    "confidence": 0.9,
                    "importance": 0.7,
                    "valid_at": null,
                    "metadata": {}
                }
            ]
        }"#;

        let output = parse_fact_extraction_response(json).expect("parse should succeed");
        let facts = &output.candidates;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "Valid fact");
        // The empty-content fact is gone, but it is no longer gone silently.
        assert_eq!(output.skipped.get(CandidateSkipReason::InvalidCandidate), 1);
        assert_eq!(output.skipped.total(), 1);
        assert_eq!(output.returned_by_model(), 2);
    }

    #[test]
    fn parses_classification_json() {
        let json = r#"{
            "operation": "Add",
            "target_fact_id": null,
            "reason": "No conflict.",
            "confidence": 0.8
        }"#;

        let decision = parse_classification_response(json).expect("parse should succeed");
        assert_eq!(decision.operation, FactOperation::Add);
        assert_eq!(decision.confidence, 0.8);
    }

    #[test]
    fn extracts_output_text_from_response_payload() {
        let response: ResponsesCreateResponse = serde_json::from_value(serde_json::json!({
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "content": [
                        { "type": "output_text", "text": "{\"facts\":[]}" }
                    ]
                }
            ]
        }))
        .expect("fixture should deserialize");

        let text = extract_output_text(&response).expect("text should extract");
        assert_eq!(text, "{\"facts\":[]}");
    }
}
