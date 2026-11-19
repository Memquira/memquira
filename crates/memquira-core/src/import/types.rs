use serde::{Deserialize, Serialize};

use crate::TenantContext;
use crate::export::UserMemoryExport;

/// How imported facts are merged with existing user data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    #[default]
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportUserDataInput {
    pub tenant: TenantContext,
    pub export: UserMemoryExport,
    pub mode: ImportMode,
    pub restore_events: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportValidationIssue {
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportValidationSummary {
    pub valid: bool,
    pub errors: Vec<ImportValidationIssue>,
    pub warnings: Vec<ImportValidationIssue>,
}

impl ImportValidationSummary {
    pub fn valid_empty() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn first_error_message(&self) -> Option<String> {
        self.errors.first().map(|issue| issue.message.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportUserDataOutput {
    /// True only when the archive declares a complete export and its contents match
    /// its own manifest (counts, sections, unique ids). This is a consistency check of the
    /// archive, NOT proof that it holds everything the source had: the manifest is not signed, so
    /// an archive edited together with its manifest still passes. Legacy and truncated exports
    /// are always false, whatever their row counts.
    #[serde(default)]
    pub manifest_consistent: bool,
    pub imported_facts: usize,
    pub imported_events: usize,
    pub skipped_facts: usize,
    pub replaced_existing: bool,
    pub dry_run: bool,
    pub validation: ImportValidationSummary,
    /// Episodes imported from a v2 export; `None` when the export has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_episodes: Option<usize>,
    /// Episodes in the export that were not imported because episodes are disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_episodes: Option<usize>,
    /// Entities imported from a v3 export; `None` when the export has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_entities: Option<usize>,
    /// Entities in the export that were not imported because entities are disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_entities: Option<usize>,
}
