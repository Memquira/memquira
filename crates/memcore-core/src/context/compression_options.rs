use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};

/// How skipped memories are compressed into context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompressionMode {
    #[default]
    Disabled,
    SimpleExtractive,
    ProviderSummary,
}

impl ContextCompressionMode {
    pub fn parse(value: &str) -> MemcoreResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "simple_extractive" => Ok(Self::SimpleExtractive),
            "provider_summary" => Ok(Self::ProviderSummary),
            other => Err(MemcoreError::ValidationError(format!(
                "invalid compression_mode: {other}"
            ))),
        }
    }

    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// Default maximum tokens for compressed summary sections.
pub const DEFAULT_SUMMARY_MAX_TOKENS: usize = 300;

/// Hard upper bound for `summary_max_tokens`.
pub const MAX_SUMMARY_MAX_TOKENS: usize = 2000;

/// Compression configuration for context assembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompressionOptions {
    pub mode: ContextCompressionMode,
    pub summary_max_tokens: usize,
    pub include_summary_section: bool,
}

impl Default for ContextCompressionOptions {
    fn default() -> Self {
        Self {
            mode: ContextCompressionMode::Disabled,
            summary_max_tokens: DEFAULT_SUMMARY_MAX_TOKENS,
            include_summary_section: true,
        }
    }
}

impl ContextCompressionOptions {
    pub fn validate(&self, available_tokens: usize) -> MemcoreResult<()> {
        if self.summary_max_tokens == 0 {
            return Err(MemcoreError::ValidationError(
                "summary_max_tokens must be greater than 0".to_string(),
            ));
        }

        if self.summary_max_tokens > MAX_SUMMARY_MAX_TOKENS {
            return Err(MemcoreError::ValidationError(format!(
                "summary_max_tokens cannot exceed {MAX_SUMMARY_MAX_TOKENS}"
            )));
        }

        if self.mode.is_enabled() && self.summary_max_tokens > available_tokens {
            return Err(MemcoreError::ValidationError(
                "summary_max_tokens cannot exceed available context tokens".to_string(),
            ));
        }

        Ok(())
    }
}

/// Compression metadata returned with assembled context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompressionUsage {
    pub enabled: bool,
    pub mode: ContextCompressionMode,
    pub summarized_memories: usize,
    pub summary_tokens: usize,
}

impl ContextCompressionUsage {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            mode: ContextCompressionMode::Disabled,
            summarized_memories: 0,
            summary_tokens: 0,
        }
    }

    pub fn from_compression(
        mode: ContextCompressionMode,
        summarized_memories: usize,
        summary_tokens: usize,
    ) -> Self {
        Self {
            enabled: true,
            mode,
            summarized_memories,
            summary_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_compression_is_disabled() {
        let options = ContextCompressionOptions::default();
        assert_eq!(options.mode, ContextCompressionMode::Disabled);
        assert_eq!(options.summary_max_tokens, 300);
        assert!(options.include_summary_section);
    }

    #[test]
    fn parse_compression_mode_values() {
        assert_eq!(
            ContextCompressionMode::parse("simple_extractive").unwrap(),
            ContextCompressionMode::SimpleExtractive
        );
    }

    #[test]
    fn invalid_summary_max_tokens_is_rejected() {
        let options = ContextCompressionOptions {
            mode: ContextCompressionMode::SimpleExtractive,
            summary_max_tokens: 0,
            include_summary_section: true,
        };
        assert!(options.validate(1000).is_err());
    }
}
