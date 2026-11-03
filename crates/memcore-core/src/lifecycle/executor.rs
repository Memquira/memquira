use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use serde_json::Value;
use uuid::Uuid;

use crate::dedup::ExactDedupRule;
use crate::observability::{counted_write, record_write_failure};
use crate::ports::{
    DedupInsert, EmbeddingProvider, FactStore, VectorRecord, VectorStore, VectorWriteIntent,
};
use crate::{
    CandidateFact, Fact, FactOperation, FactOperationDecision, FactStructure, MemorySource,
    TenantContext,
};

/// Outcome of applying a lifecycle operation to a candidate fact.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum LifecycleApplyResult {
    Added(Fact),
    Updated {
        previous: Fact,
        updated: Fact,
    },
    Deleted(Fact),
    NoOp,
    /// An ADD lost to an equivalent fact (same tenant, exact-dedup key) that is
    /// already stored, typically by a concurrent request. Nothing was written.
    Duplicate {
        existing_fact_id: Uuid,
    },
}

pub struct LifecycleContext<'a> {
    pub fact_store: &'a dyn FactStore,
    pub vector_store: &'a dyn VectorStore,
    pub embedding_provider: &'a dyn EmbeddingProvider,
}

/// Maps provider decisions to executable operations. Archive/Summarize are treated as NoOp.
pub fn normalize_operation(decision: &FactOperationDecision) -> MemcoreResult<FactOperation> {
    match decision.operation {
        FactOperation::Add => Ok(FactOperation::Add),
        FactOperation::Update => {
            require_target_fact_id(decision, "update")?;
            Ok(FactOperation::Update)
        }
        FactOperation::Delete => {
            require_target_fact_id(decision, "delete")?;
            Ok(FactOperation::Delete)
        }
        FactOperation::NoOp => Ok(FactOperation::NoOp),
        FactOperation::Archive | FactOperation::Summarize => Ok(FactOperation::NoOp),
    }
}

fn require_target_fact_id(decision: &FactOperationDecision, operation: &str) -> MemcoreResult<()> {
    if decision.target_fact_id.is_none() {
        return Err(MemcoreError::ValidationError(format!(
            "target_fact_id is required for {operation} operation"
        )));
    }
    Ok(())
}

/// Applies `decision` for the default ingestion path: an ADD is deduplicated by the store under
/// [`ExactDedupRule::Legacy`].
pub async fn apply_fact_operation(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    decision: &FactOperationDecision,
    input_metadata: &Value,
    precomputed_embedding: Option<Vec<f32>>,
) -> MemcoreResult<LifecycleApplyResult> {
    apply_fact_operation_with_rule(
        ctx,
        tenant,
        candidate,
        decision,
        input_metadata,
        precomputed_embedding,
        ExactDedupRule::Legacy,
    )
    .await
}

/// [`apply_fact_operation`] with the exact-duplicate rule of the calling ingestion
/// mode, which the store enforces for an ADD.
pub async fn apply_fact_operation_with_rule(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    decision: &FactOperationDecision,
    input_metadata: &Value,
    precomputed_embedding: Option<Vec<f32>>,
    rule: ExactDedupRule,
) -> MemcoreResult<LifecycleApplyResult> {
    apply_fact_operation_with_id(
        ctx,
        tenant,
        candidate,
        decision,
        input_metadata,
        precomputed_embedding,
        rule,
        Uuid::new_v4(),
    )
    .await
}

/// [`apply_fact_operation_with_rule`] where an ADD stores the fact under
/// `new_fact_id`, allocated (and journalled) by the caller before anything is written. Other
/// operations ignore it.
#[allow(clippy::too_many_arguments)]
pub async fn apply_fact_operation_with_id(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    decision: &FactOperationDecision,
    input_metadata: &Value,
    precomputed_embedding: Option<Vec<f32>>,
    rule: ExactDedupRule,
    new_fact_id: Uuid,
) -> MemcoreResult<LifecycleApplyResult> {
    let operation = normalize_operation(decision)?;

    match operation {
        FactOperation::Add => {
            apply_add(
                ctx,
                tenant,
                candidate,
                input_metadata,
                precomputed_embedding,
                rule,
                new_fact_id,
            )
            .await
        }
        FactOperation::Update => {
            let target_id = decision
                .target_fact_id
                .expect("target_fact_id validated above");
            apply_update(ctx, tenant, candidate, target_id).await
        }
        FactOperation::Delete => {
            let target_id = decision
                .target_fact_id
                .expect("target_fact_id validated above");
            apply_delete(ctx, tenant, target_id).await
        }
        FactOperation::NoOp => Ok(LifecycleApplyResult::NoOp),
        FactOperation::Archive | FactOperation::Summarize => Ok(LifecycleApplyResult::NoOp),
    }
}

