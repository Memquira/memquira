use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::MemcoreResult;

use crate::ports::{LlmProvider, SummarizationInput};
use crate::{Fact, MemorySource, TenantContext};

use super::compression::{CompressedContext, SimpleContextCompressor, wrap_provider_summary};
use super::compression_options::ContextCompressionMode;
use super::format_options::ContextFormat;
use super::formatter::ContextMemoryItem;
use super::token_estimator::SimpleTokenEstimator;

/// Summarizes skipped context memories for compression.
#[async_trait]
pub trait ContextSummarizer: Send + Sync {
    async fn summarize_memories(
        &self,
        tenant: &TenantContext,
        memories: &[ContextMemoryItem],
        max_tokens: usize,
        format: ContextFormat,
        include_summary_section: bool,
    ) -> MemcoreResult<CompressedContext>;
}

/// Wraps `LlmProvider::summarize_memory` for context compression.
pub struct LlmContextSummarizer {
    llm: Arc<dyn LlmProvider>,
}

impl LlmContextSummarizer {
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl ContextSummarizer for LlmContextSummarizer {
    async fn summarize_memories(
        &self,
        tenant: &TenantContext,
        memories: &[ContextMemoryItem],
        max_tokens: usize,
        format: ContextFormat,
        include_summary_section: bool,
    ) -> MemcoreResult<CompressedContext> {
        let facts = memories
            .iter()
            .map(context_item_to_fact)
            .collect::<Result<Vec<_>, _>>()?;

        let summary = self
            .llm
            .summarize_memory(SummarizationInput {
                tenant: tenant.clone(),
                facts,
                max_tokens: Some(max_tokens),
            })
            .await?;

        Ok(wrap_provider_summary(
            &summary,
            memories.len(),
            format,
            include_summary_section,
            &SimpleTokenEstimator,
        ))
    }
}

/// Deterministic summarizer for `simple_extractive` compression.
pub struct SimpleContextSummarizer;

#[async_trait]
impl ContextSummarizer for SimpleContextSummarizer {
    async fn summarize_memories(
        &self,
        _tenant: &TenantContext,
        memories: &[ContextMemoryItem],
        max_tokens: usize,
        format: ContextFormat,
        include_summary_section: bool,
    ) -> MemcoreResult<CompressedContext> {
        SimpleContextCompressor::compress(
            memories,
            max_tokens,
            format,
            include_summary_section,
            &SimpleTokenEstimator,
        )
    }
}

/// Summarizes skipped memories using the configured mode; falls back to simple extractive on provider errors.
pub async fn summarize_skipped_memories(
    mode: ContextCompressionMode,
    llm: Arc<dyn LlmProvider>,
    tenant: &TenantContext,
    memories: &[ContextMemoryItem],
    max_tokens: usize,
    format: ContextFormat,
    include_summary_section: bool,
) -> (CompressedContext, ContextCompressionMode) {
    if memories.is_empty() {
        return (
            CompressedContext {
                text: String::new(),
                summarized_memories: 0,
                estimated_tokens: 0,
                bullets: Vec::new(),
            },
            ContextCompressionMode::Disabled,
        );
    }

    if matches!(mode, ContextCompressionMode::ProviderSummary) {
        let summarizer = LlmContextSummarizer::new(llm);
        if let Ok(compressed) = summarizer
            .summarize_memories(
                tenant,
                memories,
                max_tokens,
                format,
                include_summary_section,
            )
            .await
        {
            return (compressed, ContextCompressionMode::ProviderSummary);
        }
    }

    let compressed = SimpleContextCompressor::compress(
        memories,
        max_tokens,
        format,
        include_summary_section,
        &SimpleTokenEstimator,
    )
    .unwrap_or(CompressedContext {
        text: String::new(),
        summarized_memories: 0,
        estimated_tokens: 0,
        bullets: Vec::new(),
    });

    (
        compressed,
        if matches!(mode, ContextCompressionMode::ProviderSummary) {
            ContextCompressionMode::SimpleExtractive
        } else {
            mode
        },
    )
}

fn context_item_to_fact(item: &ContextMemoryItem) -> MemcoreResult<Fact> {
    let now = Utc::now();
    Fact::new(
        item.fact_id,
        "compression",
        "compression",
        item.memory_type,
        item.content.clone(),
        None,
        MemorySource::System,
        item.confidence,
        item.importance,
        item.valid_at,
        None,
        now,
        now,
        serde_json::json!({}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryType;
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
            valid_at: None,
            metadata: json!({}),
            structure: Default::default(),
        }
    }

    #[tokio::test]
    async fn simple_summarizer_returns_compressed_context() {
        let summarizer = SimpleContextSummarizer;
        let tenant = TenantContext::new("org", "user").unwrap();
        let compressed = summarizer
            .summarize_memories(
                &tenant,
                &[sample_item("User is building memcore.")],
                200,
                ContextFormat::PlainText,
                true,
            )
            .await
            .unwrap();
        assert_eq!(compressed.summarized_memories, 1);
        assert!(compressed.text.contains("User is building memcore."));
    }
}
