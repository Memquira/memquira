use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::ports::{CandidateSkipCounts, MemoryMessage};
use crate::{Fact, MemorySearchResult, TenantContext};

/// Default minimum importance for add-memory filtering until config is wired into core.
pub const DEFAULT_MIN_IMPORTANCE: f32 = 0.55;

/// Default result limit for memory search when callers omit an explicit limit.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;

/// Maximum allowed result limit for memory search.
pub const MAX_SEARCH_LIMIT: usize = 50;

/// Highest value an operator may configure as the search limit (`MEMCORE_SEARCH_MAX_LIMIT`).
pub const MAX_CONFIGURABLE_SEARCH_LIMIT: usize = 200;

/// Default limit for listing memories when callers omit an explicit limit.
pub const DEFAULT_LIST_MEMORIES_LIMIT: usize = 20;

/// Maximum allowed limit for listing memories.
pub const MAX_LIST_MEMORIES_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddMemoryInput {
    pub tenant: TenantContext,
    pub messages: Vec<MemoryMessage>,
    pub metadata: Value,
    /// Conversation session the messages belong to. Empty for callers that do not
    /// send one.
    #[serde(default)]
    pub session: MemorySession,
}

/// The conversation session an add-memory request belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySession {
    /// The client's own session ID, stored on every fact extracted from the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// When the session took place. Used as the time of any message that has no timestamp of its
    /// own. A message with neither has no time; the time memcore received the request is never
    /// used in its place (P2-20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Why a candidate fact produced by extraction was not stored.
///
/// Every candidate ends in exactly one outcome — added, updated, deleted, or
/// discarded for one of these reasons — so the counts always reconcile against
/// the number extracted. Before this existed, all four collapsed into a single
/// `noop` count and a rejected memory was invisible: there was no way to tell a
/// duplicate from a fact the classifier declined to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactDiscardReason {
    /// Importance scored below `MEMCORE_MIN_IMPORTANCE`.
    LowImportance,
    /// Near-identical wording to an existing fact (token-overlap dedup).
    DuplicateContent,
    /// Near-identical embedding to an existing fact (vector dedup).
    DuplicateEmbedding,
    /// The classifier chose no-op, archive, or summarize rather than a write.
    NoOpDecision,
    /// The classifier's output broke its contract (`ProviderOutputInvalid`): the candidate was
    /// rejected and not stored; the rest of the request was processed.
    InvalidClassification,
}

impl FactDiscardReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowImportance => "low_importance",
            Self::DuplicateContent => "duplicate_content",
            Self::DuplicateEmbedding => "duplicate_embedding",
            Self::NoOpDecision => "no_op_decision",
            Self::InvalidClassification => "invalid_classification",
        }
    }

    pub const ALL: [Self; 5] = [
        Self::LowImportance,
        Self::DuplicateContent,
        Self::DuplicateEmbedding,
        Self::NoOpDecision,
        Self::InvalidClassification,
    ];
}

/// Per-reason counts of discarded candidate facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FactDiscardCounts {
    pub low_importance: usize,
    pub duplicate_content: usize,
    pub duplicate_embedding: usize,
    pub no_op_decision: usize,
    #[serde(default)]
    pub invalid_classification: usize,
}

impl FactDiscardCounts {
    pub fn record(&mut self, reason: FactDiscardReason) {
        match reason {
            FactDiscardReason::LowImportance => self.low_importance += 1,
            FactDiscardReason::DuplicateContent => self.duplicate_content += 1,
            FactDiscardReason::DuplicateEmbedding => self.duplicate_embedding += 1,
            FactDiscardReason::NoOpDecision => self.no_op_decision += 1,
            FactDiscardReason::InvalidClassification => self.invalid_classification += 1,
        }
    }

    pub fn get(&self, reason: FactDiscardReason) -> usize {
        match reason {
            FactDiscardReason::LowImportance => self.low_importance,
            FactDiscardReason::DuplicateContent => self.duplicate_content,
            FactDiscardReason::DuplicateEmbedding => self.duplicate_embedding,
            FactDiscardReason::NoOpDecision => self.no_op_decision,
            FactDiscardReason::InvalidClassification => self.invalid_classification,
        }
    }

