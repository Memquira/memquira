//! The fact-extraction prompt, built from the one memory-type vocabulary.
//!
//! This lives outside `openai/` so every provider asks for the same thing. Previously the
//! prompt listed a memory-type vocabulary by hand while `parse_memory_type_label` matched on a
//! different one, and facts labelled with the difference were dropped without a trace. The list is
//! now generated from `MemoryType::ALL`, so the prompt and the parser cannot disagree.

use std::sync::LazyLock;

use memcore_core::MemoryType;
use serde_json::Value;

static FACT_EXTRACTION_INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"You extract durable long-term memory facts from conversation messages.
Rules:
- Extract only useful, durable facts worth remembering later.
- Do not store random short-lived details.
- Do not store sensitive data unless the user clearly asks for it.
- Prefer concise fact statements.
- confidence and importance must be floating-point numbers between 0.0 and 1.0 inclusive.
- memory_type must be one of: {vocabulary}.
- Return valid JSON only matching the schema.
- Do not include markdown or commentary."#,
        vocabulary = MemoryType::prompt_vocabulary()
    )
});

/// The system instructions for a fact-extraction call.
pub fn fact_extraction_instructions() -> &'static str {
    FACT_EXTRACTION_INSTRUCTIONS.as_str()
}

/// The JSON schema a fact-extraction response must match.
///
/// `memory_type` carries an explicit `enum` so a model that honours structured output cannot
/// return a label outside the vocabulary in the first place. Labels a model writes anyway are
/// still parsed tolerantly and counted rather than dropped.
pub fn fact_extraction_json_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string" },
                        "memory_type": {
                            "type": "string",
                            "enum": MemoryType::schema_labels(),
                        },
                        "confidence": { "type": "number" },
                        "importance": { "type": "number" },
                        "valid_at": { "type": ["string", "null"] },
                        "metadata": { "type": "object" }
                    },
                    "required": ["content", "memory_type", "confidence", "importance", "valid_at", "metadata"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["facts"],
        "additionalProperties": false
    })
}

/// The structured extraction prompt, used when
/// `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` is on.
///
/// The example uses invented people and dates. Nothing here may come from a third-party dataset
/// (the leakage scan checks every prompt).
static STRUCTURED_FACT_EXTRACTION_INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"You extract durable long-term memory facts from a conversation.
Each input message has an id, a speaker, a role, a time and its text.
Rules:
- Extract only facts worth remembering later. Skip greetings, filler and short-lived details.
- One atomic fact per item. Write `content` as a self-contained sentence that names who the fact is about,
  for example "Priya adopted a rescue dog named Biscuit."
  Do not write dates, times or time expressions such as "yesterday" or "next month" into `content`:
  give them in `event_time`. The resolved date is added to the stored sentence for you.
- `subject`: the name of the person or thing the fact is about. For something a speaker says about
  themselves, use that speaker's name. Never attribute one speaker's fact to the other speaker.
- `event_time`: when the fact happened or will happen, or null when the conversation does not say.
  Do not use a message's own time as the event time unless the message says so, for example "today".
  Resolve relative expressions such as "yesterday", "last Saturday", "two weeks ago" or "next month"
  against the time of the message the expression appears in, never against today's date.
  `start` and `end` are ISO dates (YYYY-MM-DD) covering the whole period; a single day has start equal to end.
  `granularity` is one of: day, week, month, year, approximate.
  `text` is the time expression exactly as written in the message.
- `source_message_ids`: the ids of the messages the fact comes from. Use only ids from the input.
- memory_type must be one of: {vocabulary}.
- confidence and importance must be floating-point numbers between 0.0 and 1.0 inclusive.
- Return valid JSON only matching the schema. Do not include markdown or commentary."#,
        vocabulary = MemoryType::prompt_vocabulary()
    )
});

/// The system instructions for a structured fact-extraction call.
pub fn structured_fact_extraction_instructions() -> &'static str {
    STRUCTURED_FACT_EXTRACTION_INSTRUCTIONS.as_str()
}

/// How much a structured extraction keeps (`MEMCORE_STRUCTURED_EXTRACTION_POLICY`).
///
/// `Retentive` (the default) replaces only the retention rule of `Atomic`,
/// the original Part B prompt, asking the model to keep names, relationships and distinguishing
/// detail; every subject, time and source rule stays the same.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StructuredExtractionPolicy {
    Atomic,
    #[default]
    Retentive,
}

/// The retention rule of the `Atomic` prompt, which `Retentive` replaces.
const ATOMIC_RETENTION_RULE: &str = "- Extract only facts worth remembering later. Skip greetings, filler and short-lived details.\n";

const RETENTIVE_RETENTION_RULE: &str = "- Extract atomic memories worth remembering later. Preserve:
  names of people, places, organizations and objects; relationships between them;
  preferences, goals, experiences, events and important details;
  contextual details that may help answer future questions.
- Do not remove information only because it is descriptive. Keep details that distinguish one
  person, event or object from another.
