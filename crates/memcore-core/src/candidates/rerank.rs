//! Reranking abstraction.
//!
//! A [`RerankProvider`] scores (query, document) pairs; memcore never requires one. With no provider
//! (`MEMCORE_RERANK_PROVIDER=none`, the default) the candidate order is untouched. With one, only the
//! first `window` candidates (60) are reordered, purely by the provider's scores: no blend
//! with vector similarity, no RRF, no normalisation, no interpolation. Exactly equal scores keep the
//! prior (E-merge) order. Any provider failure keeps the prior order and is reported.
//!
//! The concrete providers (a local ONNX cross-encoder and Amazon Bedrock Rerank) are separate
//! implementations of this trait.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};

use super::{Candidate, CandidateKind};

#[async_trait]
pub trait RerankProvider: Send + Sync {
    /// Short provider name for reports (`local`, `bedrock`, ...).
    fn name(&self) -> &'static str;

    /// One relevance score per document, in the documents' order (higher is more relevant).
    async fn score(&self, query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>>;
}

/// The text a reranker sees for one candidate: a fact's content, or an episode's `speaker: content`
/// (the harness's memory text without its date line). The exact representation and its 512-token
/// truncation are frozen with the concrete providers, before any measured rerank run.
pub fn rerank_document(candidate: &Candidate) -> String {
    match (candidate.kind, &candidate.speaker) {
        (CandidateKind::Episode, Some(speaker)) => format!("{speaker}: {}", candidate.content),
        _ => candidate.content.clone(),
    }
}

/// Reorders the first `window` candidates by `scores` (one per windowed candidate), descending;
/// exact ties keep their prior order. Candidates below the window keep their order.
pub fn apply_rerank_scores(
    mut list: Vec<Candidate>,
    window: usize,
    scores: &[f32],
) -> MemcoreResult<Vec<Candidate>> {
    let end = window.min(list.len());
    if scores.len() != end {
        return Err(MemcoreError::ProviderError(format!(
            "reranker returned {} scores for {end} candidates",
            scores.len()
        )));
    }
    if scores.iter().any(|score| !score.is_finite()) {
        return Err(MemcoreError::ProviderError(
            "reranker returned a non-finite score".to_string(),
        ));
    }
    let rest = list.split_off(end);
    let mut scored: Vec<(f32, Candidate)> = scores
        .iter()
        .copied()
        .zip(list)
        .map(|(score, mut item)| {
            item.rerank_score = Some(score);
            (score, item)
        })
        .collect();
    scored.sort_by(|left, right| right.0.total_cmp(&left.0));
    let mut reordered: Vec<Candidate> = scored.into_iter().map(|(_, item)| item).collect();
    reordered.extend(rest);
    Ok(reordered)
}

/// Scores in submitted-document order from a provider's `(index, score)` results, which may come
/// in any order (Bedrock returns them sorted by score). The results must cover every submitted
/// index exactly once with a finite score; anything else is an error, so a partial or corrupted
/// ranking is never applied.
pub fn scores_from_indexed(submitted: usize, results: &[(i64, f32)]) -> MemcoreResult<Vec<f32>> {
    if results.len() != submitted {
        return Err(MemcoreError::ProviderError(format!(
            "reranker returned {} results for {submitted} documents",
            results.len()
        )));
    }
    let mut scores: Vec<Option<f32>> = vec![None; submitted];
    for (index, score) in results {
        let slot = usize::try_from(*index)
            .ok()
            .filter(|slot| *slot < submitted)
            .ok_or_else(|| {
                MemcoreError::ProviderError(format!("reranker returned invalid index {index}"))
            })?;
        if !score.is_finite() {
            return Err(MemcoreError::ProviderError(format!(
                "reranker returned a non-finite score for index {index}"
            )));
        }
        if scores[slot].replace(*score).is_some() {
            return Err(MemcoreError::ProviderError(format!(
                "reranker returned index {index} more than once"
            )));
        }
    }
    scores
        .into_iter()
        .enumerate()
        .map(|(index, score)| {
            score.ok_or_else(|| {
                MemcoreError::ProviderError(format!("reranker omitted index {index}"))
            })
        })
        .collect()
}

/// The result of one rerank attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankOutcome {
    pub list: Vec<Candidate>,
    /// Why the provider's order was not applied (the prior order is kept); `None` on success.
    pub fallback: Option<String>,
    /// Wall-clock time of the provider call, including a timed-out one.
    pub latency_ms: f64,
}

