mod assembler;
mod budget;
mod cache;
mod cache_coordinator;
mod cache_metrics;
mod compression;
mod compression_options;
mod format_options;
mod formatter;
mod summarizer;
mod token_estimator;
mod types;

pub use assembler::{
    AssembledContext, apply_provider_compression_summary, assemble_context,
    assemble_context_with_budget,
};
pub use budget::{
    ContextBudget, ContextBudgetUsage, DEFAULT_CONTEXT_MAX_TOKENS, DEFAULT_CONTEXT_RESERVED_TOKENS,
    MAX_CONTEXT_MAX_TOKENS,
};
pub use cache::{
    CachedContextEntry, ContextCache, ContextCacheConfig, ContextCacheKey, ContextCacheUsage,
    DEFAULT_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS, DEFAULT_CONTEXT_CACHE_MAX_ENTRIES,
    DEFAULT_CONTEXT_CACHE_TTL_SECONDS, InMemoryContextCache, build_context_cache_key,
    cached_entry_from_output, cached_entry_with_ttl, stable_sha256_hex,
};
pub use cache_coordinator::{ContextCacheCoordinator, ContextCacheStampedeConfig};
pub use cache_metrics::{
    ContextCacheMetricsRecorder, ContextCacheMetricsSnapshot, InMemoryContextCacheMetrics,
    NoopContextCacheMetrics, context_cache_metrics_recorder,
};
pub use compression::{
    CompressedContext, SimpleContextCompressor, bullet_content, effective_summary_budget,
    format_summary_text, merge_context_with_summary,
};
pub use compression_options::{
    ContextCompressionMode, ContextCompressionOptions, ContextCompressionUsage,
    DEFAULT_SUMMARY_MAX_TOKENS, MAX_SUMMARY_MAX_TOKENS,
};
pub use format_options::{ContextFormat, ContextFormatOptions};
pub use formatter::{
    ContextFormatter, ContextMemoryItem, FormattedContext, memory_type_label, section_title,
};
pub use summarizer::{
    ContextSummarizer, LlmContextSummarizer, SimpleContextSummarizer, summarize_skipped_memories,
};
pub use token_estimator::{SimpleTokenEstimator, TokenEstimator};
pub use types::{
    BuildContextInput, BuildContextOutput, DEFAULT_CONTEXT_MAX_MEMORIES, EMPTY_CONTEXT_MESSAGE,
    MAX_CONTEXT_MAX_MEMORIES,
};