- Skip greetings, filler, duplicate facts and temporary conversational statements with no future value.
- Prefer complete, useful memories over overly short summaries.
";

static RETENTIVE_STRUCTURED_FACT_EXTRACTION_INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| {
    let atomic = STRUCTURED_FACT_EXTRACTION_INSTRUCTIONS.as_str();
    assert!(
        atomic.contains(ATOMIC_RETENTION_RULE),
        "the structured prompt must contain the retention rule the retentive policy replaces"
    );
    atomic.replacen(ATOMIC_RETENTION_RULE, RETENTIVE_RETENTION_RULE, 1)
});

/// The structured extraction instructions for a retention policy.
pub fn structured_fact_extraction_instructions_for(
    policy: StructuredExtractionPolicy,
) -> &'static str {
    match policy {
        StructuredExtractionPolicy::Atomic => structured_fact_extraction_instructions(),
        StructuredExtractionPolicy::Retentive => {
            RETENTIVE_STRUCTURED_FACT_EXTRACTION_INSTRUCTIONS.as_str()
        }
    }
}

/// The JSON schema a structured fact-extraction response must match.
///
/// Unlike the legacy schema there is no free-form `metadata` object: models filled it by copying
/// the request metadata into every fact, which cost roughly 100 output tokens per fact and carried
/// no information.
pub fn structured_fact_extraction_json_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string" },
                        "memory_type": { "type": "string", "enum": MemoryType::schema_labels() },
                        "subject": { "type": ["string", "null"] },
                        "event_time": {
                            "type": ["object", "null"],
                            "properties": {
                                "start": { "type": "string" },
                                "end": { "type": "string" },
                                "granularity": {
                                    "type": "string",
                                    "enum": ["day", "week", "month", "year", "approximate"]
                                },
                                "text": { "type": ["string", "null"] }
                            },
                            "required": ["start", "end", "granularity", "text"],
                            "additionalProperties": false
                        },
                        "source_message_ids": { "type": "array", "items": { "type": "string" } },
                        "confidence": { "type": "number" },
                        "importance": { "type": "number" }
                    },
                    "required": [
                        "content", "memory_type", "subject", "event_time",
                        "source_message_ids", "confidence", "importance"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "required": ["facts"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression guard for an early bug: whatever the prompt asks the model for must be a
    /// label the parser accepts. A new variant added to only one side fails here.
    #[test]
    fn every_label_the_prompt_offers_is_parseable() {
        let instructions = fact_extraction_instructions();
        let line = instructions
            .lines()
            .find(|line| line.starts_with("- memory_type must be one of:"))
            .expect("prompt should state the memory type vocabulary");
        let listed: Vec<&str> = line
            .trim_end_matches('.')
            .split(':')
            .nth(1)
            .expect("vocabulary after the colon")
            .split(',')
            .map(str::trim)
            .collect();

        assert_eq!(listed.len(), MemoryType::ALL.len());
        for label in listed {
            MemoryType::parse_label(label)
                .unwrap_or_else(|_| panic!("prompt offers {label} but the parser rejects it"));
        }
    }

    #[test]
    fn schema_enum_matches_the_vocabulary() {
        let schema = fact_extraction_json_schema();
        let values = schema["properties"]["facts"]["items"]["properties"]["memory_type"]["enum"]
            .as_array()
            .expect("memory_type should constrain an enum");
        let labels: Vec<&str> = values
            .iter()
            .map(|value| value.as_str().expect("label should be a string"))
            .collect();
        assert_eq!(labels, MemoryType::schema_labels());
    }

    #[test]
    fn structured_prompt_offers_only_parseable_labels_and_resolves_against_the_message() {
        let instructions = structured_fact_extraction_instructions();
        assert!(instructions.contains(&MemoryType::prompt_vocabulary()));
        assert!(instructions.contains("never against today's date"));
        assert!(instructions.contains("source_message_ids"));
        let schema = structured_fact_extraction_json_schema();
        let props = &schema["properties"]["facts"]["items"]["properties"];
        assert!(
            props.get("metadata").is_none(),
            "structured facts must not echo metadata"
        );
        assert_eq!(
            props["memory_type"]["enum"].as_array().unwrap().len(),
            MemoryType::ALL.len()
        );
    }

    #[test]
    fn retentive_policy_changes_only_the_retention_rule() {
        let atomic =
            structured_fact_extraction_instructions_for(StructuredExtractionPolicy::Atomic);
        let retentive =
            structured_fact_extraction_instructions_for(StructuredExtractionPolicy::Retentive);
        assert_eq!(atomic, structured_fact_extraction_instructions());
        assert_ne!(atomic, retentive);
        assert!(!retentive.contains("short-lived details"));
        assert!(retentive.contains("Keep details that distinguish"));
        assert_eq!(
            atomic.replacen(ATOMIC_RETENTION_RULE, "", 1),
            retentive.replacen(RETENTIVE_RETENTION_RULE, "", 1),
            "everything but the retention rule must be shared"
        );
    }
}