    pub fn total(&self) -> usize {
        self.low_importance
            + self.duplicate_content
            + self.duplicate_embedding
            + self.no_op_decision
            + self.invalid_classification
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryOperationSummary {
    /// Facts the model returned for this request, including ones extraction could not use.
    pub extracted: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Stored facts superseded and kept as history (`invalid_at` set).
    /// Always 0 in `legacy` mode.
    #[serde(default)]
    pub invalidated: usize,
    /// Total discarded after extraction. Kept as `noop` for backward compatibility; equals
    /// `discarded.total()`.
    pub noop: usize,
    pub discarded: FactDiscardCounts,
    /// Facts lost during extraction, before any memory decision was taken.
    pub skipped: CandidateSkipCounts,
    /// Facts kept although their event time could not be read.
    pub event_time_unparsed: usize,
    /// How each structured fact's event time was settled; all zero otherwise.
    pub temporal: crate::temporal::TemporalResolutionCounts,
    /// Cited source message IDs that were not in the input and were dropped.
    pub invalid_source_message_ids: usize,
}

impl MemoryOperationSummary {
    /// Candidates that resulted in a stored fact (added or updated).
    pub fn stored(&self) -> usize {
        self.added + self.updated
    }

    pub fn discard(&mut self, reason: FactDiscardReason) {
        self.discarded.record(reason);
        self.noop += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddMemoryOutput {
    /// Facts the model returned, including ones extraction could not use.
    pub extracted: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Stored facts superseded and kept as history. Not a candidate
    /// outcome (the superseding candidate is counted in `added`), so the invariant below holds.
    #[serde(default)]
    pub invalidated: usize,
    pub noop: usize,
    /// Why candidates were not stored, once they reached the memory decision.
    pub discarded: FactDiscardCounts,
    /// Why facts were lost before they reached a memory decision, by reason. `extracted` always
    /// equals `added + updated + deleted + discarded.total() + skipped.total()`, so no fact the
    /// model produced can go unaccounted for.
    pub skipped: CandidateSkipCounts,
    /// Facts stored without an event time because `valid_at` could not be read. These are counted
    /// in `added`/`updated`, not in `skipped` — the fact survived, only its date did not.
    pub event_time_unparsed: usize,
    /// How each structured fact's event time was settled; all zero otherwise.
    #[serde(default)]
    pub temporal: crate::temporal::TemporalResolutionCounts,
    /// Cited source message IDs that were not in the input and were dropped.
    #[serde(default)]
    pub invalid_source_message_ids: usize,
    pub memories: Vec<Fact>,
    /// Entity processing for this request; `None` when entities are disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<crate::EntityProcessingSummary>,
}

impl AddMemoryOutput {
    /// Candidates that resulted in a stored fact (added or updated).
    pub fn stored(&self) -> usize {
        self.added + self.updated
    }

    /// Facts the model returned that extraction could use.
    pub fn usable(&self) -> usize {
        self.extracted - self.skipped.total()
    }
}

impl From<MemoryOperationSummary> for AddMemoryOutput {
    fn from(summary: MemoryOperationSummary) -> Self {
        Self {
            extracted: summary.extracted,
            added: summary.added,
            updated: summary.updated,
            deleted: summary.deleted,
            invalidated: summary.invalidated,
            noop: summary.noop,
            discarded: summary.discarded,
            skipped: summary.skipped,
            event_time_unparsed: summary.event_time_unparsed,
            temporal: summary.temporal,
            invalid_source_message_ids: summary.invalid_source_message_ids,
            memories: Vec::new(),
            entities: None,
        }
    }
}

/// Which retrieval legs a search uses.
///
/// `Hybrid` is the fused search memcore has always run (vector leg + substring keyword leg, RRF) and
/// stays the default, unchanged. `Semantic` and `Fulltext` run one leg alone. `Rrf` fuses the vector
/// leg with the ranked full-text leg by Reciprocal Rank Fusion over rank positions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    #[default]
    Hybrid,
    Semantic,
    Fulltext,
    Rrf,
}

/// How a search runs, beyond what it searches for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    /// Retrieval legs; `None` uses the engine's configured default.
    pub mode: Option<RetrievalMode>,
    /// Names the scoped user goes by. They are not used as full-text terms, because every fact in
    /// the scope is from or about that user. Ignored by the vector leg and by `Hybrid`.
    pub user_names: Vec<String>,
    /// Also return facts invalidated by consolidation v2 (history). Without it, an engine
    /// in `llm_v2` mode leaves them out; in `legacy` mode nothing is left out either way.
    #[serde(default)]
    pub include_history: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchMemoryInput {
    pub tenant: TenantContext,
    pub query: String,
    pub limit: usize,
    pub memory_types: Option<Vec<crate::MemoryType>>,
    pub metadata_filter: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchMemoryOutput {
    pub results: Vec<MemorySearchResult>,
    /// The results that are not current (superseded or invalidated history, returned
    /// only when a request asks for history or a mode that keeps it in), with when and by what they
    /// stopped being current. Empty when every result is current.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoricalResult>,
}

/// Why a search result is no longer current, so a reader never takes a previous
/// value for the current one. Times are memcore's bookkeeping (`superseded_at`) and the fact's own
/// real-world end (`invalid_at`) when known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoricalResult {
    pub fact_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_at: Option<DateTime<Utc>>,
}

impl HistoricalResult {
    /// `superseded` when a later fact replaced it, `invalidated` otherwise.
    pub fn status(&self) -> &'static str {
        if self.superseded_at.is_some() {
            "superseded"
        } else {
            "invalidated"
        }
    }
}

/// A semantic search over one user's episodes. `limit` follows the same rules as
/// memory search (`MEMCORE_SEARCH_MAX_LIMIT`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchEpisodesInput {
    pub tenant: TenantContext,
    pub query: String,
    pub limit: usize,
}

/// An episode with its raw vector similarity to the query (the vector store's score, cosine with
/// Qdrant; higher is better). Not normalised or rescaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeSearchResult {
    pub episode: crate::Episode,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchEpisodesOutput {
    pub results: Vec<EpisodeSearchResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListMemoriesInput {
    pub tenant: TenantContext,
    pub memory_type: Option<crate::MemoryType>,
    pub query_text: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
    pub include_deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListMemoriesOutput {
    pub memories: Vec<Fact>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteMemoryInput {
    pub tenant: TenantContext,
    pub memory_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteMemoryOutput {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportUserDataInput {
    pub tenant: TenantContext,
    pub include_events: bool,
    pub include_deleted: bool,
}

/// An opt-in complete export (memory events are always included).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompleteExportInput {
    pub tenant: TenantContext,
    pub include_deleted: bool,
    /// Keyset page size, 1..=`COMPLETE_EXPORT_MAX_PAGE_SIZE`.
    pub page_size: usize,
    /// Refuse (never truncate) when the export would hold more records than this.
    pub max_records: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgetUserInput {
    pub tenant: TenantContext,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgetUserOutput {
    pub deleted: bool,
}

/// Default limit for listing memory audit events when callers omit an explicit limit.
pub const DEFAULT_LIST_MEMORY_EVENTS_LIMIT: usize = crate::ports::DEFAULT_MEMORY_EVENT_LIST_LIMIT;

/// Maximum allowed limit for listing memory audit events via the API.
pub const MAX_LIST_MEMORY_EVENTS_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListMemoryEventsInput {
    pub tenant: TenantContext,
    pub fact_id: Option<Uuid>,
    pub operation: Option<crate::MemoryEventOperation>,
    pub created_after: Option<chrono::DateTime<chrono::Utc>>,
    pub created_before: Option<chrono::DateTime<chrono::Utc>>,
    pub query_text: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListMemoryEventsOutput {
    pub events: Vec<crate::MemoryEvent>,
    pub next_cursor: Option<String>,
}
