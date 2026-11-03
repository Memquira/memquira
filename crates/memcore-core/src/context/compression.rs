use memcore_common::MemcoreResult;
use serde_json::{Value, json};

use super::format_options::ContextFormat;
use super::formatter::ContextMemoryItem;
use super::token_estimator::TokenEstimator;

/// Output of compressing skipped memories into a short summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedContext {
    pub text: String,
    pub summarized_memories: usize,
    pub estimated_tokens: usize,
    pub bullets: Vec<String>,
}

/// Deterministic local compression without provider calls.
pub struct SimpleContextCompressor;

const MAX_BULLET_CHARS: usize = 200;

impl SimpleContextCompressor {
    pub fn compress(
        memories: &[ContextMemoryItem],
        max_tokens: usize,
        format: ContextFormat,
        include_summary_section: bool,
        estimator: &impl TokenEstimator,
    ) -> MemcoreResult<CompressedContext> {
        if memories.is_empty() {
            return Ok(CompressedContext {
                text: String::new(),
                summarized_memories: 0,
                estimated_tokens: 0,
                bullets: Vec::new(),
            });
        }

        let mut bullets = Vec::new();
        let mut summarized = 0;

        for memory in memories {
            let bullet = bullet_content(&memory.content);
            let candidate_bullets: Vec<String> = bullets
                .iter()
                .cloned()
                .chain(std::iter::once(bullet.clone()))
                .collect();
            let candidate_text =
                format_summary_text(&candidate_bullets, format, include_summary_section);
            if estimator.estimate_tokens(&candidate_text) <= max_tokens {
                bullets.push(bullet);
                summarized += 1;
            } else {
                break;
            }
        }

        let text = format_summary_text(&bullets, format, include_summary_section);
        let estimated_tokens = estimator.estimate_tokens(&text);

        Ok(CompressedContext {
            text,
            summarized_memories: summarized,
            estimated_tokens,
            bullets,
        })
    }
}

pub fn bullet_content(content: &str) -> String {
    let char_count = content.chars().count();
    if char_count <= MAX_BULLET_CHARS {
        return content.to_string();
    }
    let truncated: String = content.chars().take(MAX_BULLET_CHARS).collect();
    format!("{}...", truncated.trim_end())
}

pub fn format_summary_text(
    bullets: &[String],
    format: ContextFormat,
    include_summary_section: bool,
) -> String {
    if bullets.is_empty() {
        return String::new();
    }

    match format {
        ContextFormat::Markdown => {
            let mut lines = Vec::new();
            if include_summary_section {
                lines.push("## Compressed Memory Summary".to_string());
            }
            for bullet in bullets {
                lines.push(format!("- {bullet}"));
            }
            lines.join("\n")
        }
        ContextFormat::Json => {
            serde_json::to_string(&json!({ "summary": bullets })).unwrap_or_default()
        }
        ContextFormat::PlainText => {
            let mut lines = Vec::new();
            if include_summary_section {
                lines.push("Compressed Memory Summary:".to_string());
            }
            for bullet in bullets {
                lines.push(format!("- {bullet}"));
            }
            lines.join("\n")
        }
    }
}

pub fn wrap_provider_summary(
    summary_text: &str,
    summarized_memories: usize,
    format: ContextFormat,
    include_summary_section: bool,
    estimator: &impl TokenEstimator,
) -> CompressedContext {
    let bullets: Vec<String> = summary_text
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();

    let text = if bullets.is_empty() {
        format_summary_text(&[summary_text.to_string()], format, include_summary_section)
    } else {
        format_summary_text(&bullets, format, include_summary_section)
    };

    CompressedContext {
        estimated_tokens: estimator.estimate_tokens(&text),
        summarized_memories,
        bullets,
        text,
    }
}

pub fn merge_context_with_summary(
    main_context: &str,
    format: ContextFormat,
    compressed: &CompressedContext,
) -> String {
    if compressed.text.is_empty() {
        return main_context.to_string();
    }

    match format {
        ContextFormat::Json => merge_json_context(main_context, compressed),
        _ => {
            if main_context.is_empty() {
                compressed.text.clone()
            } else {
                format!("{main_context}\n\n{}", compressed.text)
            }
        }
    }
}

