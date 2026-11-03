mod detector;
mod embedding;
mod types;

pub use detector::{
    ExactDedupRule, detect_duplicate, exact_dedup_key, find_existing_facts_for_dedup,
    normalize_content, token_overlap_ratio,
};
pub use embedding::detect_embedding_duplicate;
pub(crate) use embedding::vector_hit_rejection;
pub use types::{
    DEDUP_SEARCH_LIMIT, DEFAULT_EMBEDDING_DEDUP_SEARCH_LIMIT,
    DEFAULT_EMBEDDING_DEDUP_SIMILARITY_THRESHOLD, DeduplicationDecision, EXACT_DUPLICATE_THRESHOLD,
    EmbeddingDeduplicationConfig, HIGH_SIMILARITY_DUPLICATE_THRESHOLD,
    MODERATE_SIMILARITY_THRESHOLD,
};
