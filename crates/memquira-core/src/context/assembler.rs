use crate::MemorySearchResult;

use super::budget::{ContextBudget, ContextBudgetUsage};
use super::compression::{
    CompressedContext, SimpleContextCompressor, effective_summary_budget,
    merge_context_with_summary,
};
use super::compression_options::{
    ContextCompressionMode, ContextCompressionOptions, ContextCompressionUsage,
};
use super::format_options::ContextFormatOptions;
use super::formatter::{ContextFormatter, ContextMemoryItem};
use super::token_estimator::{SimpleTokenEstimator, TokenEstimator};
use super::types::EMPTY_CONTEXT_MESSAGE;

/// Result of budget-aware context assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledContext {
    pub context: String,
    pub memories: Vec<MemorySearchResult>,
    pub budget: ContextBudgetUsage,
    pub compression: ContextCompressionUsage,
    /// Skipped memories retained for async provider summarization.
    pub skipped_items: Vec<ContextMemoryItem>,
}

/// Formats search hits into a simple bullet-list context block for agents.
pub fn assemble_context(memories: &[MemorySearchResult], include_metadata: bool) -> String {
    assemble_context_with_budget(
        memories,
        include_metadata,
        &ContextFormatOptions::default(),
        &ContextBudget {
            max_tokens: usize::MAX / 4,
            reserved_tokens: 0,
        },
        &ContextCompressionOptions::default(),
        &SimpleTokenEstimator,
    )
    .context
}

/// Assembles context from ranked memories while enforcing token budget and optional compression.
pub fn assemble_context_with_budget(
    memories: &[MemorySearchResult],
    legacy_include_fact_metadata: bool,
    format_options: &ContextFormatOptions,
    budget: &ContextBudget,
    compression_options: &ContextCompressionOptions,
    estimator: &impl TokenEstimator,
) -> AssembledContext {
    let available = budget.available_tokens();
    let total_candidates = memories.len();
    let summary_reserve = if compression_options.mode.is_enabled() {
        compression_options.summary_max_tokens.min(available)
    } else {
        0
    };
    let memory_available = available.saturating_sub(summary_reserve);

    if memories.is_empty() {
        let context = EMPTY_CONTEXT_MESSAGE.to_string();
        return AssembledContext {
            context: context.clone(),
            memories: Vec::new(),
            budget: ContextBudgetUsage {
                max_tokens: budget.max_tokens,
                reserved_tokens: budget.reserved_tokens,
                available_tokens: available,
                used_tokens: estimator.estimate_tokens(&context),
                included_memories: 0,
                skipped_memories: 0,
            },
            compression: ContextCompressionUsage::disabled(),
            skipped_items: Vec::new(),
        };
    }

    let items: Vec<ContextMemoryItem> = memories.iter().map(ContextMemoryItem::from).collect();
    let mut included_items: Vec<ContextMemoryItem> = Vec::new();
    let mut included_memories: Vec<MemorySearchResult> = Vec::new();
    let mut skipped_items: Vec<ContextMemoryItem> = Vec::new();
    let mut skipped = 0;

    for (item, memory) in items.iter().zip(memories.iter()) {
        let delta = token_delta_for_addition(
            &included_items,
            item,
            format_options,
            legacy_include_fact_metadata,
            estimator,
        );

        let used_so_far = if included_items.is_empty() {
            0
        } else {
            estimate_context_tokens(
                &included_items,
                format_options,
                legacy_include_fact_metadata,
                estimator,
            )
        };

        if used_so_far + delta <= memory_available {
            included_items.push(item.clone());
            included_memories.push(memory.clone());
        } else {
            skipped_items.push(item.clone());
            skipped += 1;
        }
    }

    if included_items.is_empty() && skipped_items.is_empty() {
        let context = EMPTY_CONTEXT_MESSAGE.to_string();
        return AssembledContext {
            context: context.clone(),
            memories: Vec::new(),
            budget: ContextBudgetUsage {
                max_tokens: budget.max_tokens,
                reserved_tokens: budget.reserved_tokens,
                available_tokens: available,
                used_tokens: estimator.estimate_tokens(&context),
                included_memories: 0,
                skipped_memories: total_candidates,
            },
            compression: ContextCompressionUsage::disabled(),
            skipped_items: Vec::new(),
        };
    }

    let mut context = if included_items.is_empty() {
        String::new()
    } else {
        ContextFormatter::format(
            &included_items,
            format_options,
            legacy_include_fact_metadata,
        )
        .expect("context formatting")
        .context
    };

    let mut compression = ContextCompressionUsage::disabled();

    if matches!(
        compression_options.mode,
        ContextCompressionMode::SimpleExtractive
    ) && !skipped_items.is_empty()
    {
        let used_before_summary = if context.is_empty() {
            0
        } else {
            estimator.estimate_tokens(&context)
        };
        let summary_budget = effective_summary_budget(
            available,
            used_before_summary,
            compression_options.summary_max_tokens,
        );

        if summary_budget > 0 {
            let compressed = compress_skipped_sync(
                &skipped_items,
                compression_options,
                format_options,
                summary_budget,
                estimator,
            );

            if !compressed.text.is_empty() {
                context = if context.is_empty() {
                    compressed.text.clone()
                } else {
                    merge_context_with_summary(&context, format_options.format, &compressed)
                };
                compression = ContextCompressionUsage::from_compression(
                    ContextCompressionMode::SimpleExtractive,
                    compressed.summarized_memories,
                    compressed.estimated_tokens.min(summary_budget),
                );
            }
        }
    }

    if context.is_empty() {
        context = EMPTY_CONTEXT_MESSAGE.to_string();
    }

    let used_tokens = estimator.estimate_tokens(&context).min(available);

    AssembledContext {
        context,
        memories: included_memories,
        budget: ContextBudgetUsage {
            max_tokens: budget.max_tokens,
            reserved_tokens: budget.reserved_tokens,
            available_tokens: available,
            used_tokens,
            included_memories: included_items.len(),
            skipped_memories: skipped,
        },
        compression,
        skipped_items,
    }
}

