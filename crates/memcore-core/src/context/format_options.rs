use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};

/// Output encoding for assembled context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextFormat {
    #[default]
    PlainText,
    Markdown,
    Json,
}

impl ContextFormat {
    pub fn parse(value: &str) -> MemcoreResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "plain_text" | "plaintext" => Ok(Self::PlainText),
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            other => Err(MemcoreError::ValidationError(format!(
                "invalid context format: {other}"
            ))),
        }
    }
}

/// Controls section grouping and opt-in metadata in formatted context output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFormatOptions {
    pub format: ContextFormat,
    pub section_by_memory_type: bool,
    pub include_memory_ids: bool,
    pub include_memory_types: bool,
    pub include_scores: bool,
    pub include_timestamps: bool,
    pub include_confidence: bool,
    pub include_importance: bool,
}

impl Default for ContextFormatOptions {
    /// Backward-compatible defaults matching pre-formatting context assembly.
    fn default() -> Self {
        Self {
            format: ContextFormat::PlainText,
            section_by_memory_type: false,
            include_memory_ids: false,
            include_memory_types: false,
            include_scores: false,
            include_timestamps: false,
            include_confidence: false,
            include_importance: false,
        }
    }
}

impl ContextFormatOptions {
    /// Recommended settings when clients explicitly opt into structured formatting.
    pub fn structured_markdown() -> Self {
        Self {
            format: ContextFormat::Markdown,
            section_by_memory_type: true,
            include_memory_ids: false,
            include_memory_types: true,
            include_scores: false,
            include_timestamps: false,
            include_confidence: false,
            include_importance: false,
        }
    }

    pub fn is_legacy_plain(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_backward_compatible() {
        let opts = ContextFormatOptions::default();
        assert_eq!(opts.format, ContextFormat::PlainText);
        assert!(!opts.section_by_memory_type);
        assert!(!opts.include_memory_ids);
    }

    #[test]
    fn parse_format_accepts_snake_case_values() {
        assert_eq!(
            ContextFormat::parse("markdown").unwrap(),
            ContextFormat::Markdown
        );
        assert_eq!(
            ContextFormat::parse("plain_text").unwrap(),
            ContextFormat::PlainText
        );
        assert_eq!(ContextFormat::parse("json").unwrap(), ContextFormat::Json);
    }

    #[test]
    fn parse_format_rejects_unknown_value() {
        assert!(ContextFormat::parse("yaml").is_err());
    }
}
