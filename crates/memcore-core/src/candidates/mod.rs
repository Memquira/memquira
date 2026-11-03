//! One user's facts and episodes as a single
//! ordered candidate list, the input every ranking component works on.
//!
//! Baseline mode reproduces the E-merge order, which previously existed only in the client-side
//! harness: the semantic fact list and the episode list, merged by raw vector similarity
//! with no epsilon, no normalisation, no RRF and no deduplication across kinds. Exact ties are broken
//! by stable fields only: content text, then provenance, then kind, then rank within the item's own
//! list. Rendering and the answer-context token budget are not part of the pipeline.
//!
//! EXPERIMENTAL: served by `POST /api/v1/memories/candidates` only when
//! `MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED` is on; not a stable retrieval contract.
//!
//! The transformations (query analysis, temporal ranking, deterministic multi-query and
//! reranking) are applied by the engine only when their flags are on; every flag defaults to off,
//! and with all of them off the list is exactly the baseline.

pub mod decomposition;
pub mod rerank;
pub mod temporal;

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{Episode, MemorySearchResult, QueryAnalysis, TemporalValue, TenantContext};

/// The the reference depths: semantic top-100 facts and top-100 episodes.
pub const DEFAULT_CANDIDATE_FACT_LIMIT: usize = 100;
pub const DEFAULT_CANDIDATE_EPISODE_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Fact,
    Episode,
}

impl CandidateKind {
    /// The kind's name. Exact ties compare these strings, so episodes sort before facts, as in the
    /// harness that defined E-merge.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Episode => "episode",
        }
    }
}

/// One fact or episode in a candidate list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub id: Uuid,
    pub content: String,
    /// Raw vector similarity to the query, unchanged.
    pub vector_score: f32,
    /// 1-based position in the item's own search result list.
    pub rank: usize,
    /// A fact's source message IDs; empty for an episode.
    pub source_message_ids: Vec<String>,
    /// An episode's message ID; `None` for a fact.
    pub message_id: Option<String>,
    pub session_id: Option<String>,
    /// An episode's speaker; `None` for a fact.
    pub speaker: Option<String>,
    /// A fact's search metadata; `Value::Null` for an episode.
    pub metadata: Value,
    /// A fact's source-supported event time. Never set for an episode: a message's time is not the
    /// time of the events it describes.
    pub event_time: Option<TemporalValue>,
    /// When the fact was stated or the message was sent, as the client gave it. Not event time.
    pub observed_at: Option<DateTime<Utc>>,
    /// Multi-query only: the query (0 = the original) whose retrieval gave this item its score.
    #[serde(default)]
    pub query_index: usize,
    /// Reranking only: the provider's score for a candidate inside the rerank window (the
    /// score cache records it). `None` without a reranker, below the window, or on a fallback.
    #[serde(default)]
    pub rerank_score: Option<f32>,
}

impl Candidate {
    /// A fact from memory search. `None` when the result has no raw vector similarity, which a
    /// semantic search always returns.
    pub fn from_fact(result: &MemorySearchResult, rank: usize) -> Option<Self> {
        Some(Self {
            kind: CandidateKind::Fact,
            id: result.fact_id,
            content: result.content.clone(),
            vector_score: result.vector_score?,
            rank,
            source_message_ids: result.structure.source_message_ids.clone(),
            message_id: None,
            session_id: result.structure.session_id.clone(),
            speaker: None,
            metadata: result.metadata.clone(),
            event_time: result.structure.event_time.clone(),
            observed_at: result.structure.observed_at,
            query_index: 0,
            rerank_score: None,
        })
    }

    pub fn from_episode(episode: &Episode, score: f32, rank: usize) -> Self {
        Self {
            kind: CandidateKind::Episode,
            id: episode.id,
            content: episode.content.clone(),
            vector_score: score,
            rank,
            source_message_ids: Vec::new(),
            message_id: episode.message_id.clone(),
            session_id: episode.session_id.clone(),
            speaker: episode.speaker.clone(),
            metadata: Value::Null,
            event_time: None,
            observed_at: episode.observed_at,
            query_index: 0,
            rerank_score: None,
        }
    }

    /// Whether this is a fact with a source-supported event time. Episodes never are.
    pub fn has_event_time(&self) -> bool {
        self.kind == CandidateKind::Fact && self.event_time.is_some()
    }

    /// A fact's source message IDs joined by commas, or an episode's message ID.
    fn provenance(&self) -> String {
        match self.kind {
            CandidateKind::Fact => self.source_message_ids.join(","),
            CandidateKind::Episode => self.message_id.clone().unwrap_or_default(),
        }
    }
}