/// Reranks the window with `provider`, bounded by `timeout`. On any failure (error, timeout, wrong
/// count, non-finite score) the list is returned unchanged with the reason.
pub async fn rerank(
    provider: &dyn RerankProvider,
    query: &str,
    list: Vec<Candidate>,
    window: usize,
    timeout: Duration,
) -> RerankOutcome {
    let end = window.min(list.len());
    if end == 0 {
        return RerankOutcome {
            list,
            fallback: None,
            latency_ms: 0.0,
        };
    }
    let documents: Vec<String> = list[..end].iter().map(rerank_document).collect();
    let started = Instant::now();
    let scored = tokio::time::timeout(timeout, provider.score(query, &documents)).await;
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    let fail = |list: Vec<Candidate>, reason: String| RerankOutcome {
        list,
        fallback: Some(format!("{}: {reason}", provider.name())),
        latency_ms,
    };
    match scored {
        Err(_) => fail(list, format!("timed out after {} ms", timeout.as_millis())),
        Ok(Err(error)) => fail(list, error.to_string()),
        Ok(Ok(scores)) => match apply_rerank_scores(list.clone(), window, &scores) {
            Ok(reordered) => RerankOutcome {
                list: reordered,
                fallback: None,
                latency_ms,
            },
            Err(error) => fail(list, error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;

    fn item(name: &str) -> Candidate {
        Candidate {
            kind: CandidateKind::Fact,
            id: Uuid::new_v4(),
            content: name.to_string(),
            vector_score: 0.5,
            rank: 1,
            source_message_ids: Vec::new(),
            message_id: None,
            session_id: None,
            speaker: None,
            metadata: Value::Null,
            event_time: None,
            observed_at: None,
            query_index: 0,
            rerank_score: None,
        }
    }

    fn names(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|item| item.content.as_str()).collect()
    }

    struct Fixed(Vec<f32>);

    #[async_trait]
    impl RerankProvider for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        async fn score(&self, _query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
            Ok(self.0.iter().copied().take(documents.len()).collect())
        }
    }

    struct Failing;

    #[async_trait]
    impl RerankProvider for Failing {
        fn name(&self) -> &'static str {
            "failing"
        }
        async fn score(&self, _query: &str, _documents: &[String]) -> MemcoreResult<Vec<f32>> {
            Err(MemcoreError::Timeout("reranker timed out".to_string()))
        }
    }

    const LONG: Duration = Duration::from_secs(30);

    struct Slow;

    #[async_trait]
    impl RerankProvider for Slow {
        fn name(&self) -> &'static str {
            "slow"
        }
        async fn score(&self, _query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(vec![1.0; documents.len()])
        }
    }

    #[tokio::test]
    async fn pure_score_order_in_the_window_and_ties_keep_prior_order() {
        let list: Vec<Candidate> = ["a", "b", "c", "d", "e"].iter().map(|n| item(n)).collect();
        let outcome = rerank(&Fixed(vec![0.1, 0.9, 0.5, 0.9]), "q", list.clone(), 4, LONG).await;
        assert!(outcome.fallback.is_none());
        // b and d tie at 0.9: b was first. e is below the window and stays last.
        assert_eq!(names(&outcome.list), ["b", "d", "c", "a", "e"]);
        // Identity preserved: the same candidates, only reordered.
        let mut before: Vec<_> = list.iter().map(|c| c.id).collect();
        let mut after: Vec<_> = outcome.list.iter().map(|c| c.id).collect();
        before.sort();
        after.sort();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn provider_failure_keeps_the_prior_order_and_reports_it() {
        let list: Vec<Candidate> = ["a", "b", "c"].iter().map(|n| item(n)).collect();
        let outcome = rerank(&Failing, "q", list.clone(), 60, LONG).await;
        assert_eq!(outcome.list, list);
        assert!(outcome.fallback.unwrap().starts_with("failing: "));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_keeps_the_prior_order_and_reports_it() {
        let list: Vec<Candidate> = ["a", "b", "c"].iter().map(|n| item(n)).collect();
        let outcome = rerank(&Slow, "q", list.clone(), 60, Duration::from_millis(250)).await;
        assert_eq!(outcome.list, list);
        assert_eq!(
            outcome.fallback.as_deref(),
            Some("slow: timed out after 250 ms")
        );
    }

    #[tokio::test]
    async fn wrong_score_count_or_non_finite_scores_fall_back() {
        let list: Vec<Candidate> = ["a", "b", "c"].iter().map(|n| item(n)).collect();
        let outcome = rerank(&Fixed(vec![0.3, 0.2]), "q", list.clone(), 60, LONG).await;
        assert_eq!(outcome.list, list);
        assert!(outcome.fallback.is_some());
        let outcome = rerank(
            &Fixed(vec![0.3, f32::NAN, 0.1]),
            "q",
            list.clone(),
            60,
            LONG,
        )
        .await;
        assert_eq!(outcome.list, list);
        assert!(outcome.fallback.is_some());
    }

    #[test]
    fn indexed_results_map_back_to_submitted_order() {
        // Bedrock-style: sorted by score, not by index.
        let scores = scores_from_indexed(3, &[(2, 0.9), (0, 0.5), (1, 0.1)]).unwrap();
        assert_eq!(scores, [0.5, 0.1, 0.9]);
    }

    #[test]
    fn indexed_results_reject_invalid_missing_duplicate_and_non_finite() {
        for (results, why) in [
            (vec![(0, 0.5), (3, 0.1), (1, 0.2)], "out of range"),
            (vec![(0, 0.5), (-1, 0.1), (1, 0.2)], "negative"),
            (vec![(0, 0.5), (1, 0.1)], "missing"),
            (vec![(0, 0.5), (0, 0.1), (1, 0.2)], "duplicate"),
            (vec![(0, 0.5), (1, f32::INFINITY), (2, 0.2)], "non-finite"),
            (vec![(0, 0.5), (1, 0.1), (2, 0.2), (2, 0.3)], "too many"),
        ] {
            assert!(scores_from_indexed(3, &results).is_err(), "{why}");
        }
    }

    #[test]
    fn episode_documents_carry_the_speaker() {
        let mut episode = item("went sailing");
        episode.kind = CandidateKind::Episode;
        episode.speaker = Some("Pellam".to_string());
        assert_eq!(rerank_document(&episode), "Pellam: went sailing");
        assert_eq!(rerank_document(&item("a fact")), "a fact");
    }
}
