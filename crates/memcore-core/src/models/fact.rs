use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::memory::{FactOperation, MemorySource, MemoryType};
use super::temporal::TemporalValue;
use super::validation::{validate_non_empty, validate_unit_interval};

/// Who a fact is about, when it happened, and which messages it came from.
///
/// Every field is optional and empty by default, and none is serialized while empty, so a fact
/// written without structured extraction serializes exactly as it did before Part B. Structured
/// extraction (`MEMCORE_STRUCTURED_EXTRACTION_ENABLED`) is what fills them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FactStructure {
    /// Who or what the fact is about, by name — e.g. the speaker who said it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// When the fact happened, resolved against the time of the message it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time: Option<TemporalValue>,
    /// The expression the time was resolved from, as written ("last Saturday").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time_text: Option<String>,
    /// When the fact was stated, as the client gave it: the time of its source message, or of the
    /// session. `None` when the client sent neither; never the receipt time (`recorded_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Client IDs of the messages the fact was extracted from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_message_ids: Vec<String>,
}

impl FactStructure {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub memory_type: MemoryType,
    pub content: String,
    pub summary: Option<String>,
    pub source: MemorySource,
    pub confidence: f32,
    pub importance: f32,
    pub valid_at: Option<DateTime<Utc>>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub recorded_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: Value,
    #[serde(default, flatten)]
    pub structure: FactStructure,
    /// When memcore marked this fact superseded or invalidated. A bookkeeping time,
    /// never a real-world event time (that is `invalid_at`, when known). Omitted while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<DateTime<Utc>>,
    /// The replacement fact, when one was stored. Provenance only; activity is
    /// decided by `superseded_at` / `invalid_at`, never by this field. Omitted while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
}

impl Fact {
    /// An active fact has no real-world end (`invalid_at`) and was not superseded
    /// (`superseded_at`). Deleted facts are never returned by the stores, so `deleted_at` is not
    /// part of the model.
    pub fn is_active(&self) -> bool {
        self.invalid_at.is_none() && self.superseded_at.is_none()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        org_id: impl Into<String>,
        user_id: impl Into<String>,
        memory_type: MemoryType,
        content: impl Into<String>,
        summary: Option<String>,
        source: MemorySource,
        confidence: f32,
        importance: f32,
        valid_at: Option<DateTime<Utc>>,
        invalid_at: Option<DateTime<Utc>>,
        recorded_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        metadata: Value,
    ) -> MemcoreResult<Self> {
        let org_id = org_id.into();
        let user_id = user_id.into();
        let content = content.into();

        validate_non_empty("org_id", &org_id)?;
        validate_non_empty("user_id", &user_id)?;
        validate_non_empty("content", &content)?;
        validate_unit_interval("confidence", confidence)?;
        validate_unit_interval("importance", importance)?;

        Ok(Self {
            id,
            org_id,
            user_id,
            memory_type,
            content,
            summary,
            source,
            confidence,
            importance,
            valid_at,
            invalid_at,
            recorded_at,
            updated_at,
            metadata,
            structure: FactStructure::default(),
            superseded_at: None,
            superseded_by: None,
        })
    }

    /// Attaches the structured fields.
    pub fn with_structure(mut self, structure: FactStructure) -> Self {
        self.structure = structure;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateFact {
    pub content: String,
    pub memory_type: MemoryType,
    pub confidence: f32,
    pub importance: f32,
    pub valid_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    #[serde(default, flatten)]
    pub structure: FactStructure,
}

impl CandidateFact {
    pub fn new(
        content: impl Into<String>,
        memory_type: MemoryType,
        confidence: f32,
        importance: f32,
        valid_at: Option<DateTime<Utc>>,
        metadata: Value,
    ) -> MemcoreResult<Self> {
        let content = content.into();
        validate_non_empty("content", &content)?;
        validate_unit_interval("confidence", confidence)?;
        validate_unit_interval("importance", importance)?;

        Ok(Self {
            content,
            memory_type,
            confidence,
            importance,
            valid_at,
            metadata,
            structure: FactStructure::default(),
        })
    }

    /// Attaches the structured fields. When an event time is present it also becomes `valid_at`,
    /// which is what the rest of memcore (ranking, retention, export) already reads.
    pub fn with_structure(mut self, structure: FactStructure) -> Self {
        if let Some(event_time) = &structure.event_time {
            self.valid_at = Some(event_time.start);
        }
        self.structure = structure;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactOperationDecision {
    pub operation: FactOperation,
    pub target_fact_id: Option<Uuid>,
    pub reason: Option<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub fact_id: Uuid,
    pub content: String,
    pub memory_type: MemoryType,
    /// The engine's search-strategy score, which orders the results (in every current mode the RRF
    /// score over rank positions).
    pub score: f32,
    /// The vector store's raw similarity for this fact from the semantic leg (cosine similarity
    /// with Qdrant), unchanged: not RRF, not normalised, rescaled or calibrated. `None` when
    /// no vector leg returned the fact (full-text mode, or a keyword-only hybrid hit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_score: Option<f32>,
    pub confidence: f32,
    pub importance: f32,
    pub valid_at: Option<DateTime<Utc>>,
    pub metadata: Value,
    /// Subject, event time and provenance of the fact; empty when not extracted.
    #[serde(default, flatten)]
    pub structure: FactStructure,
}
