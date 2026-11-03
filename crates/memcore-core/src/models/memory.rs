use std::fmt;
use std::str::FromStr;

use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};

/// Declares the memory-type vocabulary once.
///
/// Previously the vocabulary lived in six hand-written tables — the extraction prompt, the
/// provider label parser, the SQLite/Postgres/Qdrant/LanceDB converters and the API DTO — and they
/// had drifted: the prompt asked the model for `Goal`, `Relationship`, `Event`, `Knowledge`,
/// `Instruction` and `Other`, none of which the parser accepted, so those facts were dropped
/// without a log or a metric. Everything now derives from this one list, and
/// `serde_label_matches_as_str` pins the serde representation to `as_str` so the two cannot
/// separate again.
macro_rules! memory_type_vocabulary {
    ($($variant:ident => $label:literal),+ $(,)?) => {
        /// The kind of durable memory a fact represents.
        ///
        /// Serialized as `snake_case`; `as_str` returns the same strings and is what storage
        /// backends persist. New variants are additive — existing rows keep their labels.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum MemoryType {
            $($variant,)+
        }

        impl MemoryType {
            /// Every variant, in declaration order. Drives the extraction prompt, the JSON schema
            /// sent to the model, and the exhaustiveness tests.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The canonical `snake_case` label. This is the stored and serialized form.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                }
            }

            /// The `PascalCase` label used when asking a model for a memory type.
            pub const fn prompt_label(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)+
                }
            }
        }
    };
}

memory_type_vocabulary! {
    Profile => "profile",
    Preference => "preference",
    Project => "project",
    Conversation => "conversation",
    Task => "task",
    Entity => "entity",
    Skill => "skill",
    System => "system",
    // Added later: the extraction prompt had been asking for these all along.
    Goal => "goal",
    Relationship => "relationship",
    Event => "event",
    Knowledge => "knowledge",
    Instruction => "instruction",
    Other => "other",
}

impl MemoryType {
    /// The comma-separated `PascalCase` list to put in an extraction prompt.
    pub fn prompt_vocabulary() -> String {
        Self::ALL
            .iter()
            .map(|value| value.prompt_label())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The canonical labels, for a JSON-schema `enum` constraint.
    pub fn schema_labels() -> Vec<&'static str> {
        Self::ALL.iter().map(|value| value.as_str()).collect()
    }

    /// Parses a label written by a model or a client.
    ///
    /// Accepts `snake_case`, `PascalCase`, `camelCase`, spaces or hyphens as separators, and any
    /// casing, so `"Knowledge"`, `"knowledge"` and `"KNOWLEDGE"` all resolve. An unknown label is
    /// an error the caller must account for — never a silent skip.
    pub fn parse_label(value: &str) -> MemcoreResult<Self> {
        let normalized = normalize_label(value);
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == normalized)
            .ok_or_else(|| {
                MemcoreError::ValidationError(format!("invalid memory_type from model: {value}"))
            })
    }
}

/// Lowercases and reduces any separator style to `snake_case`.
fn normalize_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    let mut previous_was_lower_or_digit = false;
    for ch in value.trim().chars() {
        if ch == ' ' || ch == '-' || ch == '_' {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            previous_was_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() {
            // Split camelCase and PascalCase, but leave runs of capitals intact so an
            // all-caps label normalizes to a single word rather than one word per letter.
            if previous_was_lower_or_digit && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_was_lower_or_digit = false;
        } else {
            out.push(ch);
            previous_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

impl fmt::Display for MemoryType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MemoryType {
    type Err = MemcoreError;

    fn from_str(value: &str) -> MemcoreResult<Self> {
        Self::parse_label(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    UserMessage,
    AssistantMessage,
    ApiImport,
    Manual,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactOperation {
    Add,
    Update,
    Delete,
    NoOp,
    Archive,
    Summarize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason the vocabulary could drift before: `serde` derived its labels from the variant
    /// names while every other table wrote them out by hand. Pinning the two together here means a
    /// new variant cannot be added with a mismatched label.
    #[test]
    fn serde_label_matches_as_str() {
        for memory_type in MemoryType::ALL {
            let encoded = serde_json::to_string(memory_type).expect("memory type should serialize");
            assert_eq!(encoded, format!("\"{}\"", memory_type.as_str()));
            let decoded: MemoryType =
                serde_json::from_str(&encoded).expect("memory type should deserialize");
            assert_eq!(decoded, *memory_type);
        }
    }

    #[test]
    fn every_variant_round_trips_through_parse_label() {
        for memory_type in MemoryType::ALL {
            assert_eq!(
                MemoryType::parse_label(memory_type.as_str()).expect("snake label"),
                *memory_type
            );
            assert_eq!(
                MemoryType::parse_label(memory_type.prompt_label()).expect("pascal label"),
                *memory_type
            );
        }
    }

    /// A fixed early bug: the extraction prompt asked for these eight and the parser rejected six
    /// of them, so those facts were dropped. Every one must parse now.
    #[test]
    fn labels_the_extraction_prompt_asks_for_are_all_accepted() {
        for label in [
            "Preference",
            "Profile",
            "Goal",
            "Relationship",
            "Event",
            "Knowledge",
            "Instruction",
            "Other",
        ] {
            MemoryType::parse_label(label)
                .unwrap_or_else(|_| panic!("prompt vocabulary label {label} should parse"));
        }
    }

    #[test]
    fn parse_label_tolerates_separator_and_casing_variation() {
        assert_eq!(
            MemoryType::parse_label("  KNOWLEDGE ").expect("uppercase"),
            MemoryType::Knowledge
        );
        assert_eq!(
            MemoryType::parse_label("relationship").expect("lowercase"),
            MemoryType::Relationship
        );
        assert_eq!(
            MemoryType::parse_label("Other").expect("pascal"),
            MemoryType::Other
        );
    }

    #[test]
    fn unknown_labels_are_rejected_rather_than_mapped_to_other() {
        let error = MemoryType::parse_label("Sandwich").expect_err("unknown label should fail");
        assert!(error.to_string().contains("Sandwich"));
    }

    #[test]
    fn prompt_vocabulary_lists_every_variant() {
        let vocabulary = MemoryType::prompt_vocabulary();
        for memory_type in MemoryType::ALL {
            assert!(
                vocabulary.contains(memory_type.prompt_label()),
                "{} missing from prompt vocabulary",
                memory_type.prompt_label()
            );
        }
    }

    /// Existing rows and existing clients must keep working after the vocabulary widened.
    #[test]
    fn pre_phase_1_labels_are_unchanged() {
        for (label, expected) in [
            ("profile", MemoryType::Profile),
            ("preference", MemoryType::Preference),
            ("project", MemoryType::Project),
            ("conversation", MemoryType::Conversation),
            ("task", MemoryType::Task),
            ("entity", MemoryType::Entity),
            ("skill", MemoryType::Skill),
            ("system", MemoryType::System),
        ] {
            assert_eq!(expected.as_str(), label);
            assert_eq!(
                MemoryType::parse_label(label).expect("legacy label"),
                expected
            );
        }
    }
}