/// The E-merge order: raw vector similarity descending, no epsilon; exact ties by content
/// text, provenance, kind name and own-list rank, then (multi-query only) the query index.
pub fn e_merge_order(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .vector_score
        .partial_cmp(&left.vector_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.content.cmp(&right.content))
        .then_with(|| left.provenance().cmp(&right.provenance()))
        .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
        .then_with(|| left.rank.cmp(&right.rank))
        .then_with(|| left.query_index.cmp(&right.query_index))
}

/// Deterministic multi-query merge. `lists[0]` is the original query's baseline
/// list, the others the sub-queries' lists in clause order. The union is deduplicated by kind and
/// id; each item keeps its maximum raw vector score (no sum, no count bonus), and on an equal maximum
/// the earliest query's entry, with that query's own-list rank. The result is in E-merge order.
pub fn merge_multi_query(lists: Vec<Vec<Candidate>>) -> Vec<Candidate> {
    let mut best: HashMap<(CandidateKind, Uuid), Candidate> = HashMap::new();
    for (query_index, list) in lists.into_iter().enumerate() {
        for mut item in list {
            item.query_index = query_index;
            match best.get(&(item.kind, item.id)) {
                Some(kept) if kept.vector_score >= item.vector_score => {}
                _ => {
                    best.insert((item.kind, item.id), item);
                }
            }
        }
    }
    let mut merged: Vec<Candidate> = best.into_values().collect();
    merged.sort_by(e_merge_order);
    merged
}

/// Multi-query mode (`MEMCORE_MULTI_QUERY`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiQueryMode {
    #[default]
    None,
    /// Clause sub-queries from the analyzer; no model call.
    Deterministic,
}

/// The transformations the engine applies to candidate lists. The default is every
/// transformation off: the baseline list, unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidatePipelineConfig {
    pub query_analysis: bool,
    pub temporal_ranking: bool,
    /// β, added to the raw score of facts whose event time overlaps the question's range.
    pub temporal_boost: f64,
    pub multi_query: MultiQueryMode,
    /// The rerank / temporal window (60).
    pub window: usize,
    /// Upper bound on one reranker call; on expiry the prior order is kept.
    pub rerank_timeout: std::time::Duration,
    /// Upper bound on the one decomposition call; on expiry the original query is used alone.
    pub decomposition_timeout: std::time::Duration,
}

pub const DEFAULT_TEMPORAL_BOOST: f64 = 0.05;
pub const DEFAULT_CANDIDATE_WINDOW: usize = 60;
pub const DEFAULT_RERANK_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_DECOMPOSITION_TIMEOUT_MS: u64 = 10_000;

impl Default for CandidatePipelineConfig {
    fn default() -> Self {
        Self {
            query_analysis: false,
            temporal_ranking: false,
            temporal_boost: DEFAULT_TEMPORAL_BOOST,
            multi_query: MultiQueryMode::None,
            window: DEFAULT_CANDIDATE_WINDOW,
            rerank_timeout: std::time::Duration::from_millis(DEFAULT_RERANK_TIMEOUT_MS),
            decomposition_timeout: std::time::Duration::from_millis(
                DEFAULT_DECOMPOSITION_TIMEOUT_MS,
            ),
        }
    }
}

