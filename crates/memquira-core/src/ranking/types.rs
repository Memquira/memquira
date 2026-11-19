/// Weighted ranking configuration for search and context assembly.
///
/// Not yet wired to `Settings`; defaults are used until config-driven weights land.
#[derive(Debug, Clone, PartialEq)]
pub struct RankingConfig {
    pub semantic_weight: f32,
    pub importance_weight: f32,
    pub confidence_weight: f32,
    pub freshness_weight: f32,
    pub memory_type_weight: f32,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            semantic_weight: 0.50,
            importance_weight: 0.20,
            confidence_weight: 0.15,
            freshness_weight: 0.10,
            memory_type_weight: 0.05,
        }
    }
}

impl RankingConfig {
    /// Returns true when all weights are non-negative and sum to a positive value.
    pub fn is_valid(&self) -> bool {
        let weights = [
            self.semantic_weight,
            self.importance_weight,
            self.confidence_weight,
            self.freshness_weight,
            self.memory_type_weight,
        ];
        weights.iter().all(|w| *w >= 0.0) && weights.iter().sum::<f32>() > 0.0
    }
}
