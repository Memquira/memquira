use std::cmp::Ordering;
use std::collections::HashMap;

use uuid::Uuid;

/// Configuration for Reciprocal Rank Fusion across retrieval sources.
///
/// Not yet wired to `Settings`; defaults are used until config-driven weights land.
#[derive(Debug, Clone, PartialEq)]
pub struct RrfConfig {
    pub k: f32,
    pub semantic_weight: f32,
    pub keyword_weight: f32,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            semantic_weight: 1.0,
            keyword_weight: 1.0,
        }
    }
}

impl RrfConfig {
    /// Returns a sanitized copy with safe defaults for invalid values.
    pub fn normalized(&self) -> Self {
        Self {
            k: if self.k.is_finite() && self.k > 0.0 {
                self.k
            } else {
                60.0
            },
            semantic_weight: if self.semantic_weight.is_finite() && self.semantic_weight >= 0.0 {
                self.semantic_weight
            } else {
                1.0
            },
            keyword_weight: if self.keyword_weight.is_finite() && self.keyword_weight >= 0.0 {
                self.keyword_weight
            } else {
                1.0
            },
        }
    }
}

/// Which retrieval source produced a ranked candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankingSource {
    Semantic,
    Keyword,
}

/// A single ranked hit from semantic or keyword retrieval before fusion.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedCandidate {
    pub fact_id: Uuid,
    pub source: RankingSource,
    /// 1-based rank within the source list.
    pub rank: usize,
    /// Source-specific score (semantic similarity or unused for keyword); not used in RRF formula.
    pub score: f32,
}

/// Fuses semantic and keyword ranked lists using weighted Reciprocal Rank Fusion.
///
/// Formula per candidate: `rrf_score += source_weight * (1.0 / (k + rank))` with 1-based rank.
/// Facts appearing in both lists accumulate both contributions. Output is sorted by RRF score
/// descending; ties break on `fact_id` ascending for deterministic ordering.
pub fn reciprocal_rank_fusion(
    semantic_results: &[RankedCandidate],
    keyword_results: &[RankedCandidate],
    config: &RrfConfig,
) -> Vec<(Uuid, f32)> {
    let config = config.normalized();
    let mut scores: HashMap<Uuid, f32> = HashMap::new();

    let mut apply_source = |candidates: &[RankedCandidate], weight: f32| {
        let mut seen = HashMap::new();
        for candidate in candidates {
            if seen.insert(candidate.fact_id, ()).is_some() {
                continue;
            }
            let rank = candidate.rank.max(1) as f32;
            let contribution = weight * (1.0 / (config.k + rank));
            *scores.entry(candidate.fact_id).or_insert(0.0) += contribution;
        }
    };

    apply_source(semantic_results, config.semantic_weight);
    apply_source(keyword_results, config.keyword_weight);

    let mut fused: Vec<(Uuid, f32)> = scores.into_iter().collect();
    fused.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(fact_id: Uuid, source: RankingSource, rank: usize) -> RankedCandidate {
        RankedCandidate {
            fact_id,
            source,
            rank,
            score: 0.0,
        }
    }

    #[test]
    fn semantic_only_results_return_expected_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let semantic = vec![
            candidate(a, RankingSource::Semantic, 1),
            candidate(b, RankingSource::Semantic, 2),
        ];

        let fused = reciprocal_rank_fusion(&semantic, &[], &RrfConfig::default());
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].0, a);
        assert_eq!(fused[1].0, b);
        assert!(fused[0].1 > fused[1].1);
    }

    #[test]
    fn keyword_only_results_return_expected_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let keyword = vec![
            candidate(a, RankingSource::Keyword, 1),
            candidate(b, RankingSource::Keyword, 2),
        ];

        let fused = reciprocal_rank_fusion(&[], &keyword, &RrfConfig::default());
        assert_eq!(fused[0].0, a);
        assert!(fused[0].1 > fused[1].1);
    }

    #[test]
    fn overlapping_facts_receive_combined_score() {
        let shared = Uuid::new_v4();
        let keyword_only = Uuid::new_v4();
        let semantic = vec![candidate(shared, RankingSource::Semantic, 1)];
        let keyword = vec![
            candidate(shared, RankingSource::Keyword, 1),
            candidate(keyword_only, RankingSource::Keyword, 2),
        ];

        let fused = reciprocal_rank_fusion(&semantic, &keyword, &RrfConfig::default());
        let shared_score = fused.iter().find(|(id, _)| *id == shared).unwrap().1;
        let keyword_only_score = fused.iter().find(|(id, _)| *id == keyword_only).unwrap().1;
        assert!(shared_score > keyword_only_score);
    }

    #[test]
    fn rank_is_one_based() {
        let fact_id = Uuid::new_v4();
        let config = RrfConfig::default();
        let rank_one = reciprocal_rank_fusion(
            &[candidate(fact_id, RankingSource::Semantic, 1)],
            &[],
            &config,
        )[0]
        .1;
        let rank_two = reciprocal_rank_fusion(
            &[candidate(fact_id, RankingSource::Semantic, 2)],
            &[],
            &config,
        )[0]
        .1;
        assert!(rank_one > rank_two);
    }

    #[test]
    fn lower_rank_receives_higher_contribution() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let fused = reciprocal_rank_fusion(
            &[
                candidate(a, RankingSource::Semantic, 1),
                candidate(b, RankingSource::Semantic, 3),
            ],
            &[],
            &RrfConfig::default(),
        );
        assert_eq!(fused[0].0, a);
    }

    #[test]
    fn tie_break_is_deterministic_by_fact_id() {
        let low_id = Uuid::from_u128(1);
        let high_id = Uuid::from_u128(2);
        let config = RrfConfig {
            k: 60.0,
            semantic_weight: 1.0,
            keyword_weight: 1.0,
        };
        let fused = reciprocal_rank_fusion(
            &[
                candidate(high_id, RankingSource::Semantic, 1),
                candidate(low_id, RankingSource::Keyword, 1),
            ],
            &[],
            &config,
        );
        assert_eq!(fused.len(), 2);
        assert!(fused[0].1 >= fused[1].1);
        if (fused[0].1 - fused[1].1).abs() < f32::EPSILON {
            assert!(fused[0].0 < fused[1].0);
        }
    }

    #[test]
    fn custom_source_weights_affect_score() {
        let fact_id = Uuid::new_v4();
        let heavy_semantic = reciprocal_rank_fusion(
            &[candidate(fact_id, RankingSource::Semantic, 1)],
            &[],
            &RrfConfig {
                k: 60.0,
                semantic_weight: 2.0,
                keyword_weight: 1.0,
            },
        )[0]
        .1;
        let default_semantic = reciprocal_rank_fusion(
            &[candidate(fact_id, RankingSource::Semantic, 1)],
            &[],
            &RrfConfig::default(),
        )[0]
        .1;
        assert!(heavy_semantic > default_semantic);
    }

    #[test]
    fn invalid_zero_config_is_handled_safely() {
        let fact_id = Uuid::new_v4();
        let config = RrfConfig {
            k: 0.0,
            semantic_weight: -1.0,
            keyword_weight: f32::NAN,
        };
        let fused = reciprocal_rank_fusion(
            &[candidate(fact_id, RankingSource::Semantic, 1)],
            &[],
            &config,
        );
        assert_eq!(fused.len(), 1);
        assert!(fused[0].1.is_finite());
        assert!(fused[0].1 > 0.0);
    }
}
