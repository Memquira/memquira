use memcore_common::MemcoreResult;

use crate::observability::record_vector_hit_rejected;
use crate::ports::{FactStore, VectorSearchQuery, VectorSearchResult, VectorStore};
use crate::{Fact, MemoryType, TenantContext};

use super::types::{DeduplicationDecision, EmbeddingDeduplicationConfig};

/// Why a vector hit is NOT backed by `fact`, the tenant-scoped live
/// SQL read for the hit's fact id (`None` = no live row), or `None` when it is. SQL is the
/// authority: a hit counts only if the row exists, belongs to the tenant and the point carries the
/// row's current text and memory type. A point left behind by a failed vector write after an
/// update (stale text) does not count. The reasons are the fixed R6 metric labels.
pub(crate) fn vector_hit_rejection(
    hit: &VectorSearchResult,
    fact: Option<&Fact>,
    tenant: &TenantContext,
) -> Option<&'static str> {
    match fact {
        Some(fact)
            if fact.id == hit.fact_id
                && fact.org_id == tenant.org_id
                && fact.user_id == tenant.user_id =>
        {
            (fact.content != hit.content || fact.memory_type != hit.memory_type)
                .then_some("stale_content")
        }
        _ => Some("no_live_row"),
    }
}

/// Checks vector store for semantically similar facts above the configured threshold.
///
/// Search scores are treated as **cosine similarity** in \[0.0, 1.0\] (higher = more similar).
/// A match is a duplicate when `score >= similarity_threshold` **and** the
/// tenant's live SQL fact confirms the hit: an orphaned point (fact absent, soft-deleted or of
/// another tenant) or a stale one (text changed in SQL) never suppresses a candidate; the next hit
/// is examined instead. A SQL read error is returned (nothing is suppressed or written).
pub async fn detect_embedding_duplicate(
    fact_store: &dyn FactStore,
    vector_store: &dyn VectorStore,
    tenant: &TenantContext,
    memory_type: MemoryType,
    embedding: &[f32],
    config: &EmbeddingDeduplicationConfig,
) -> MemcoreResult<Option<DeduplicationDecision>> {
    if !config.enabled {
        return Ok(None);
    }

    let results = vector_store
        .search_vectors(VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: embedding.to_vec(),
            limit: config.search_limit,
            memory_types: Some(vec![memory_type]),
            metadata_filter: None,
        })
        .await?;

    for result in results {
        if result.score < config.similarity_threshold {
            continue;
        }
        let fact = fact_store.get_fact(tenant, result.fact_id).await?;
        if let Some(reason) = vector_hit_rejection(&result, fact.as_ref(), tenant) {
            record_vector_hit_rejected("dedup", reason);
            continue;
        }
        return Ok(Some(DeduplicationDecision::Duplicate {
            existing_fact_id: result.fact_id,
            reason: format!(
                "embedding similarity ({:.2} >= {:.2})",
                result.score, config.similarity_threshold
            ),
        }));
    }

    Ok(None)
}
