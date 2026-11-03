use chrono::{DateTime, Utc};
use memcore_core::{Episode, SearchEpisodesOutput, message_role_to_str};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

fn default_episode_search_limit() -> usize {
    memcore_core::DEFAULT_SEARCH_LIMIT
}

/// Semantic search over one user's episodes. No full-text leg, reranking or query
/// rewriting. `limit` follows the memory search rules (`MEMCORE_SEARCH_MAX_LIMIT`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SearchEpisodesRequest {
    pub user_id: String,
    pub query: String,
    #[serde(default = "default_episode_search_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchEpisodesResponse {
    pub status: &'static str,
    pub results: Vec<EpisodeSearchResultResponse>,
}

/// One episode: a stored source message (PII-redacted when redaction is on).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EpisodeSearchResultResponse {
    pub episode_id: Uuid,
    pub message_id: Option<String>,
    pub session_id: Option<String>,
    pub speaker: Option<String>,
    /// `user`, `assistant` or `system`.
    pub role: String,
    pub content: String,
    /// The client's message or session time; never the time memcore received it.
    pub observed_at: Option<DateTime<Utc>>,
    /// Vector similarity to the query (higher is better). Episode search is semantic only, so this
    /// is the raw vector similarity, the same value as `vector_score`.
    pub score: f32,
    /// Raw vector similarity (cosine similarity with Qdrant), with the same meaning as
    /// `vector_score` on memory search results; not normalised or rescaled.
    pub vector_score: Option<f32>,
}

impl EpisodeSearchResultResponse {
    fn new(episode: &Episode, score: f32) -> Self {
        Self {
            episode_id: episode.id,
            message_id: episode.message_id.clone(),
            session_id: episode.session_id.clone(),
            speaker: episode.speaker.clone(),
            role: message_role_to_str(&episode.role).to_string(),
            content: episode.content.clone(),
            observed_at: episode.observed_at,
            score,
            vector_score: Some(score),
        }
    }
}

impl From<SearchEpisodesOutput> for SearchEpisodesResponse {
    fn from(output: SearchEpisodesOutput) -> Self {
        Self {
            status: "success",
            results: output
                .results
                .iter()
                .map(|result| EpisodeSearchResultResponse::new(&result.episode, result.score))
                .collect(),
        }
    }
}
