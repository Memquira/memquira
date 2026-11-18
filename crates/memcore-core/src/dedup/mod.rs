mod detector;
mod embedding;
mod types;
mod value_guard;

pub use detector::{
    ExactDedupRule, detect_duplicate, detect_duplicate_guarded, exact_dedup_key,
    find_existing_facts_for_dedup, normalize_content, token_overlap_ratio,
};
pub(crate) use embedding::vector_hit_rejection;
pub use embedding::{detect_embedding_duplicate, detect_embedding_duplicate_guarded};
pub use types::{
    DEDUP_SEARCH_LIMIT, DEFAULT_EMBEDDING_DEDUP_SEARCH_LIMIT,
    DEFAULT_EMBEDDING_DEDUP_SIMILARITY_THRESHOLD, DeduplicationDecision, EXACT_DUPLICATE_THRESHOLD,
    EmbeddingDeduplicationConfig, HIGH_SIMILARITY_DUPLICATE_THRESHOLD,
    MODERATE_SIMILARITY_THRESHOLD,
};
pub use value_guard::{
    DedupValueGuard, SentenceValues, ValueConflict, sentence_values, value_compatibility,
};