/// Merges a fact list and an episode list into one E-merge-ordered list. Both lists keep their
/// items; nothing is deduplicated, rescaled or dropped.
pub fn merge_by_vector_score(facts: Vec<Candidate>, episodes: Vec<Candidate>) -> Vec<Candidate> {
    let mut merged: Vec<Candidate> = facts.into_iter().chain(episodes).collect();
    merged.sort_by(e_merge_order);
    merged
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchCandidatesInput {
    pub tenant: TenantContext,
    pub query: String,
    pub fact_limit: usize,
    pub episode_limit: usize,
    /// Resolves relative time expressions in the query; never the system clock.
    pub reference_time: Option<DateTime<FixedOffset>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchCandidatesOutput {
    pub results: Vec<Candidate>,
    /// The analyzer output, when query analysis is on.
    pub analysis: Option<QueryAnalysis>,
    /// The transformations applied, in order (empty in baseline mode).
    pub stages: Vec<String>,
    /// Why the reranker's order was not used, when it failed (the prior order is kept).
    pub rerank_fallback: Option<String>,
    /// Wall-clock time of the reranker call, when one was made.
    pub rerank_latency_ms: Option<f64>,
    /// The decomposition attempt, when a decomposition provider is configured.
    pub decomposition: Option<decomposition::DecompositionAudit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        kind: CandidateKind,
        score: f32,
        content: &str,
        provenance: &str,
        rank: usize,
    ) -> Candidate {
        Candidate {
            kind,
            id: Uuid::new_v4(),
            content: content.to_string(),
            vector_score: score,
            rank,
            source_message_ids: match kind {
                CandidateKind::Fact if !provenance.is_empty() => {
                    provenance.split(',').map(str::to_string).collect()
                }
                _ => Vec::new(),
            },
            message_id: match kind {
                CandidateKind::Episode if !provenance.is_empty() => Some(provenance.to_string()),
                _ => None,
            },
            session_id: None,
            speaker: None,
            metadata: Value::Null,
            event_time: None,
            observed_at: None,
            query_index: 0,
            rerank_score: None,
        }
    }

    fn contents(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|c| c.content.as_str()).collect()
    }

    #[test]
    fn merges_by_raw_score_descending() {
        let facts = vec![
            item(CandidateKind::Fact, 0.9, "f1", "D1:1", 1),
            item(CandidateKind::Fact, 0.5, "f2", "D1:2", 2),
        ];
        let episodes = vec![
            item(CandidateKind::Episode, 0.7, "e1", "D1:3", 1),
            item(CandidateKind::Episode, 0.1, "e2", "D1:4", 2),
        ];
        let merged = merge_by_vector_score(facts, episodes);
        assert_eq!(contents(&merged), ["f1", "e1", "f2", "e2"]);
    }

    #[test]
    fn exact_ties_use_content_then_provenance_then_kind_then_rank() {
        let merged = merge_by_vector_score(
            vec![
                item(CandidateKind::Fact, 0.5, "b", "D2:1", 1),
                item(CandidateKind::Fact, 0.5, "a", "D2:9", 2),
                item(CandidateKind::Fact, 0.5, "a", "D2:1,D2:2", 3),
                item(CandidateKind::Fact, 0.5, "same", "", 4),
                item(CandidateKind::Fact, 0.5, "same", "", 5),
            ],
            vec![item(CandidateKind::Episode, 0.5, "same", "", 1)],
        );
        let keys: Vec<(&str, String, &str, usize)> = merged
            .iter()
            .map(|c| (c.content.as_str(), c.provenance(), c.kind.as_str(), c.rank))
            .collect();
        assert_eq!(
            keys,
            [
                ("a", "D2:1,D2:2".to_string(), "fact", 3),
                ("a", "D2:9".to_string(), "fact", 2),
                ("b", "D2:1".to_string(), "fact", 1),
                ("same", String::new(), "episode", 1),
                ("same", String::new(), "fact", 4),
                ("same", String::new(), "fact", 5),
            ]
        );
    }

    #[test]
    fn content_ties_compare_by_code_point() {
        // Byte order of UTF-8 equals code point order, as in the harness.
        let merged = merge_by_vector_score(
            vec![
                item(CandidateKind::Fact, 0.3, "\u{e9}t\u{e9}", "", 1),
                item(CandidateKind::Fact, 0.3, "zeta", "", 2),
                item(CandidateKind::Fact, 0.3, "Zeta", "", 3),
            ],
            Vec::new(),
        );
        assert_eq!(contents(&merged), ["Zeta", "zeta", "\u{e9}t\u{e9}"]);
    }

    #[test]
    fn empty_inputs_merge_to_empty() {
        assert!(merge_by_vector_score(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn multi_query_keeps_the_maximum_score_and_the_earliest_query_on_ties() {
        let shared = item(CandidateKind::Fact, 0.4, "shared", "D1:1", 5);
        let tied = item(CandidateKind::Episode, 0.6, "tied", "D1:2", 2);
        let mut shared_better = shared.clone();
        shared_better.vector_score = 0.8;
        shared_better.rank = 1;
        let mut tied_later = tied.clone();
        tied_later.rank = 9;
        let only_sub = item(CandidateKind::Fact, 0.5, "only sub", "D1:3", 3);
        let merged = merge_multi_query(vec![
            vec![shared.clone(), tied.clone()],
            vec![shared_better.clone(), tied_later, only_sub.clone()],
        ]);
        assert_eq!(contents(&merged), ["shared", "tied", "only sub"]);
        // max score, from the sub-query, with the sub-query's rank
        assert_eq!(merged[0].vector_score, 0.8);
        assert_eq!((merged[0].query_index, merged[0].rank), (1, 1));
        // equal maximum: the original query's entry
        assert_eq!((merged[1].query_index, merged[1].rank), (0, 2));
        assert_eq!(merged[2].query_index, 1);
    }

    #[test]
    fn multi_query_with_only_the_original_equals_the_baseline() {
        let list = merge_by_vector_score(
            vec![
                item(CandidateKind::Fact, 0.9, "a", "D1:1", 1),
                item(CandidateKind::Fact, 0.5, "same", "", 2),
            ],
            vec![item(CandidateKind::Episode, 0.5, "same", "", 1)],
        );
        assert_eq!(merge_multi_query(vec![list.clone()]), list);
    }
}
