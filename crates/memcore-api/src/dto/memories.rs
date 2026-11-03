use memcore_common::MemcoreError;
use memcore_core::{
    AddMemoryOutput, DeleteMemoryOutput, Fact, ForgetUserOutput, ListMemoriesOutput,
    MemorySearchResult, MemoryType, SearchMemoryOutput,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

pub fn default_search_limit() -> usize {
    memcore_core::DEFAULT_SEARCH_LIMIT
}

pub fn default_list_memories_limit() -> usize {
    memcore_core::DEFAULT_LIST_MEMORIES_LIMIT
}

/// When a fact happened: the span it covers and the message time it was resolved
/// against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventTimeResponse {
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
    /// `day`, `week`, `month`, `year` or `approximate`.
    pub granularity: String,
    pub anchor: chrono::DateTime<chrono::Utc>,
}

impl From<&memcore_core::TemporalValue> for EventTimeResponse {
    fn from(value: &memcore_core::TemporalValue) -> Self {
        Self {
            start: value.start,
            end: value.end,
            granularity: value.granularity.as_str().to_string(),
            anchor: value.anchor,
        }
    }
}

impl TryFrom<&EventTimeResponse> for memcore_core::TemporalValue {
    type Error = MemcoreError;

    fn try_from(value: &EventTimeResponse) -> Result<Self, Self::Error> {
        memcore_core::TemporalValue::new(
            value.start,
            value.end,
            value.granularity.parse()?,
            value.anchor,
        )
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AddMemoryRequest {
    pub user_id: String,
    pub messages: Vec<MemoryMessageRequest>,
    #[serde(default)]
    pub metadata: Value,
    /// Optional client ID for the conversation session. Stored on every fact extracted from the
    /// request. It also scopes `message_id`s: retries must send the same `session_id`. Omitting it
    /// places the request's message IDs in the one session-less scope of the organization and user.
    /// Empty or whitespace-only values and values over 200 characters are rejected with 400.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional time of the session: RFC 3339, or a date memcore can read (e.g. `2023-05-08`,
    /// `8 May 2023`, `4:15 pm on 12 March, 2024`). Used for messages without their own `timestamp`.
    /// An unreadable value is rejected with 400. With neither a session nor a message time, relative
    /// dates ("last year") get no event time: the time memcore received the request is never used
    /// as an anchor. Explicit dates ("March 2024") are kept either way.
    #[serde(default)]
    pub session_timestamp: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MemoryMessageRequest {
    pub role: String,
    pub content: String,
    /// Optional display name of the speaker, so a fact can be attributed to a person.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional time the message was sent, in the same formats as `session_timestamp`. Relative
    /// expressions in the message ("yesterday") are resolved against it (else against
    /// `session_timestamp`; never against the receipt time). An unreadable value is rejected with
    /// 400.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Optional client ID of this message occurrence, returned in a fact's `source_message_ids`.
    /// Stable IDs are what make a retry safe: generate the ID once, when the message is created, and
    /// send the same ID when retrying. An ID must be unique for its organization, `user_id` and
    /// `session_id` across all requests, not only within one request; messages without a
    /// `session_id` share one scope per organization and user. Two identical messages that were both
    /// really sent need different IDs. Re-sending an ID with the same message (same role, `name`,
    /// content and time) is a retry: the stored message is reused, not stored again. Reusing an ID
    /// for a different message is rejected with 409 `MESSAGE_ID_CONFLICT`, and nothing from that
    /// request is stored. Duplicate IDs within one request, empty or whitespace-only IDs and IDs over
    /// 200 characters are rejected with 400. Without IDs nothing is deduplicated. A request that
    /// timed out may have been partly stored; `Retry-After` is only a timing hint. Concurrent
    /// identical requests are not idempotent at the fact level: retry sequentially. IDs are checked
    /// only when episode storage is on (`MEMCORE_EPISODES_ENABLED`, on in every shipped profile).
    #[serde(default)]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AddMemoryResponse {
    pub status: &'static str,
    pub summary: MemoryOperationSummaryResponse,
    pub memories: Vec<MemoryItemResponse>,
    /// Entity processing for this request (`MEMCORE_ENTITIES_ENABLED`); omitted when entities are
    /// disabled, so responses are otherwise unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entities: Option<super::entities::EntityProcessingSummaryResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryOperationSummaryResponse {
    /// Candidate facts extraction produced for this request.
    pub extracted: usize,
    /// Candidates that ended up stored (added + updated).
    pub stored: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Stored memories superseded and kept as history (`MEMCORE_CONSOLIDATION_MODE=llm_v2`).
    /// Omitted when zero, so `legacy` responses are unchanged.
    #[serde(skip_serializing_if = "is_zero")]
    #[schema(nullable = false)]
    pub invalidated: usize,
    /// Total candidates not stored. Equals `discarded.total`, and is kept under
    /// the original `noop` name so existing clients keep working.
    pub noop: usize,
    pub discarded: FactDiscardCountsResponse,
    /// Facts the model returned that extraction could not use at all.
    pub skipped: CandidateSkipCountsResponse,
    /// Facts stored without an event time because `valid_at` could not be read. Counted in
    /// `added`/`updated`, not in `skipped` — the fact survived, only its date did not.
    pub event_time_unparsed: usize,
    /// How structured facts' event times were settled. All zero when structured
    /// extraction is off.
    pub temporal_resolution: TemporalResolutionResponse,
    /// Source message IDs the model cited that were not in the request; dropped, never stored.
    pub invalid_source_message_ids: usize,
}

/// Per-outcome count of how structured facts' event times were settled.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TemporalResolutionResponse {
    /// Model and deterministic resolver agreed.
    pub agree: usize,
    /// They disagreed; the resolver's answer was used.
    pub deterministic_override: usize,
    /// Only the model produced a time (expression outside the resolver's patterns).
    pub llm_only: usize,
    /// Only the resolver produced a time.
    pub deterministic_only: usize,
    /// The model's time was impossible and could not be replaced.
    pub rejected: usize,
    /// No time.
    pub none: usize,
    /// The model gave a time with no expression behind it (usually the message's own date on a
    /// timeless fact); ignored, and the fact keeps its observation time only.
    pub ungrounded: usize,
    /// The time expression cannot be dated from the conversation ("recently", "every week", "when
    /// I was 10", "for five years"); no event time is stored and the fact keeps its observation time.
    pub non_datable: usize,
    /// A relative expression ("last year", "yesterday") in a request with no client message or
    /// session timestamp: nothing to resolve it against, so no event time is stored. memcore never
    /// uses the time it received the request instead.
    pub unanchored: usize,
}

impl From<memcore_core::temporal::TemporalResolutionCounts> for TemporalResolutionResponse {
    fn from(counts: memcore_core::temporal::TemporalResolutionCounts) -> Self {
        Self {
            agree: counts.agree,
            deterministic_override: counts.deterministic_override,
            llm_only: counts.llm_only,
            deterministic_only: counts.deterministic_only,
            rejected: counts.rejected,
            none: counts.none,
            ungrounded: counts.ungrounded,
            non_datable: counts.non_datable,
            unanchored: counts.unanchored,
        }
    }
}

/// Why facts the model returned never became candidates.
///
/// These are losses inside extraction, before any memory decision. They used to be invisible:
/// a fact whose `memory_type` the parser did not recognise was dropped with no log, no metric and
/// no error, which is how memcore stored 634 memories from 5,882 conversation turns.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CandidateSkipCountsResponse {
    pub total: usize,
    /// The `memory_type` label was not in the vocabulary.
    pub unknown_memory_type: usize,
    /// The fact failed validation — empty content, or a score outside 0.0..=1.0.
    pub invalid_candidate: usize,
}

impl From<memcore_core::CandidateSkipCounts> for CandidateSkipCountsResponse {
    fn from(counts: memcore_core::CandidateSkipCounts) -> Self {
        Self {
            total: counts.total(),
            unknown_memory_type: counts.unknown_memory_type,
            invalid_candidate: counts.invalid_candidate,
        }
    }
}

/// Why extracted candidates were not stored.
///
/// `extracted` always equals
/// `added + updated + deleted + discarded.total + skipped.total`, so a rejected
/// memory is accounted for rather than silently dropped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FactDiscardCountsResponse {
    pub total: usize,
    /// Importance below `MEMCORE_MIN_IMPORTANCE`.
    pub low_importance: usize,
    /// Near-identical wording to an existing fact.
    pub duplicate_content: usize,
    /// Near-identical embedding to an existing fact.
    pub duplicate_embedding: usize,
    /// The classifier chose no-op, archive, or summarize instead of a write.
    pub no_op_decision: usize,
    /// The classifier's output was invalid; the candidate was rejected, not stored.
    pub invalid_classification: usize,
}

