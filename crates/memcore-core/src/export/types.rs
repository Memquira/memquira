use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Entity, EntityRelation, Episode, Fact, FactEntityLink, MemoryEvent};

/// JSON export format version for per-user memory exports.
pub const USER_EXPORT_FORMAT_VERSION: &str = "memcore.user_export.v1";

/// Export format version when the export carries episodes. Import accepts both.
pub const USER_EXPORT_FORMAT_VERSION_V2: &str = "memcore.user_export.v2";

/// Export format version when the export carries entities; it may also carry
/// episodes. Import accepts v1, v2 and v3.
pub const USER_EXPORT_FORMAT_VERSION_V3: &str = "memcore.user_export.v3";

/// A user's entity layer in an export: entities (with name embeddings and their provenance), fact
/// links and relations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityExport {
    pub entities: Vec<Entity>,
    pub links: Vec<FactEntityLink>,
    pub relations: Vec<EntityRelation>,
}

/// Maximum episodes included in a single user export.
pub const EXPORT_EPISODES_LIMIT: usize = 10_000;

/// Maximum facts included in a single user export (foundation phase).
pub const EXPORT_FACTS_LIMIT: usize = crate::MAX_LIST_MEMORIES_LIMIT;

/// Maximum memory events included in a single user export (foundation phase).
pub const EXPORT_EVENTS_LIMIT: usize = crate::MAX_LIST_MEMORY_EVENTS_LIMIT;

/// A section of a user export that stopped at its documented per-export limit: more records
/// exist than were exported. Listed only on incomplete exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSectionTruncation {
    /// `facts`, `memory_events` or `episodes`.
    pub section: String,
    /// The section's per-export limit; exactly this many records were exported.
    pub limit: usize,
}

/// The `completeness` manifest's mode value.
pub const EXPORT_COMPLETE_MODE: &str = "complete";
/// Default and maximum page size of a complete export's paginated reads.
pub const COMPLETE_EXPORT_DEFAULT_PAGE_SIZE: usize = 500;
pub const COMPLETE_EXPORT_MAX_PAGE_SIZE: usize = 1_000;
/// Default bound on the records a complete export holds in memory
/// (`MEMCORE_EXPORT_MAX_RECORDS`); a larger export is refused, never returned partially.
pub const COMPLETE_EXPORT_DEFAULT_MAX_RECORDS: usize = 100_000;
/// Consistency attempts (each = two identical full paginated reads) before refusing.
pub const COMPLETE_EXPORT_ATTEMPTS: usize = 3;

/// Exact per-section record counts of a complete export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSectionCounts {
    pub facts: usize,
    pub memory_events: usize,
    pub episodes: usize,
    pub entities: usize,
    pub entity_links: usize,
    pub entity_relations: usize,
}

/// Present only on an opt-in COMPLETE export. Every section was read to its end with
/// bounded keyset pages, twice, and both reads were identical; `counts` are the exact section sizes
/// and an import verifies them before any write. Not a cross-store snapshot: Qdrant vectors are not
/// part of the export (they are re-embedded on import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCompleteness {
    /// Always `complete`.
    pub mode: String,
    pub include_deleted: bool,
    pub page_size: usize,
    /// Identical full reads required (2).
    pub verified_reads: usize,
    pub counts: ExportSectionCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMemoryExport {
    pub org_id: String,
    pub user_id: String,
    pub exported_at: DateTime<Utc>,
    pub format_version: String,
    pub facts: Vec<Fact>,
    pub memory_events: Vec<MemoryEvent>,
    /// Present only in v2 exports, written when episodes are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes: Option<Vec<Episode>>,
    /// Present only in v3 exports, written when entities are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<EntityExport>,
    /// Present only when the export is INCOMPLETE: each listed section hit its documented
    /// per-export limit and later records were not exported. Complete exports omit the field,
    /// so their format is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Vec<ExportSectionTruncation>>,
    /// Present only on an opt-in complete export (see [`ExportCompleteness`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<ExportCompleteness>,
}

impl UserMemoryExport {
    pub fn new(
        org_id: impl Into<String>,
        user_id: impl Into<String>,
        facts: Vec<Fact>,
        memory_events: Vec<MemoryEvent>,
    ) -> Self {
        Self {
            org_id: org_id.into(),
            user_id: user_id.into(),
            exported_at: Utc::now(),
            format_version: USER_EXPORT_FORMAT_VERSION.to_string(),
            facts,
            memory_events,
            episodes: None,
            entities: None,
            truncated: None,
            completeness: None,
        }
    }

    /// Adds the user's episodes, which makes this a v2 export.
    pub fn with_episodes(mut self, episodes: Vec<Episode>) -> Self {
        self.format_version = USER_EXPORT_FORMAT_VERSION_V2.to_string();
        self.episodes = Some(episodes);
        self
    }

    /// Marks the export incomplete for the given sections; no-op when none were truncated.
    pub fn with_truncated(mut self, sections: Vec<ExportSectionTruncation>) -> Self {
        self.truncated = (!sections.is_empty()).then_some(sections);
        self
    }

    /// Marks the export complete.
    pub fn with_completeness(mut self, completeness: ExportCompleteness) -> Self {
        self.completeness = Some(completeness);
        self
    }

    /// Adds the user's entity layer, which makes this a v3 export.
    pub fn with_entities(mut self, entities: EntityExport) -> Self {
        self.format_version = USER_EXPORT_FORMAT_VERSION_V3.to_string();
        self.entities = Some(entities);
        self
    }
}