/// Appends a provider-generated summary to an assembled context.
pub fn apply_provider_compression_summary(
    mut assembled: AssembledContext,
    compressed: CompressedContext,
    format: super::format_options::ContextFormat,
    available_tokens: usize,
    mode: ContextCompressionMode,
    estimator: &impl TokenEstimator,
) -> AssembledContext {
    if compressed.text.is_empty() {
        return assembled;
    }

    assembled.context = if assembled.context == EMPTY_CONTEXT_MESSAGE {
        compressed.text.clone()
    } else {
        merge_context_with_summary(&assembled.context, format, &compressed)
    };
    assembled.budget.used_tokens = estimator
        .estimate_tokens(&assembled.context)
        .min(available_tokens);
    assembled.compression = ContextCompressionUsage::from_compression(
        mode,
        compressed.summarized_memories,
        compressed.estimated_tokens,
    );
    assembled
}

fn compress_skipped_sync(
    skipped_items: &[ContextMemoryItem],
    compression_options: &ContextCompressionOptions,
    format_options: &ContextFormatOptions,
    summary_budget: usize,
    estimator: &impl TokenEstimator,
) -> CompressedContext {
    SimpleContextCompressor::compress(
        skipped_items,
        summary_budget,
        format_options.format,
        compression_options.include_summary_section,
        estimator,
    )
    .unwrap_or(CompressedContext {
        text: String::new(),
        summarized_memories: 0,
        estimated_tokens: 0,
        bullets: Vec::new(),
    })
}

fn estimate_context_tokens(
    items: &[ContextMemoryItem],
    format_options: &ContextFormatOptions,
    legacy_include_fact_metadata: bool,
    estimator: &impl TokenEstimator,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    ContextFormatter::format(items, format_options, legacy_include_fact_metadata)
        .map(|formatted| estimator.estimate_tokens(&formatted.context))
        .unwrap_or(0)
}