async fn apply_add(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    input_metadata: &Value,
    precomputed_embedding: Option<Vec<f32>>,
    rule: ExactDedupRule,
    fact_id: Uuid,
) -> MemcoreResult<LifecycleApplyResult> {
    // The fact id is fresh (allocated by the caller for this one ADD), and its single point id is
    // allocated once below, so no other point of this fact can exist.
    let fact = candidate_to_fact(tenant, candidate, input_metadata, fact_id)?;
    // The store decides atomically whether an active fact that `rule` (the calling
    // mode's exact rule) calls a duplicate already exists. A fact stored already inactive holds no
    // key.
    let inserted = if fact.is_active() {
        let key = crate::dedup::exact_dedup_key(rule, &fact);
        match ctx
            .fact_store
            .insert_fact_deduplicated(tenant, fact, &key)
            .await?
        {
            DedupInsert::Inserted(fact) => fact,
            DedupInsert::Duplicate { existing_fact_id } => {
                return Ok(LifecycleApplyResult::Duplicate { existing_fact_id });
            }
        }
    } else {
        ctx.fact_store.insert_fact(tenant, fact).await?
    };
    // From here a failure leaves the stored row without its point.
    let written = async {
        let embedding = match precomputed_embedding {
            Some(embedding) => embedding,
            None => ctx.embedding_provider.embed_text(&inserted.content).await?,
        };
        ctx.vector_store
            .write_vector(
                tenant,
                vector_record_from_fact(&inserted, embedding),
                VectorWriteIntent::FreshFact,
            )
            .await
    }
    .await;
    counted_write("fact_add", written)?;
    Ok(LifecycleApplyResult::Added(inserted))
}

async fn apply_update(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    target_fact_id: Uuid,
) -> MemcoreResult<LifecycleApplyResult> {
    let existing = ctx
        .fact_store
        .get_fact(tenant, target_fact_id)
        .await?
        .ok_or_else(|| MemcoreError::NotFound("memory not found".to_string()))?;

    let updated = merge_candidate_into_fact(&existing, candidate)?;
    let stored = update_with_vector(ctx, tenant, updated).await?;
    Ok(LifecycleApplyResult::Updated {
        previous: existing,
        updated: stored,
    })
}

async fn apply_delete(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    target_fact_id: Uuid,
) -> MemcoreResult<LifecycleApplyResult> {
    let existing = ctx
        .fact_store
        .get_fact(tenant, target_fact_id)
        .await?
        .ok_or_else(|| MemcoreError::NotFound("memory not found".to_string()))?;

    ctx.fact_store
        .soft_delete_fact(tenant, target_fact_id)
        .await?;

    match ctx
        .vector_store
        .delete_by_fact_id(tenant, target_fact_id)
        .await
    {
        Ok(()) => {}
        Err(MemcoreError::NotFound(_)) => {}
        Err(error) => {
            // The row is soft-deleted, its point remains.
            record_write_failure("fact_delete");
            return Err(error);
        }
    }

    Ok(LifecycleApplyResult::Deleted(existing))
}

/// Consolidation v2 UPDATE: the target keeps its id and gains the candidate's structured fields
/// and provenance (as a legacy update), with `content` as its new text; its vector is rebuilt.
pub async fn apply_update_with_content(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    target_fact_id: Uuid,
    content: &str,
) -> MemcoreResult<LifecycleApplyResult> {
    let existing = ctx
        .fact_store
        .get_fact(tenant, target_fact_id)
        .await?
        .ok_or_else(|| MemcoreError::NotFound("memory not found".to_string()))?;
    let mut updated = merge_candidate_into_fact(&existing, candidate)?;
    updated.content = content.to_string();
    let stored = update_with_vector(ctx, tenant, updated).await?;
    Ok(LifecycleApplyResult::Updated {
        previous: existing,
        updated: stored,
    })
}

