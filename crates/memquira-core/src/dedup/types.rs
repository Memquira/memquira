use uuid::Uuid;

/// Normalized content match (threshold 1.0).
pub const EXACT_DUPLICATE_THRESHOLD: f32 = 1.0;

/// Token-overlap ratio at or above which a candidate is treated as a duplicate.
pub const HIGH_SIMILARITY_DUPLICATE_THRESHOLD: f32 = 0.90;

/// Token-overlap ratio at or above which classification may decide Update vs Add.
pub const MODERATE_SIMILARITY_THRESHOLD: f32 = 0.65;

/// Maximum active facts scanned per deduplication check (same tenant + memory type).
pub const DEDUP_SEARCH_LIMIT: usize = 50;

/// Default cosine-similarity threshold for embedding-based duplicate detection.
pub const DEFAULT_EMBEDDING_DEDUP_SIMILARITY_THRESHOLD: f32 = 0.92;

/// Default vector search limit for embedding-based duplicate detection.
pub const DEFAULT_EMBEDDING_DEDUP_SEARCH_LIMIT: usize = 5;

/// Configuration for embedding-based duplicate detection.
///
/// Not yet wired to `Settings`; defaults are used until config-driven thresholds land.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingDeduplicationConfig {
    pub enabled: bool,
    pub similarity_threshold: f32,
    pub search_limit: usize,
}

impl Default for EmbeddingDeduplicationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            similarity_threshold: DEFAULT_EMBEDDING_DEDUP_SIMILARITY_THRESHOLD,
            search_limit: DEFAULT_EMBEDDING_DEDUP_SEARCH_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeduplicationDecision {
    Duplicate {
        existing_fact_id: Uuid,
        reason: String,
    },
    SimilarButDistinct {
        reason: String,
    },
    New,
}