fn token_delta_for_addition(
    current: &[ContextMemoryItem],
    candidate: &ContextMemoryItem,
    format_options: &ContextFormatOptions,
    legacy_include_fact_metadata: bool,
    estimator: &impl TokenEstimator,
) -> usize {
    let before = estimate_context_tokens(
        current,
        format_options,
        legacy_include_fact_metadata,
        estimator,
    );
    let mut with_candidate = current.to_vec();
    with_candidate.push(candidate.clone());
    let after = estimate_context_tokens(
        &with_candidate,
        format_options,
        legacy_include_fact_metadata,
        estimator,
    );
    after.saturating_sub(before)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::context::budget::{ContextBudget, DEFAULT_CONTEXT_RESERVED_TOKENS};
    use crate::context::compression_options::ContextCompressionMode;
    use crate::context::format_options::{ContextFormat, ContextFormatOptions};
    use crate::{MemorySearchResult, MemoryType};

    fn sample_result(content: &str) -> MemorySearchResult {
        MemorySearchResult {
            fact_id: Uuid::new_v4(),
            content: content.to_string(),
            memory_type: MemoryType::Skill,
            score: 0.9,
            vector_score: None,
            confidence: 0.8,
            importance: 0.7,
            valid_at: None,
            metadata: json!({}),
            structure: Default::default(),
        }
    }

    #[test]
    fn empty_memories_returns_safe_message() {
        assert_eq!(assemble_context(&[], false), EMPTY_CONTEXT_MESSAGE);
    }

    #[test]
    fn compression_disabled_preserves_skip_behavior() {
        let long_content = "x".repeat(4000);
        let memories = vec![sample_result(&long_content), sample_result("tiny")];
        let budget = ContextBudget {
            max_tokens: 120,
            reserved_tokens: 20,
        };

        let assembled = assemble_context_with_budget(
            &memories,
            false,
            &ContextFormatOptions::default(),
            &budget,
            &ContextCompressionOptions::default(),
            &SimpleTokenEstimator,
        );

        assert_eq!(assembled.memories.len(), 1);
        assert_eq!(assembled.budget.skipped_memories, 1);
        assert!(!assembled.compression.enabled);
    }

    #[test]
    fn simple_extractive_adds_summary_section() {
        let memories: Vec<_> = (0..8)
            .map(|index| {
                sample_result(&format!(
                    "compression memory item {index} alpha bravo charlie delta"
                ))
            })
            .collect();
        let budget = ContextBudget {
            max_tokens: 50,
            reserved_tokens: 10,
        };
        let options = ContextCompressionOptions {
            mode: ContextCompressionMode::SimpleExtractive,
            summary_max_tokens: 40,
            include_summary_section: true,
        };

        let assembled = assemble_context_with_budget(
            &memories,
            false,
            &ContextFormatOptions {
                format: ContextFormat::Markdown,
                ..ContextFormatOptions::default()
            },
            &budget,
            &options,
            &SimpleTokenEstimator,
        );

        assert!(assembled.compression.enabled);
        assert!(assembled.compression.summarized_memories > 0);
        assert!(assembled.context.contains("Compressed Memory Summary"));
        assert!(assembled.budget.skipped_memories > 0);
    }

    #[test]
    fn compression_summary_fits_within_budget() {
        let memories: Vec<_> = (0..8)
            .map(|index| sample_result(&format!("budget compression memory {index}")))
            .collect();
        let budget = ContextBudget {
            max_tokens: 100,
            reserved_tokens: 10,
        };
        let options = ContextCompressionOptions {
            mode: ContextCompressionMode::SimpleExtractive,
            summary_max_tokens: 50,
            include_summary_section: true,
        };

        let assembled = assemble_context_with_budget(
            &memories,
            false,
            &ContextFormatOptions::default(),
            &budget,
            &options,
            &SimpleTokenEstimator,
        );

        assert!(assembled.budget.used_tokens <= assembled.budget.available_tokens);
    }

    #[test]
    fn included_memories_preserve_ranked_order() {
        let memories = vec![
            sample_result("first ranked"),
            sample_result(&"second very long memory ".repeat(200)),
            sample_result("third ranked"),
        ];
        let budget = ContextBudget {
            max_tokens: 200,
            reserved_tokens: 20,
        };

        let assembled = assemble_context_with_budget(
            &memories,
            false,
            &ContextFormatOptions::default(),
            &budget,
            &ContextCompressionOptions::default(),
            &SimpleTokenEstimator,
        );

        assert_eq!(assembled.memories[0].content, "first ranked");
        assert_eq!(assembled.memories[1].content, "third ranked");
    }

    #[test]
    fn budget_includes_memories_that_fit() {
        let memories = vec![
            sample_result("Short memory one."),
            sample_result("Short memory two."),
        ];
        let budget = ContextBudget {
            max_tokens: 500,
            reserved_tokens: DEFAULT_CONTEXT_RESERVED_TOKENS,
        };

        let assembled = assemble_context_with_budget(
            &memories,
            false,
            &ContextFormatOptions::default(),
            &budget,
            &ContextCompressionOptions::default(),
            &SimpleTokenEstimator,
        );

        assert_eq!(assembled.memories.len(), 2);
        assert_eq!(assembled.budget.included_memories, 2);
    }
}