fn merge_json_context(main_context: &str, compressed: &CompressedContext) -> String {
    let mut value: Value =
        serde_json::from_str(main_context).unwrap_or_else(|_| json!({ "memories": [] }));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "compressed_summary".to_string(),
            json!({ "summary": compressed.bullets }),
        );
    }
    serde_json::to_string(&value).unwrap_or_else(|_| compressed.text.clone())
}

pub fn effective_summary_budget(
    available_tokens: usize,
    used_tokens: usize,
    summary_max_tokens: usize,
) -> usize {
    let remaining = available_tokens.saturating_sub(used_tokens);
    summary_max_tokens.min(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryType;
    use crate::context::token_estimator::SimpleTokenEstimator;
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    fn sample_item(content: &str) -> ContextMemoryItem {
        ContextMemoryItem {
            fact_id: Uuid::new_v4(),
            content: content.to_string(),
            memory_type: MemoryType::Skill,
            score: 0.8,
            confidence: 0.9,
            importance: 0.85,
            valid_at: Some(Utc::now()),
            metadata: json!({}),
            structure: Default::default(),
        }
    }

    #[test]
    fn compresses_ranked_memories_into_bullets() {
        let memories = vec![
            sample_item("User is building memcore."),
            sample_item("User prefers practical backend explanations."),
        ];
        let compressed = SimpleContextCompressor::compress(
            &memories,
            500,
            ContextFormat::Markdown,
            true,
            &SimpleTokenEstimator,
        )
        .unwrap();

        assert_eq!(compressed.summarized_memories, 2);
        assert!(compressed.text.contains("## Compressed Memory Summary"));
        assert!(compressed.text.contains("User is building memcore."));
    }

    #[test]
    fn respects_summary_max_tokens() {
        let memories: Vec<_> = (0..20)
            .map(|index| sample_item(&format!("memory item number {index} with extra words")))
            .collect();
        let compressed = SimpleContextCompressor::compress(
            &memories,
            30,
            ContextFormat::PlainText,
            true,
            &SimpleTokenEstimator,
        )
        .unwrap();

        assert!(compressed.summarized_memories < memories.len());
        assert!(compressed.estimated_tokens <= 30);
    }

    #[test]
    fn empty_input_returns_empty_summary() {
        let compressed = SimpleContextCompressor::compress(
            &[],
            100,
            ContextFormat::PlainText,
            true,
            &SimpleTokenEstimator,
        )
        .unwrap();
        assert!(compressed.text.is_empty());
        assert_eq!(compressed.summarized_memories, 0);
    }

    #[test]
    fn output_is_deterministic() {
        let memories = vec![sample_item("deterministic compression sample")];
        let first = SimpleContextCompressor::compress(
            &memories,
            200,
            ContextFormat::PlainText,
            true,
            &SimpleTokenEstimator,
        )
        .unwrap();
        let second = SimpleContextCompressor::compress(
            &memories,
            200,
            ContextFormat::PlainText,
            true,
            &SimpleTokenEstimator,
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn long_memories_are_truncated_safely() {
        let long = "word ".repeat(100);
        let bullet = bullet_content(&long);
        assert!(bullet.chars().count() <= MAX_BULLET_CHARS + 3);
    }

    #[test]
    fn no_sensitive_metadata_in_compressed_output() {
        let mut item = sample_item("safe summary content");
        item.metadata = json!({
            "input_text": "secret",
            "api_key": "mc_live_secret"
        });
        let compressed = SimpleContextCompressor::compress(
            &[item],
            200,
            ContextFormat::PlainText,
            true,
            &SimpleTokenEstimator,
        )
        .unwrap();
        assert!(!compressed.text.contains("mc_live_secret"));
        assert!(!compressed.text.contains("secret"));
        assert!(compressed.text.contains("safe summary content"));
    }
}
