use chrono::{DateTime, Utc};
use memcore_core::{Candidate, CandidateKind, SearchCandidatesOutput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// EXPERIMENTAL: one user's facts and episodes as one candidate list. Not a stable
/// retrieval contract.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SearchCandidatesRequest {
    pub user_id: String,
    pub query: String,
    /// Semantic fact depth; default 100 (the reference depth), capped at `MEMCORE_SEARCH_MAX_LIMIT`.
    #[serde(default)]
    pub fact_limit: Option<usize>,
    /// Episode depth; default 100 (the reference depth), capped at `MEMCORE_SEARCH_MAX_LIMIT`.
    #[serde(default)]
    pub episode_limit: Option<usize>,
    /// RFC 3339 time that relative expressions in the query ("yesterday") are resolved against, when
    /// query analysis is on. Never replaced by the server clock: without it they stay unresolved.
    #[serde(default)]
    pub reference_time: Option<String>,
    /// Return the query analysis (when query analysis is on).
    #[serde(default)]
    pub include_analysis: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchCandidatesResponse {
    pub status: &'static str,
    pub results: Vec<CandidateResponse>,
    /// The transformations applied, in order; empty in baseline mode.
    pub stages: Vec<String>,
    /// Why the reranker's order was not used, if it failed (the prior order is kept).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_fallback: Option<String>,
    /// Wall-clock time of the reranker call, when a reranker is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_latency_ms: Option<f64>,
    /// The decomposition attempt (provider, calls, accepted / rejected sub-queries, fallback), when
    /// a decomposition provider is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub decomposition: Option<Value>,
    /// The query analysis, when requested and query analysis is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub analysis: Option<Value>,
}

/// One candidate, in list order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CandidateResponse {
    /// `fact` or `episode`.
    pub kind: String,
    pub id: Uuid,
    pub content: String,
    /// Raw vector similarity to the query; not normalised or rescaled.
    pub vector_score: f32,
    /// 1-based position in the item's own search result list.
    pub rank: usize,
    /// A fact's source message IDs (empty for an episode).
    pub source_message_ids: Vec<String>,
    /// An episode's message ID.
    pub message_id: Option<String>,
    pub session_id: Option<String>,
    /// An episode's speaker.
    pub speaker: Option<String>,
    /// A fact's metadata (`null` for an episode).
    pub metadata: Value,
    /// A fact's source-supported event time; never set for an episode.
    #[schema(value_type = Option<Object>)]
    pub event_time: Option<Value>,
    /// When the fact was stated or the message sent, as the client gave it. Not event time.
    pub observed_at: Option<DateTime<Utc>>,
    /// The reranker's score, for candidates inside the rerank window when a reranker ran (the
    /// score cache). Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_score: Option<f32>,
}

impl From<&Candidate> for CandidateResponse {
    fn from(candidate: &Candidate) -> Self {
        Self {
            kind: match candidate.kind {
                CandidateKind::Fact => "fact",
                CandidateKind::Episode => "episode",
            }
            .to_string(),
            id: candidate.id,
            content: candidate.content.clone(),
            vector_score: candidate.vector_score,
            rank: candidate.rank,
            source_message_ids: candidate.source_message_ids.clone(),
            message_id: candidate.message_id.clone(),
            session_id: candidate.session_id.clone(),
            speaker: candidate.speaker.clone(),
            metadata: candidate.metadata.clone(),
            event_time: candidate
                .event_time
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok()),
            observed_at: candidate.observed_at,
            rerank_score: candidate.rerank_score,
        }
    }
}

impl SearchCandidatesResponse {
    pub fn new(output: SearchCandidatesOutput, include_analysis: bool) -> Self {
        Self {
            status: "success",
            results: output.results.iter().map(CandidateResponse::from).collect(),
            stages: output.stages,
            rerank_fallback: output.rerank_fallback,
            rerank_latency_ms: output.rerank_latency_ms,
            decomposition: output
                .decomposition
                .and_then(|audit| serde_json::to_value(audit).ok()),
            analysis: if include_analysis {
                output
                    .analysis
                    .and_then(|analysis| serde_json::to_value(analysis).ok())
            } else {
                None
            },
        }
    }
}
