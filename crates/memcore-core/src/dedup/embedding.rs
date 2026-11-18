use memcore_common::MemcoreResult;

use crate::observability::{record_dedup_value_guard, record_vector_hit_rejected};
use crate::ports::{FactStore, VectorSearchQuery, VectorSearchResult, VectorStore};
use crate::{CandidateFact, Fact, MemoryType, TenantContext};

use super::value_guard::{ValueConflict, value_compatibility};

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
    detect_embedding_duplicate_guarded(
        fact_store,
        vector_store,
        tenant,
        memory_type,
        embedding,
        config,
        None,
    )
    .await
}

/// [`detect_embedding_duplicate`] with the value guard
/// (`MEMCORE_DEDUP_VALUE_GUARD=on`): `guarded` is the candidate whose values are compared. A hit at
/// or above the threshold that is not value-compatible with the candidate
/// ([`super::value_compatibility`]) is skipped like an unconfirmed hit, and the next hit is
/// examined; with no compatible hit the candidate is not a duplicate. `None`: the legacy rule.
pub async fn detect_embedding_duplicate_guarded(
    fact_store: &dyn FactStore,
    vector_store: &dyn VectorStore,
    tenant: &TenantContext,
    memory_type: MemoryType,
    embedding: &[f32],
    config: &EmbeddingDeduplicationConfig,
    guarded: Option<&CandidateFact>,
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
        if let (Some(candidate), Some(existing)) = (guarded, fact.as_ref()) {
            let conflict = value_compatibility(
                &candidate.content,
                candidate.structure.event_time.as_ref(),
                &existing.content,
                existing.structure.event_time.as_ref(),
            )
            .err();
            record_dedup_value_guard(
                "embedding",
                conflict.map_or("duplicate", ValueConflict::as_str),
            );
            if conflict.is_some() {
                continue;
            }
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
