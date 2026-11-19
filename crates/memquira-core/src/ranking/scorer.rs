use chrono::{DateTime, Utc};

use crate::MemoryType;

use super::types::RankingConfig;

/// Rule-based ranker combining semantic and fact metadata signals.
pub struct MemoryRanker;

impl MemoryRanker {
    pub fn score(
        semantic_score: f32,
        importance: f32,
        confidence: f32,
        freshness_score: f32,
        memory_type_boost: f32,
        config: &RankingConfig,
    ) -> f32 {
        clamp01(semantic_score) * config.semantic_weight
            + clamp01(importance) * config.importance_weight
            + clamp01(confidence) * config.confidence_weight
            + clamp01(freshness_score) * config.freshness_weight
            + clamp01(memory_type_boost) * config.memory_type_weight
    }
}

/// Clamps a value into the \[0.0, 1.0\] interval.
pub fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Freshness score from fact `updated_at` (or `recorded_at` when used by callers).
///
/// Bucketed decay (deterministic):
/// - <= 7 days: 1.0
/// - <= 30 days: 0.8
/// - <= 90 days: 0.6
/// - <= 365 days: 0.4
/// - older: 0.2
pub fn freshness_score(timestamp: DateTime<Utc>, now: DateTime<Utc>) -> f32 {
    let age_days = (now - timestamp).num_days().max(0);
    if age_days <= 7 {
        1.0
    } else if age_days <= 30 {
        0.8
    } else if age_days <= 90 {
        0.6
    } else if age_days <= 365 {
        0.4
    } else {
        0.2
    }
}

/// Stable memory types receive a higher boost in ranking.
pub fn memory_type_boost(memory_type: &MemoryType) -> f32 {
    match memory_type {
        MemoryType::Profile => 1.0,
        MemoryType::Preference => 0.95,
        MemoryType::Project => 0.9,
        MemoryType::Skill => 0.85,
        MemoryType::Entity => 0.75,
        MemoryType::Task => 0.7,
        MemoryType::Conversation => 0.55,
        MemoryType::System => 0.5,
        // Added with the vocabulary. Ordered by how durable the type is: a relationship
        // outlives a goal, which outlives a dated event. Existing types keep their old values, so
        // ranking over data written by earlier versions is unchanged.
        MemoryType::Relationship => 0.9,
        MemoryType::Goal => 0.8,
        MemoryType::Knowledge => 0.8,
        MemoryType::Instruction => 0.7,
        MemoryType::Event => 0.6,
        MemoryType::Other => 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_formula_combines_weighted_components() {
        let config = RankingConfig::default();
        let score = MemoryRanker::score(0.8, 0.9, 0.7, 1.0, 0.85, &config);
        let expected = 0.8 * 0.50 + 0.9 * 0.20 + 0.7 * 0.15 + 1.0 * 0.10 + 0.85 * 0.05;
        assert!((score - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn score_clamps_out_of_range_inputs() {
        let config = RankingConfig::default();
        let score = MemoryRanker::score(1.5, -0.2, 2.0, -1.0, 5.0, &config);
        let expected = 1.0 * 0.50 + 0.0 * 0.20 + 1.0 * 0.15 + 0.0 * 0.10 + 1.0 * 0.05;
        assert!((score - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn higher_importance_improves_final_score() {
        let config = RankingConfig::default();
        let low = MemoryRanker::score(0.8, 0.4, 0.8, 1.0, 0.85, &config);
        let high = MemoryRanker::score(0.8, 0.95, 0.8, 1.0, 0.85, &config);
        assert!(high > low);
    }

    #[test]
    fn higher_confidence_improves_final_score() {
        let config = RankingConfig::default();
        let low = MemoryRanker::score(0.8, 0.8, 0.3, 1.0, 0.85, &config);
        let high = MemoryRanker::score(0.8, 0.8, 0.95, 1.0, 0.85, &config);
        assert!(high > low);
    }

    #[test]
    fn recent_fact_scores_higher_freshness_than_old_fact() {
        let now = Utc::now();
        let recent = freshness_score(now - chrono::Duration::days(3), now);
        let old = freshness_score(now - chrono::Duration::days(400), now);
        assert!(recent > old);
        assert_eq!(recent, 1.0);
        assert_eq!(old, 0.2);
    }

    #[test]
    fn memory_type_boost_returns_expected_values() {
        assert_eq!(memory_type_boost(&MemoryType::Profile), 1.0);
        assert_eq!(memory_type_boost(&MemoryType::Preference), 0.95);
        assert_eq!(memory_type_boost(&MemoryType::Conversation), 0.55);
        assert_eq!(memory_type_boost(&MemoryType::System), 0.5);
    }

    #[test]
    fn ranking_config_defaults_are_valid() {
        let config = RankingConfig::default();
        assert!(config.is_valid());
        assert!((config.semantic_weight - 0.50).abs() < f32::EPSILON);
        assert!((config.importance_weight - 0.20).abs() < f32::EPSILON);
        assert!((config.confidence_weight - 0.15).abs() < f32::EPSILON);
        assert!((config.freshness_weight - 0.10).abs() < f32::EPSILON);
        assert!((config.memory_type_weight - 0.05).abs() < f32::EPSILON);
    }
}
