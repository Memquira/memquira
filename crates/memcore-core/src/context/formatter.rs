use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::MemorySearchResult;
use crate::MemoryType;

use super::format_options::{ContextFormat, ContextFormatOptions};
use super::types::EMPTY_CONTEXT_MESSAGE;

/// Memory item prepared for context formatting.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMemoryItem {
    pub fact_id: Uuid,
    pub content: String,
    pub memory_type: MemoryType,
    pub score: f32,
    pub confidence: f32,
    pub importance: f32,
    pub valid_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    /// Subject, event time and provenance; empty when not extracted.
    pub structure: crate::FactStructure,
}

impl From<&MemorySearchResult> for ContextMemoryItem {
    fn from(result: &MemorySearchResult) -> Self {
        Self {
            fact_id: result.fact_id,
            content: result.content.clone(),
            memory_type: result.memory_type,
            score: result.score,
            confidence: result.confidence,
            importance: result.importance,
            valid_at: result.valid_at,
            metadata: result.metadata.clone(),
            structure: result.structure.clone(),
        }
    }
}

/// Rendered context output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedContext {
    pub context: String,
}

/// Formats ranked memory items into plain text, markdown, or JSON context strings.
pub struct ContextFormatter;

/// Every memory type, in the order sections are rendered.
///
/// This listed only the original eight types. When Part A widened the vocabulary, memories of the
/// six new types were silently dropped from sectioned context — and they are most of what
/// extraction stores. `section_order_covers_every_memory_type` now guards against that recurring.
const SECTION_ORDER: [MemoryType; 14] = [
    MemoryType::Profile,
    MemoryType::Preference,
    MemoryType::Project,
    MemoryType::Skill,
    MemoryType::Task,
    MemoryType::Entity,
    MemoryType::Conversation,
    MemoryType::System,
    MemoryType::Relationship,
    MemoryType::Goal,
    MemoryType::Event,
    MemoryType::Knowledge,
    MemoryType::Instruction,
    MemoryType::Other,
];

/// When a structured memory happened, or failing that when it was said. `None` for memories
/// written without structured extraction.
fn structured_time(memory: &ContextMemoryItem) -> Option<DateTime<Utc>> {
    memory
        .structure
        .event_time
        .as_ref()
        .map(|event_time| event_time.start)
        .or(memory.structure.observed_at)
}

/// A compact rendering of an event time at its own precision.
fn format_event_time(event_time: &crate::TemporalValue) -> String {
    use crate::TemporalGranularity;
    match event_time.granularity {
        TemporalGranularity::Day => event_time.start.format("%Y-%m-%d").to_string(),
        TemporalGranularity::Month => event_time.start.format("%Y-%m").to_string(),
        TemporalGranularity::Year => event_time.start.format("%Y").to_string(),
        TemporalGranularity::Week | TemporalGranularity::Approximate => format!(
            "{}..{} ({})",
            event_time.start.format("%Y-%m-%d"),
            event_time.end.format("%Y-%m-%d"),
            event_time.granularity
        ),
    }
}