/// Consolidation v2 INVALIDATE: marks the target superseded at `superseded_at` (bookkeeping
/// time) and sets `invalid_at` only when a source-supported change time is known. Keeps the
/// row and its vector (history; search leaves it out unless a request asks for history). Never
/// deletes. Returns the fact before and after.
pub async fn apply_invalidate(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    target_fact_id: Uuid,
    invalid_at: Option<chrono::DateTime<Utc>>,
    superseded_at: chrono::DateTime<Utc>,
) -> MemcoreResult<(Fact, Fact)> {
    let existing = ctx
        .fact_store
        .get_fact(tenant, target_fact_id)
        .await?
        .ok_or_else(|| MemcoreError::NotFound("memory not found".to_string()))?;
    let mut invalidated = existing.clone();
    if invalid_at.is_some() {
        invalidated.invalid_at = invalid_at;
    }
    invalidated.superseded_at = Some(superseded_at);
    invalidated.updated_at = superseded_at;
    let stored = ctx.fact_store.update_fact(tenant, invalidated).await?;
    Ok((existing, stored))
}

/// Records `replacement_fact_id` as the fact that superseded `superseded_fact_id` (provenance
/// only; the superseded fact is already inactive through `superseded_at`).
pub async fn link_superseded_by(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    superseded_fact_id: Uuid,
    replacement_fact_id: Uuid,
) -> MemcoreResult<Fact> {
    let mut superseded = ctx
        .fact_store
        .get_fact(tenant, superseded_fact_id)
        .await?
        .ok_or_else(|| MemcoreError::NotFound("memory not found".to_string()))?;
    superseded.superseded_by = Some(replacement_fact_id);
    ctx.fact_store.update_fact(tenant, superseded).await
}

/// rollback: restores `before` (a journalled before-image of an updated fact) as the
/// row and replaces its vector, the same way as an UPDATE (embedding checked first).
pub(crate) async fn restore_fact(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    before: Fact,
) -> MemcoreResult<Fact> {
    update_with_vector(ctx, tenant, before).await
}

/// Rounds an UPDATE may take to reach one current point per fact under concurrent updates of the
/// same fact before it reports a conflict.
const MAX_UPDATE_SETTLE_ROUNDS: usize = 8;

/// Updates a fact and replaces its vector (legacy and consolidation-v2 UPDATE).
///
/// The replacement embedding is computed and checked BEFORE the fact row or any vector changes:
/// a provider failure leaves both stores as they were. This does not make
/// SQL and Qdrant transactional; a vector-store failure after the row update still leaves them
/// apart.
async fn update_with_vector(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    updated: Fact,
) -> MemcoreResult<Fact> {
    let embedding = checked_embedding(ctx, &updated.content).await?;
    let embedded_text = updated.content.clone();
    let stored = ctx.fact_store.update_fact(tenant, updated.clone()).await?;
    // From here a failure leaves the updated row with an old or missing point.
    counted_write(
        "fact_update",
        settle_updated_vector(ctx, tenant, stored, embedding, embedded_text).await,
    )
}

