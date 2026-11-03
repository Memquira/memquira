use serde::{Deserialize, Serialize};

use crate::{MemorySearchResult, MemoryType, TenantContext};

use super::budget::{ContextBudget, ContextBudgetUsage};
use super::cache::ContextCacheUsage;
use super::compression_options::{ContextCompressionOptions, ContextCompressionUsage};
use super::format_options::ContextFormatOptions;

/// Default maximum memories included in assembled context when callers omit a value.
pub const DEFAULT_CONTEXT_MAX_MEMORIES: usize = 10;

/// Maximum allowed memories for context assembly.
pub const MAX_CONTEXT_MAX_MEMORIES: usize = 20;

/// Context string returned when search finds no relevant memories.
pub const EMPTY_CONTEXT_MESSAGE: &str = "No relevant long-term memories found.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildContextInput {
    pub tenant: TenantContext,
    pub query: String,
    pub max_memories: usize,
    pub memory_types: Option<Vec<MemoryType>>,
    pub include_metadata: bool,
    #[serde(default)]
    pub budget: ContextBudget,
    #[serde(default)]
    pub format_options: ContextFormatOptions,
    #[serde(default)]
    pub compression_options: ContextCompressionOptions,
    /// Also use facts invalidated by consolidation v2 (history). Only matters when the
    /// engine runs `MEMCORE_CONSOLIDATION_MODE=llm_v2`.
    #[serde(default)]
    pub include_history: bool,
}

impl Default for BuildContextInput {
    fn default() -> Self {
        Self {
            tenant: TenantContext::new("org", "user").expect("tenant"),
            query: String::new(),
            max_memories: DEFAULT_CONTEXT_MAX_MEMORIES,
            memory_types: None,
            include_metadata: false,
            budget: ContextBudget::default(),
            format_options: ContextFormatOptions::default(),
            compression_options: ContextCompressionOptions::default(),
            include_history: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildContextOutput {
    pub context: String,
    pub memories: Vec<MemorySearchResult>,
    pub budget: ContextBudgetUsage,
    pub compression: ContextCompressionUsage,
    pub cache: ContextCacheUsage,
}