impl ContextFormatter {
    pub fn format(
        memories: &[ContextMemoryItem],
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> MemcoreResult<FormattedContext> {
        if memories.is_empty() {
            return Ok(FormattedContext {
                context: EMPTY_CONTEXT_MESSAGE.to_string(),
            });
        }

        // With timestamps on, structured memories are listed in the order they happened, so a
        // reader sees a timeline. Which memories were selected is still decided by relevance, and
        // memories without a structured time keep their relevance order after the dated ones. Output
        // for memories written without structured extraction is unchanged.
        let chronological;
        let memories = if options.include_timestamps
            && memories
                .iter()
                .any(|memory| structured_time(memory).is_some())
        {
            let mut ordered = memories.to_vec();
            ordered.sort_by_key(|memory| {
                let time = structured_time(memory);
                (time.is_none(), time)
            });
            chronological = ordered;
            chronological.as_slice()
        } else {
            memories
        };

        let context = match options.format {
            ContextFormat::PlainText => {
                Self::format_plain(memories, options, legacy_include_fact_metadata)
            }
            ContextFormat::Markdown => {
                Self::format_markdown(memories, options, legacy_include_fact_metadata)
            }
            ContextFormat::Json => {
                Self::format_json(memories, options, legacy_include_fact_metadata)?
            }
        };

        Ok(FormattedContext { context })
    }

    fn format_plain(
        memories: &[ContextMemoryItem],
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> String {
        if options.is_legacy_plain() && !legacy_include_fact_metadata {
            let mut lines = vec!["Relevant long-term memories:".to_string()];
            for memory in memories {
                lines.push(format!("- {}", memory.content));
            }
            return lines.join("\n");
        }

        if options.section_by_memory_type {
            let mut blocks = Vec::new();
            for memory_type in SECTION_ORDER {
                let section_items: Vec<_> = memories
                    .iter()
                    .filter(|memory| memory.memory_type == memory_type)
                    .collect();
                if section_items.is_empty() {
                    continue;
                }
                let mut lines = vec![format!("{}:", section_title(memory_type))];
                for memory in section_items {
                    lines.push(Self::format_plain_line(
                        memory,
                        options,
                        legacy_include_fact_metadata,
                    ));
                }
                blocks.push(lines.join("\n"));
            }
            return blocks.join("\n\n");
        }

        let mut lines: Vec<String> = memories
            .iter()
            .map(|memory| Self::format_plain_line(memory, options, legacy_include_fact_metadata))
            .collect();

        if options.is_legacy_plain() && legacy_include_fact_metadata {
            let mut with_header = vec!["Relevant long-term memories:".to_string()];
            with_header.append(&mut lines);
            return with_header.join("\n");
        }

        lines.join("\n")
    }

    fn format_plain_line(
        memory: &ContextMemoryItem,
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> String {
        let mut line = format!("- {}", memory.content);
        if legacy_include_fact_metadata
            && !memory.metadata.is_null()
            && memory.metadata != json!({})
        {
            line.push_str(&format!(" (metadata: {})", memory.metadata));
        }
        if let Some(suffix) = Self::plain_metadata_suffix(memory, options) {
            line.push_str(&suffix);
        }
        line
    }

    fn plain_metadata_suffix(
        memory: &ContextMemoryItem,
        options: &ContextFormatOptions,
    ) -> Option<String> {
        let parts = Self::metadata_parts(memory, options);
        if parts.is_empty() {
            return None;
        }
        Some(format!(" [{}]", parts.join(" ")))
    }

    fn format_markdown(
        memories: &[ContextMemoryItem],
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> String {
        if options.section_by_memory_type {
            let mut blocks = Vec::new();
            for memory_type in SECTION_ORDER {
                let section_items: Vec<_> = memories
                    .iter()
                    .filter(|memory| memory.memory_type == memory_type)
                    .collect();
                if section_items.is_empty() {
                    continue;
                }
                let mut lines = vec![format!("## {}", section_title(memory_type))];
                for memory in section_items {
                    lines.push(Self::format_markdown_line(
                        memory,
                        options,
                        legacy_include_fact_metadata,
                    ));
                }
                blocks.push(lines.join("\n"));
            }
            return blocks.join("\n\n");
        }

        memories
            .iter()
            .map(|memory| Self::format_markdown_line(memory, options, legacy_include_fact_metadata))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn format_markdown_line(
        memory: &ContextMemoryItem,
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> String {
        let mut line = format!("- {}", memory.content);
        if legacy_include_fact_metadata
            && !memory.metadata.is_null()
            && memory.metadata != json!({})
        {
            line.push_str(&format!(" (metadata: {})", memory.metadata));
        }
        if let Some(suffix) = Self::markdown_metadata_suffix(memory, options) {
            line.push_str(&suffix);
        }
        line
    }

    fn markdown_metadata_suffix(
        memory: &ContextMemoryItem,
        options: &ContextFormatOptions,
    ) -> Option<String> {
        let parts = Self::metadata_parts(memory, options);
        if parts.is_empty() {
            return None;
        }
        Some(format!(" _({})_", parts.join(", ")))
    }

    fn format_json(
        memories: &[ContextMemoryItem],
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> MemcoreResult<String> {
        let payload = if options.section_by_memory_type {
            let mut sections = Vec::new();
            for memory_type in SECTION_ORDER {
                let section_items: Vec<_> = memories
                    .iter()
                    .filter(|memory| memory.memory_type == memory_type)
                    .collect();
                if section_items.is_empty() {
                    continue;
                }
                sections.push(json!({
                    "type": section_title(memory_type),
                    "memories": section_items
                        .iter()
                        .map(|memory| Self::memory_json_object(memory, options, legacy_include_fact_metadata))
                        .collect::<Vec<_>>(),
                }));
            }
            json!({ "sections": sections })
        } else {
            json!({
                "memories": memories
                    .iter()
                    .map(|memory| Self::memory_json_object(memory, options, legacy_include_fact_metadata))
                    .collect::<Vec<_>>(),
            })
        };

        Ok(serde_json::to_string(&payload).expect("context json serialization"))
    }

    fn memory_json_object(
        memory: &ContextMemoryItem,
        options: &ContextFormatOptions,
        legacy_include_fact_metadata: bool,
    ) -> Value {
        let mut object = json!({ "content": memory.content });

        if let Some(map) = object.as_object_mut() {
            if options.include_memory_ids {
                map.insert("memory_id".to_string(), json!(memory.fact_id.to_string()));
            }
            if options.include_memory_types {
                map.insert(
                    "memory_type".to_string(),
                    json!(memory_type_label(memory.memory_type)),
                );
            }
            if options.include_scores {
                map.insert("score".to_string(), json!(memory.score));
            }
            if options.include_timestamps {
                let structure = &memory.structure;
                if let Some(subject) = &structure.subject {
                    map.insert("subject".to_string(), json!(subject));
                }
                if let Some(event_time) = &structure.event_time {
                    map.insert(
                        "event_time".to_string(),
                        json!({
                            "start": event_time.start.to_rfc3339(),
                            "end": event_time.end.to_rfc3339(),
                            "granularity": event_time.granularity.as_str(),
                        }),
                    );
                } else if let Some(observed_at) = structure.observed_at {
                    // Labelled as when it was said, not when it happened.
                    map.insert("observed_at".to_string(), json!(observed_at.to_rfc3339()));
                } else if let Some(timestamp) = memory.valid_at {
                    map.insert("valid_at".to_string(), json!(timestamp.to_rfc3339()));
                }
            }
            if options.include_confidence {
                map.insert("confidence".to_string(), json!(memory.confidence));
            }
            if options.include_importance {
                map.insert("importance".to_string(), json!(memory.importance));
            }
            if legacy_include_fact_metadata
                && !memory.metadata.is_null()
                && memory.metadata != json!({})
            {
                map.insert("metadata".to_string(), memory.metadata.clone());
            }
        }

        object
    }

    fn metadata_parts(memory: &ContextMemoryItem, options: &ContextFormatOptions) -> Vec<String> {
        let mut parts = Vec::new();
        if options.include_memory_ids {
            parts.push(format!("id={}", memory.fact_id));
        }
        if options.include_memory_types {
            parts.push(format!("type={}", memory_type_label(memory.memory_type)));
        }
        if options.include_scores {
            parts.push(format!("score={:.2}", memory.score));
        }
        if options.include_timestamps {
            let structure = &memory.structure;
            if let Some(subject) = &structure.subject {
                parts.push(format!("subject={subject}"));
            }
            if let Some(event_time) = &structure.event_time {
                parts.push(format!("event_time={}", format_event_time(event_time)));
            } else if let Some(observed_at) = structure.observed_at {
                // Labelled as when it was said, not when it happened.
                parts.push(format!("observed_at={}", observed_at.format("%Y-%m-%d")));
            } else if let Some(timestamp) = memory.valid_at {
                parts.push(format!("valid_at={}", timestamp.to_rfc3339()));
            }
        }
        if options.include_confidence {
            parts.push(format!("confidence={:.2}", memory.confidence));
        }
        if options.include_importance {
            parts.push(format!("importance={:.2}", memory.importance));
        }
        parts
    }
}

pub fn section_title(memory_type: MemoryType) -> &'static str {
    match memory_type {
        MemoryType::Profile => "Profile",
        MemoryType::Preference => "Preferences",
        MemoryType::Project => "Projects",
        MemoryType::Skill => "Skills",
        MemoryType::Task => "Tasks",
        MemoryType::Entity => "Entities",
        MemoryType::Conversation => "Conversation",
        MemoryType::System => "System",
        MemoryType::Goal => "Goals",
        MemoryType::Relationship => "Relationships",
        MemoryType::Event => "Events",
        MemoryType::Knowledge => "Knowledge",
        MemoryType::Instruction => "Instructions",
        MemoryType::Other => "Other",
    }
}

/// The singular label for one memory. This is the same `PascalCase` vocabulary the extraction
/// prompt uses, so it is taken from the one place that vocabulary is declared.
pub fn memory_type_label(memory_type: MemoryType) -> &'static str {
    memory_type.prompt_label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_item(content: &str, memory_type: MemoryType, score: f32) -> ContextMemoryItem {
        ContextMemoryItem {
            fact_id: Uuid::new_v4(),
            content: content.to_string(),
            memory_type,
            score,
            confidence: 0.85,
            importance: 0.9,
            valid_at: Some(Utc::now()),
            metadata: json!({}),
            structure: Default::default(),
        }
    }

    fn structured_item(
        content: &str,
        subject: Option<&str>,
        event_day: Option<(i32, u32, u32)>,
        observed_day: Option<(i32, u32, u32)>,
    ) -> ContextMemoryItem {
        use chrono::TimeZone;
        let day = |(y, m, d): (i32, u32, u32)| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();
        let mut item = sample_item(content, MemoryType::Event, 0.9);
        item.valid_at = None;
        item.structure = crate::FactStructure {
            subject: subject.map(str::to_string),
            event_time: event_day.map(|value| {
                crate::TemporalValue::new(
                    day(value),
                    day(value),
                    crate::TemporalGranularity::Day,
                    day(value),
                )
                .unwrap()
            }),
            observed_at: observed_day.map(day),
            ..Default::default()
        };
        item
    }

    fn timestamped() -> ContextFormatOptions {
        ContextFormatOptions {
            format: ContextFormat::PlainText,
            include_timestamps: true,
            ..ContextFormatOptions::default()
        }
    }

    #[test]
    fn section_order_covers_every_memory_type() {
        for memory_type in MemoryType::ALL {
            assert!(
                SECTION_ORDER.contains(memory_type),
                "{memory_type} has no section, so sectioned context would drop it"
            );
        }
    }

    #[test]
    fn sectioned_context_keeps_memories_of_the_part_a_types() {
        let items = vec![
            sample_item("A knowledge fact", MemoryType::Knowledge, 0.9),
            sample_item("An event fact", MemoryType::Event, 0.8),
        ];
        let options = ContextFormatOptions {
            format: ContextFormat::Markdown,
            section_by_memory_type: true,
            ..ContextFormatOptions::default()
        };
        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        assert!(formatted.context.contains("A knowledge fact"));
        assert!(formatted.context.contains("An event fact"));
    }

    #[test]
    fn structured_memories_show_subject_and_event_time() {
        let items = vec![structured_item(
            "Priya ran a half marathon.",
            Some("Priya"),
            Some((2023, 5, 6)),
            Some((2023, 5, 8)),
        )];
        let formatted = ContextFormatter::format(&items, &timestamped(), false).unwrap();
        assert!(
            formatted.context.contains("subject=Priya"),
            "{}",
            formatted.context
        );
        assert!(
            formatted.context.contains("event_time=2023-05-06"),
            "{}",
            formatted.context
        );
    }

    #[test]
    fn a_memory_without_an_event_time_is_labelled_as_observed() {
        let items = vec![structured_item(
            "Likes tea.",
            Some("Mel"),
            None,
            Some((2023, 5, 8)),
        )];
        let formatted = ContextFormatter::format(&items, &timestamped(), false).unwrap();
        assert!(
            formatted.context.contains("observed_at=2023-05-08"),
            "{}",
            formatted.context
        );
        assert!(!formatted.context.contains("event_time="));
    }

    #[test]
    fn structured_memories_are_ordered_chronologically_with_undated_ones_last() {
        let items = vec![
            structured_item("third", None, Some((2023, 7, 1)), None),
            sample_item("undated", MemoryType::Event, 0.95),
            structured_item("first", None, Some((2023, 5, 1)), None),
            structured_item("second", None, None, Some((2023, 6, 1))),
        ];
        let formatted = ContextFormatter::format(&items, &timestamped(), false).unwrap();
        let context = formatted.context;
        let position = |needle: &str| context.find(needle).unwrap();
        assert!(position("first") < position("second"));
        assert!(position("second") < position("third"));
        assert!(position("third") < position("undated"));
    }

    #[test]
    fn legacy_memories_render_and_order_exactly_as_before() {
        let items = vec![
            sample_item("b", MemoryType::Profile, 0.9),
            sample_item("a", MemoryType::Profile, 0.8),
        ];
        let formatted = ContextFormatter::format(&items, &timestamped(), false).unwrap();
        assert!(formatted.context.find("b").unwrap() < formatted.context.find("a").unwrap());
        assert!(formatted.context.contains("valid_at="));
        assert!(!formatted.context.contains("subject="));
        assert!(!formatted.context.contains("event_time="));
    }

    #[test]
    fn markdown_format_without_sections() {
        let items = vec![
            sample_item("User is building memcore.", MemoryType::Project, 0.9),
            sample_item(
                "User prefers concise explanations.",
                MemoryType::Preference,
                0.8,
            ),
        ];
        let options = ContextFormatOptions {
            format: ContextFormat::Markdown,
            ..ContextFormatOptions::default()
        };

        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        assert!(formatted.context.contains("- User is building memcore."));
        assert!(!formatted.context.contains("## "));
    }

    #[test]
    fn markdown_format_with_sections() {
        let items = vec![
            sample_item("User is a full-stack developer.", MemoryType::Profile, 0.9),
            sample_item(
                "User prefers concise technical explanations.",
                MemoryType::Preference,
                0.8,
            ),
            sample_item("User is building memcore.", MemoryType::Project, 0.7),
        ];
        let options = ContextFormatOptions::structured_markdown();

        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        let profile_pos = formatted.context.find("## Profile").unwrap();
        let preferences_pos = formatted.context.find("## Preferences").unwrap();
        let projects_pos = formatted.context.find("## Projects").unwrap();
        assert!(profile_pos < preferences_pos);
        assert!(preferences_pos < projects_pos);
    }

    #[test]
    fn plain_text_format_with_sections() {
        let items = vec![sample_item(
            "User is a developer.",
            MemoryType::Profile,
            0.9,
        )];
        let options = ContextFormatOptions {
            format: ContextFormat::PlainText,
            section_by_memory_type: true,
            ..ContextFormatOptions::default()
        };

        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        assert!(formatted.context.starts_with("Profile:\n- "));
    }

    #[test]
    fn json_format_without_sections() {
        let items = vec![sample_item(
            "User is building memcore.",
            MemoryType::Project,
            0.82,
        )];
        let options = ContextFormatOptions {
            format: ContextFormat::Json,
            ..ContextFormatOptions::default()
        };

        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        let value: Value = serde_json::from_str(&formatted.context).unwrap();
        assert_eq!(value["memories"][0]["content"], "User is building memcore.");
    }

    #[test]
    fn json_format_with_sections() {
        let items = vec![sample_item(
            "User is a developer.",
            MemoryType::Profile,
            0.9,
        )];
        let options = ContextFormatOptions {
            format: ContextFormat::Json,
            section_by_memory_type: true,
            ..ContextFormatOptions::default()
        };

        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        let value: Value = serde_json::from_str(&formatted.context).unwrap();
        assert_eq!(value["sections"][0]["type"], "Profile");
    }

    #[test]
    fn ranked_order_preserved_inside_section() {
        let items = vec![
            sample_item("first project", MemoryType::Project, 0.9),
            sample_item("second project", MemoryType::Project, 0.8),
        ];
        let options = ContextFormatOptions::structured_markdown();
        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        let first = formatted.context.find("first project").unwrap();
        let second = formatted.context.find("second project").unwrap();
        assert!(first < second);
    }

    #[test]
    fn metadata_excluded_by_default() {
        let items = vec![sample_item("content", MemoryType::Skill, 0.5)];
        let formatted =
            ContextFormatter::format(&items, &ContextFormatOptions::default(), false).unwrap();
        assert!(!formatted.context.contains("score"));
        assert!(!formatted.context.contains("memory_id"));
    }

    #[test]
    fn memory_ids_included_only_when_requested() {
        let item = sample_item("content", MemoryType::Skill, 0.5);
        let fact_id = item.fact_id;
        let options = ContextFormatOptions {
            include_memory_ids: true,
            ..ContextFormatOptions::default()
        };
        let formatted = ContextFormatter::format(&[item], &options, false).unwrap();
        assert!(formatted.context.contains(&fact_id.to_string()));
    }

    #[test]
    fn scores_included_only_when_requested() {
        let items = vec![sample_item("content", MemoryType::Project, 0.82)];
        let options = ContextFormatOptions {
            format: ContextFormat::Markdown,
            include_scores: true,
            include_memory_types: true,
            ..ContextFormatOptions::default()
        };
        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        assert!(formatted.context.contains("score=0.82") || formatted.context.contains("0.82"));
    }

    #[test]
    fn timestamps_included_only_when_requested() {
        let items = vec![sample_item("content", MemoryType::Skill, 0.5)];
        let options = ContextFormatOptions {
            include_timestamps: true,
            ..ContextFormatOptions::default()
        };
        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        assert!(formatted.context.contains("valid_at="));
    }

    #[test]
    fn confidence_and_importance_included_only_when_requested() {
        let items = vec![sample_item("content", MemoryType::Skill, 0.5)];
        let options = ContextFormatOptions {
            include_confidence: true,
            include_importance: true,
            ..ContextFormatOptions::default()
        };
        let formatted = ContextFormatter::format(&items, &options, false).unwrap();
        assert!(formatted.context.contains("confidence="));
        assert!(formatted.context.contains("importance="));
    }

    #[test]
    fn no_sensitive_fields_in_formatted_output_by_default() {
        let mut item = sample_item("safe content", MemoryType::System, 0.5);
        item.metadata = json!({
            "input_text": "secret user message",
            "api_key": "mc_live_secret",
            "key_hash": "deadbeef"
        });
        let options = ContextFormatOptions {
            format: ContextFormat::Json,
            ..ContextFormatOptions::default()
        };
        let formatted = ContextFormatter::format(&[item], &options, false).unwrap();
        assert!(!formatted.context.contains("mc_live_secret"));
        assert!(!formatted.context.contains("secret user message"));
        assert!(!formatted.context.contains("deadbeef"));
    }
}
