use memcore_common::MemcoreError;
use memcore_core::{
    BuildContextOutput, ContextBudget, ContextBudgetUsage, ContextCacheUsage,
    ContextCompressionMode, ContextCompressionOptions, ContextCompressionUsage, ContextFormat,
    ContextFormatOptions, MemorySearchResult,
};

use super::memories::SearchMemoryFiltersRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

pub fn default_context_max_memories() -> usize {
    memcore_core::DEFAULT_CONTEXT_MAX_MEMORIES
}

pub fn default_context_max_tokens() -> usize {
    memcore_core::DEFAULT_CONTEXT_MAX_TOKENS
}

pub fn default_context_reserved_tokens() -> usize {
    memcore_core::DEFAULT_CONTEXT_RESERVED_TOKENS
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BuildContextRequest {
    pub user_id: String,
    pub query: String,
    #[serde(default = "default_context_max_memories")]
    pub max_memories: usize,
    #[serde(default = "default_context_max_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_context_reserved_tokens")]
    pub reserved_tokens: usize,
    #[serde(default)]
    pub include_metadata: bool,
    /// Also use memories superseded by consolidation (`MEMCORE_CONSOLIDATION_MODE=llm_v2`), which
    /// are otherwise left out. No effect in `legacy` mode. Default `false`.
    #[serde(default)]
    pub include_history: bool,
    /// Output format: `plain_text`, `markdown`, or `json`.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub section_by_memory_type: Option<bool>,
    #[serde(default)]
    pub include_memory_ids: Option<bool>,
    #[serde(default)]
    pub include_memory_types: Option<bool>,
    #[serde(default)]
    pub include_scores: Option<bool>,
    #[serde(default)]
    pub include_timestamps: Option<bool>,
    #[serde(default)]
    pub include_confidence: Option<bool>,
    #[serde(default)]
    pub include_importance: Option<bool>,
    /// Compression mode: `disabled`, `simple_extractive`, or `provider_summary`.
    #[serde(default)]
    pub compression_mode: Option<String>,
    #[serde(default)]
    pub summary_max_tokens: Option<usize>,
    #[serde(default)]
    pub include_summary_section: Option<bool>,
    #[serde(default)]
    pub filters: SearchMemoryFiltersRequest,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildContextResponse {
    pub status: &'static str,
    pub context: String,
    pub memories: Vec<ContextMemoryResponse>,
    pub budget: ContextBudgetResponse,
    #[serde(skip_serializing_if = "ContextCompressionResponse::is_disabled")]
    pub compression: ContextCompressionResponse,
    #[serde(skip_serializing_if = "ContextCacheResponse::is_disabled")]
    pub cache: ContextCacheResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextCompressionResponse {
    pub enabled: bool,
    pub mode: String,
    pub summarized_memories: usize,
    pub summary_tokens: usize,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextCacheResponse {
    pub enabled: bool,
    pub hit: bool,
    pub ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "is_false")]
    pub stampede_protection_enabled: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub waited_for_inflight: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub stale_while_revalidate_enabled: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub served_stale: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub refresh_started: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ContextCacheResponse {
    fn is_disabled(&self) -> bool {
        !self.enabled
    }
}

impl From<ContextCacheUsage> for ContextCacheResponse {
    fn from(usage: ContextCacheUsage) -> Self {
        Self {
            enabled: usage.enabled,
            hit: usage.hit,
            ttl_seconds: usage.ttl_seconds,
            stampede_protection_enabled: usage.stampede_protection_enabled,
            waited_for_inflight: usage.waited_for_inflight,
            stale_while_revalidate_enabled: usage.stale_while_revalidate_enabled,
            served_stale: usage.served_stale,
            refresh_started: usage.refresh_started,
        }
    }
}

impl ContextCompressionResponse {
    fn is_disabled(&self) -> bool {
        !self.enabled
    }
}

impl From<ContextCompressionUsage> for ContextCompressionResponse {
    fn from(usage: ContextCompressionUsage) -> Self {
        let mode = match usage.mode {
            ContextCompressionMode::Disabled => "disabled",
            ContextCompressionMode::SimpleExtractive => "simple_extractive",
            ContextCompressionMode::ProviderSummary => "provider_summary",
        };
        Self {
            enabled: usage.enabled,
            mode: mode.to_string(),
            summarized_memories: usage.summarized_memories,
            summary_tokens: usage.summary_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextBudgetResponse {
    pub max_tokens: usize,
    pub reserved_tokens: usize,
    pub available_tokens: usize,
    pub used_tokens: usize,
    pub included_memories: usize,
    pub skipped_memories: usize,
}

impl From<ContextBudgetUsage> for ContextBudgetResponse {
    fn from(usage: ContextBudgetUsage) -> Self {
        Self {
            max_tokens: usage.max_tokens,
            reserved_tokens: usage.reserved_tokens,
            available_tokens: usage.available_tokens,
            used_tokens: usage.used_tokens,
            included_memories: usage.included_memories,
            skipped_memories: usage.skipped_memories,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextMemoryResponse {
    pub fact_id: uuid::Uuid,
    pub content: String,
    pub memory_type: super::memories::MemoryTypeResponse,
    pub score: f32,
    pub metadata: Value,
    /// Structured fields; omitted when the memory was stored without them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_time: Option<super::memories::EventTimeResponse>,
    /// When the memory was stated, as opposed to when it happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_message_ids: Vec<String>,
}

impl From<BuildContextOutput> for BuildContextResponse {
    fn from(output: BuildContextOutput) -> Self {
        Self {
            status: "success",
            context: output.context,
            memories: output
                .memories
                .iter()
                .map(ContextMemoryResponse::from)
                .collect(),
            budget: output.budget.into(),
            compression: output.compression.into(),
            cache: output.cache.into(),
        }
    }
}

impl From<&MemorySearchResult> for ContextMemoryResponse {
    fn from(result: &MemorySearchResult) -> Self {
        Self {
            fact_id: result.fact_id,
            content: result.content.clone(),
            memory_type: result.memory_type.into(),
            score: result.score,
            metadata: result.metadata.clone(),
            subject: result.structure.subject.clone(),
            event_time: result
                .structure
                .event_time
                .as_ref()
                .map(super::memories::EventTimeResponse::from),
            observed_at: result.structure.observed_at,
            source_message_ids: result.structure.source_message_ids.clone(),
        }
    }
}

pub fn format_options_from_request(
    request: &BuildContextRequest,
) -> Result<ContextFormatOptions, MemcoreError> {
    let mut options = ContextFormatOptions::default();

    if let Some(format) = &request.format {
        options.format = ContextFormat::parse(format)?;
    }
    if let Some(section_by_memory_type) = request.section_by_memory_type {
        options.section_by_memory_type = section_by_memory_type;
    }
    if let Some(include_memory_ids) = request.include_memory_ids {
        options.include_memory_ids = include_memory_ids;
    }
    if let Some(include_memory_types) = request.include_memory_types {
        options.include_memory_types = include_memory_types;
    }
    if let Some(include_scores) = request.include_scores {
        options.include_scores = include_scores;
    }
    if let Some(include_timestamps) = request.include_timestamps {
        options.include_timestamps = include_timestamps;
    }
    if let Some(include_confidence) = request.include_confidence {
        options.include_confidence = include_confidence;
    }
    if let Some(include_importance) = request.include_importance {
        options.include_importance = include_importance;
    }

    Ok(options)
}

pub fn compression_options_from_request(
    request: &BuildContextRequest,
    available_tokens: usize,
) -> Result<ContextCompressionOptions, MemcoreError> {
    let mut options = ContextCompressionOptions::default();

    if let Some(mode) = &request.compression_mode {
        options.mode = ContextCompressionMode::parse(mode)?;
    }
    if let Some(summary_max_tokens) = request.summary_max_tokens {
        options.summary_max_tokens = summary_max_tokens;
    }
    if let Some(include_summary_section) = request.include_summary_section {
        options.include_summary_section = include_summary_section;
    }

    options.validate(available_tokens)?;

    Ok(options)
}

pub fn validate_build_context_request(request: &BuildContextRequest) -> Result<(), MemcoreError> {
    if request.user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "user_id cannot be empty".to_string(),
        ));
    }

    if request.query.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "query cannot be empty".to_string(),
        ));
    }

    if request.max_memories == 0 {
        return Err(MemcoreError::ValidationError(
            "max_memories must be greater than 0".to_string(),
        ));
    }

    if request.max_memories > memcore_core::MAX_CONTEXT_MAX_MEMORIES {
        return Err(MemcoreError::ValidationError(format!(
            "max_memories cannot exceed {}",
            memcore_core::MAX_CONTEXT_MAX_MEMORIES
        )));
    }

    ContextBudget {
        max_tokens: request.max_tokens,
        reserved_tokens: request.reserved_tokens,
    }
    .validate()?;

    format_options_from_request(request)?;

    let budget = ContextBudget {
        max_tokens: request.max_tokens,
        reserved_tokens: request.reserved_tokens,
    };
    compression_options_from_request(request, budget.available_tokens())?;

    if let Some(summary_max_tokens) = request.summary_max_tokens {
        if summary_max_tokens == 0 {
            return Err(MemcoreError::ValidationError(
                "summary_max_tokens must be greater than 0".to_string(),
            ));
        }
        if summary_max_tokens > memcore_core::MAX_SUMMARY_MAX_TOKENS {
            return Err(MemcoreError::ValidationError(format!(
                "summary_max_tokens cannot exceed {}",
                memcore_core::MAX_SUMMARY_MAX_TOKENS
            )));
        }
    }

    if let Some(mode) = &request.compression_mode {
        ContextCompressionMode::parse(mode)?;
    }

    Ok(())
}