/// The vector half of [`update_with_vector`], after the row update: replace the fact's vector and
/// verify until it has exactly one point carrying the current row's text.
async fn settle_updated_vector(
    ctx: &LifecycleContext<'_>,
    tenant: &TenantContext,
    stored: Fact,
    mut embedding: Vec<f32>,
    mut embedded_text: String,
) -> MemcoreResult<Fact> {
    let mut current = stored.clone();
    for round in 0..MAX_UPDATE_SETTLE_ROUNDS {
        if current.content != embedded_text {
            // The store normalised the text, or a concurrent update changed it: embed what is
            // stored (always before this round's vector change).
            embedding = checked_embedding(ctx, &current.content).await?;
            embedded_text = current.content.clone();
        }
        ctx.vector_store
            .write_vector(
                tenant,
                vector_record_from_fact(&current, embedding.clone()),
                VectorWriteIntent::Replace,
            )
            .await?;

        // Concurrent updates of the same fact, in this process or another, can
        // interleave between the row update and the vector write. Verify the shared stores after
        // every vector write and repair until the fact has exactly one point carrying the
        // current row's text. The last writer to change the vector always verifies after its
        // own change, so the final state converges without locks.
        let Some(points) = ctx
            .vector_store
            .fact_vector_points(tenant, current.id)
            .await?
        else {
            return Ok(stored);
        };
        let Some(latest) = ctx.fact_store.get_fact(tenant, current.id).await? else {
            // Deleted concurrently: remove any vector this update re-created.
            match ctx.vector_store.delete_by_fact_id(tenant, current.id).await {
                Ok(()) | Err(MemcoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
            return Ok(stored);
        };
        if points.len() == 1
            && points[0].content == latest.content
            && points[0].memory_type == latest.memory_type
        {
            return Ok(stored);
        }
        current = latest;
        // Small jitter so racing writers stop colliding.
        let jitter_ms = 1 + (Uuid::new_v4().as_u128() % 10) as u64 * (round as u64 + 1);
        tokio::time::sleep(std::time::Duration::from_millis(jitter_ms)).await;
    }
    Err(MemcoreError::Conflict(
        "concurrent updates of this memory did not settle; retry the update".to_string(),
    ))
}

/// An embedding that is non-empty and finite (dimension checks stay in the vector store, which
/// makes them before it deletes anything).
async fn checked_embedding(ctx: &LifecycleContext<'_>, text: &str) -> MemcoreResult<Vec<f32>> {
    checked_embedding_from(ctx.embedding_provider, text).await
}

/// `checked_embedding` for a caller without a lifecycle context (the retry heal):
/// the same provider call and the same non-empty / finite check.
pub(crate) async fn checked_embedding_from(
    provider: &dyn EmbeddingProvider,
    text: &str,
) -> MemcoreResult<Vec<f32>> {
    let embedding = provider.embed_text(text).await?;
    if embedding.is_empty() || embedding.iter().any(|value| !value.is_finite()) {
        return Err(MemcoreError::ProviderError(
            "embedding provider returned an empty or non-finite vector".to_string(),
        ));
    }
    Ok(embedding)
}

pub(crate) fn vector_record_from_fact(fact: &Fact, embedding: Vec<f32>) -> VectorRecord {
    VectorRecord {
        id: Uuid::new_v4(),
        fact_id: fact.id,
        org_id: fact.org_id.clone(),
        user_id: fact.user_id.clone(),
        embedding,
        content: fact.content.clone(),
        memory_type: fact.memory_type,
        metadata: fact.metadata.clone(),
    }
}

fn merge_candidate_into_fact(existing: &Fact, candidate: &CandidateFact) -> MemcoreResult<Fact> {
    let now = Utc::now();
    Fact::new(
        existing.id,
        existing.org_id.clone(),
        existing.user_id.clone(),
        candidate.memory_type,
        candidate.content.clone(),
        existing.summary.clone(),
        existing.source,
        candidate.confidence,
        candidate.importance,
        candidate.valid_at,
        existing.invalid_at,
        existing.recorded_at,
        now,
        candidate.metadata.clone(),
    )
    .map(|fact| fact.with_structure(merged_structure(&existing.structure, &candidate.structure)))
}

/// On an update the new candidate's structured fields win, but provenance accumulates: the merged
/// fact came from the existing fact's messages as well as the new ones.
fn merged_structure(existing: &FactStructure, candidate: &FactStructure) -> FactStructure {
    if candidate.is_empty() {
        return existing.clone();
    }
    let mut source_message_ids = existing.source_message_ids.clone();
    for id in &candidate.source_message_ids {
        if !source_message_ids.contains(id) {
            source_message_ids.push(id.clone());
        }
    }
    FactStructure {
        subject: candidate
            .subject
            .clone()
            .or_else(|| existing.subject.clone()),
        event_time: candidate
            .event_time
            .clone()
            .or_else(|| existing.event_time.clone()),
        event_time_text: candidate
            .event_time_text
            .clone()
            .or_else(|| existing.event_time_text.clone()),
        observed_at: candidate.observed_at.or(existing.observed_at),
        session_id: candidate
            .session_id
            .clone()
            .or_else(|| existing.session_id.clone()),
        source_message_ids,
    }
}

fn candidate_to_fact(
    tenant: &TenantContext,
    candidate: &CandidateFact,
    _input_metadata: &Value,
    fact_id: Uuid,
) -> MemcoreResult<Fact> {
    let now = Utc::now();
    Fact::new(
        fact_id,
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        candidate.memory_type,
        candidate.content.clone(),
        None,
        MemorySource::UserMessage,
        candidate.confidence,
        candidate.importance,
        candidate.valid_at,
        None,
        now,
        now,
        candidate.metadata.clone(),
    )
    .map(|fact| fact.with_structure(candidate.structure.clone()))
}