impl From<memcore_core::FactDiscardCounts> for FactDiscardCountsResponse {
    fn from(counts: memcore_core::FactDiscardCounts) -> Self {
        Self {
            total: counts.total(),
            low_importance: counts.low_importance,
            duplicate_content: counts.duplicate_content,
            duplicate_embedding: counts.duplicate_embedding,
            no_op_decision: counts.no_op_decision,
            invalid_classification: counts.invalid_classification,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryItemResponse {
    pub id: Uuid,
    pub content: String,
    pub memory_type: MemoryTypeResponse,
    pub confidence: f32,
    pub importance: f32,
}

/// API-facing memory type labels (PascalCase) separate from core snake_case serde.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "PascalCase")]
pub enum MemoryTypeResponse {
    Profile,
    Preference,
    Project,
    Conversation,
    Task,
    Entity,
    Skill,
    System,
    /// Purely additive: a client that only knows the eight original labels keeps
    /// receiving those labels for memories of those types.
    Goal,
    Relationship,
    Event,
    Knowledge,
    Instruction,
    Other,
}

impl From<MemoryType> for MemoryTypeResponse {
    fn from(value: MemoryType) -> Self {
        match value {
            MemoryType::Profile => Self::Profile,
            MemoryType::Preference => Self::Preference,
            MemoryType::Project => Self::Project,
            MemoryType::Conversation => Self::Conversation,
            MemoryType::Task => Self::Task,
            MemoryType::Entity => Self::Entity,
            MemoryType::Skill => Self::Skill,
            MemoryType::System => Self::System,
            MemoryType::Goal => Self::Goal,
            MemoryType::Relationship => Self::Relationship,
            MemoryType::Event => Self::Event,
            MemoryType::Knowledge => Self::Knowledge,
            MemoryType::Instruction => Self::Instruction,
            MemoryType::Other => Self::Other,
        }
    }
}

impl From<&Fact> for MemoryItemResponse {
    fn from(fact: &Fact) -> Self {
        Self {
            id: fact.id,
            content: fact.content.clone(),
            memory_type: fact.memory_type.into(),
            confidence: fact.confidence,
            importance: fact.importance,
        }
    }
}

impl From<AddMemoryOutput> for AddMemoryResponse {
    fn from(output: AddMemoryOutput) -> Self {
        Self {
            status: "success",
            summary: MemoryOperationSummaryResponse {
                extracted: output.extracted,
                stored: output.stored(),
                added: output.added,
                updated: output.updated,
                deleted: output.deleted,
                invalidated: output.invalidated,
                noop: output.noop,
                discarded: output.discarded.into(),
                skipped: output.skipped.into(),
                event_time_unparsed: output.event_time_unparsed,
                temporal_resolution: output.temporal.into(),
                invalid_source_message_ids: output.invalid_source_message_ids,
            },
            memories: output
                .memories
                .iter()
                .map(MemoryItemResponse::from)
                .collect(),
            entities: output.entities.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub struct SearchMemoryFiltersRequest {
    #[serde(default)]
    pub memory_type: Option<Vec<String>>,
}

impl SearchMemoryFiltersRequest {
    pub fn parse_memory_types(&self) -> Result<Option<Vec<MemoryType>>, MemcoreError> {
        let Some(types) = &self.memory_type else {
            return Ok(None);
        };

        if types.is_empty() {
            return Ok(None);
        }

        let parsed = types
            .iter()
            .map(|value| parse_memory_type_label(value))
            .collect::<Result<Vec<MemoryType>, _>>()?;
        Ok(Some(parsed))
    }
}

/// Which retrieval legs a search runs. Omitted means the server's `MEMCORE_SEARCH_STRATEGY`
/// (default `hybrid`, the fused search memcore has always run). `semantic` is vector only,
/// `fulltext` ranked full-text only, and `rrf` fuses the vector and full-text legs by Reciprocal
/// Rank Fusion over rank positions (`MEMCORE_RRF_K`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalModeRequest {
    #[default]
    Hybrid,
    Semantic,
    Fulltext,
    Rrf,
}

impl From<RetrievalModeRequest> for memcore_core::RetrievalMode {
    fn from(mode: RetrievalModeRequest) -> Self {
        match mode {
            RetrievalModeRequest::Hybrid => Self::Hybrid,
            RetrievalModeRequest::Semantic => Self::Semantic,
            RetrievalModeRequest::Fulltext => Self::Fulltext,
            RetrievalModeRequest::Rrf => Self::Rrf,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SearchMemoryRequest {
    pub user_id: String,
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub filters: SearchMemoryFiltersRequest,
    /// Retrieval legs to run; defaults to the server's `MEMCORE_SEARCH_STRATEGY`.
    #[serde(default)]
    pub retrieval_mode: Option<RetrievalModeRequest>,
    /// Names the user `user_id` goes by (display name, aliases). Every memory in this user's scope
    /// is from or about them, so these names are not used as full-text search terms; other names
    /// in the query are. Ignored by the vector leg and by `hybrid`.
    #[serde(default)]
    pub user_names: Vec<String>,
    /// Also return memories superseded by consolidation (`MEMCORE_CONSOLIDATION_MODE=llm_v2`) or by
    /// a semantic related-fact matcher, which are otherwise left out; each such result carries a
    /// `lifecycle` object. No effect in `legacy` mode. Default `false`.
    #[serde(default)]
    pub include_history: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchMemoryResponse {
    pub status: &'static str,
    pub results: Vec<SearchMemoryResultResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchMemoryResultResponse {
    pub fact_id: Uuid,
    pub content: String,
    pub memory_type: MemoryTypeResponse,
    /// The engine's search-strategy score, which orders the results (unchanged).
    pub score: f32,
    /// Raw vector similarity from the semantic leg (cosine similarity with Qdrant), not normalised
    /// or rescaled. Omitted when no vector leg returned this result (full-text mode, or a
    /// keyword-only hybrid hit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_score: Option<f32>,
    pub metadata: Value,
    /// Present only on a result that is no longer current (returned with
    /// `include_history`): a previous value, never the current one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<SearchResultLifecycleResponse>,
}

/// Why a search result is no longer current.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResultLifecycleResponse {
    /// `superseded` (a later memory replaced it) or `invalidated` (it stopped being true).
    pub status: &'static str,
    /// When memcore marked it superseded (bookkeeping time, not an event time).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The memory that replaced it, when one was stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
    /// When it stopped being true in the real world, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<SearchMemoryOutput> for SearchMemoryResponse {
    fn from(output: SearchMemoryOutput) -> Self {
        Self {
            status: "success",
            results: output
                .results
                .iter()
                .map(|result| {
                    let mut response = SearchMemoryResultResponse::from(result);
                    response.lifecycle = output
                        .history
                        .iter()
                        .find(|entry| entry.fact_id == result.fact_id)
                        .map(|entry| SearchResultLifecycleResponse {
                            status: entry.status(),
                            superseded_at: entry.superseded_at,
                            superseded_by: entry.superseded_by,
                            invalid_at: entry.invalid_at,
                        });
                    response
                })
                .collect(),
        }
    }
}

impl From<&MemorySearchResult> for SearchMemoryResultResponse {
    fn from(result: &MemorySearchResult) -> Self {
        Self {
            fact_id: result.fact_id,
            content: result.content.clone(),
            memory_type: result.memory_type.into(),
            score: result.score,
            vector_score: result.vector_score,
            metadata: result.metadata.clone(),
            lifecycle: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListMemoriesQuery {
    pub memory_type: Option<String>,
    pub q: Option<String>,
    #[serde(default = "default_list_memories_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
    #[serde(default)]
    pub include_deleted: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListMemoriesResponse {
    pub status: &'static str,
    pub memories: Vec<ListMemoryItemResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListMemoryItemResponse {
    pub id: Uuid,
    pub content: String,
    pub memory_type: MemoryTypeResponse,
    pub confidence: f32,
    pub importance: f32,
    pub metadata: Value,
}

impl From<ListMemoriesOutput> for ListMemoriesResponse {
    fn from(output: ListMemoriesOutput) -> Self {
        Self {
            status: "success",
            memories: output
                .memories
                .iter()
                .map(ListMemoryItemResponse::from)
                .collect(),
            next_cursor: output.next_cursor,
        }
    }
}

impl From<&Fact> for ListMemoryItemResponse {
    fn from(fact: &Fact) -> Self {
        Self {
            id: fact.id,
            content: fact.content.clone(),
            memory_type: fact.memory_type.into(),
            confidence: fact.confidence,
            importance: fact.importance,
            metadata: fact.metadata.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeleteMemoryResponse {
    pub status: &'static str,
    pub deleted: bool,
}

impl From<DeleteMemoryOutput> for DeleteMemoryResponse {
    fn from(output: DeleteMemoryOutput) -> Self {
        Self {
            status: "success",
            deleted: output.deleted,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ForgetUserResponse {
    pub status: &'static str,
    pub deleted: bool,
}

impl From<ForgetUserOutput> for ForgetUserResponse {
    fn from(output: ForgetUserOutput) -> Self {
        Self {
            status: "success",
            deleted: output.deleted,
        }
    }
}

pub fn parse_memory_type_label(label: &str) -> Result<MemoryType, MemcoreError> {
    match label.trim() {
        "Profile" => Ok(MemoryType::Profile),
        "Preference" => Ok(MemoryType::Preference),
        "Project" => Ok(MemoryType::Project),
        "Conversation" => Ok(MemoryType::Conversation),
        "Task" => Ok(MemoryType::Task),
        "Entity" => Ok(MemoryType::Entity),
        "Skill" => Ok(MemoryType::Skill),
        "System" => Ok(MemoryType::System),
        other => Err(MemcoreError::ValidationError(format!(
            "invalid memory type: {other}"
        ))),
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}
