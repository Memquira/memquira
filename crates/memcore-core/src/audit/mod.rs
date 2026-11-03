use std::sync::Arc;

use memcore_common::MemcoreResult;
use serde_json::{Value, json};

use crate::ports::MemoryEventStore;
use crate::{Fact, FactOperationDecision, MemoryEvent, MemoryEventOperation, TenantContext};

/// Records an audit event when a store is configured. Best-effort: a failure does not fail the
/// operation, and the event is not retried or stored later. Each failure
/// is counted (`memcore_cross_store_write_failures_total{path="event"}`), without its text.
pub async fn record_event_best_effort(
    store: &Option<Arc<dyn MemoryEventStore>>,
    tenant: &TenantContext,
    event: MemoryEvent,
) {
    if let Some(store) = store.as_ref()
        && store.record_event(tenant, event).await.is_err()
    {
        crate::observability::record_write_failure("event");
    }
}

pub fn audit_metadata_for_decision(decision: &FactOperationDecision) -> Value {
    json!({
        "reason": decision.reason,
        "classification_confidence": decision.confidence,
        "target_fact_id": decision.target_fact_id,
    })
}

pub fn build_add_event(
    tenant: &TenantContext,
    fact: &Fact,
    provider_name: &Option<String>,
    model_name: &Option<String>,
    metadata: Value,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        Some(fact.id),
        MemoryEventOperation::Add,
        None,
        Some(fact.content.clone()),
        provider_name.clone(),
        model_name.clone(),
        metadata,
    )
}

pub fn build_update_event(
    tenant: &TenantContext,
    previous: &Fact,
    updated: &Fact,
    provider_name: &Option<String>,
    model_name: &Option<String>,
    metadata: Value,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        Some(updated.id),
        MemoryEventOperation::Update,
        Some(previous.content.clone()),
        Some(updated.content.clone()),
        provider_name.clone(),
        model_name.clone(),
        metadata,
    )
}

pub fn build_delete_event(
    tenant: &TenantContext,
    fact: &Fact,
    provider_name: &Option<String>,
    model_name: &Option<String>,
    metadata: Value,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        Some(fact.id),
        MemoryEventOperation::Delete,
        Some(fact.content.clone()),
        None,
        provider_name.clone(),
        model_name.clone(),
        metadata,
    )
}

pub fn build_noop_event(
    tenant: &TenantContext,
    decision: &FactOperationDecision,
    provider_name: &Option<String>,
    model_name: &Option<String>,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        decision.target_fact_id,
        MemoryEventOperation::NoOp,
        None,
        None,
        provider_name.clone(),
        model_name.clone(),
        audit_metadata_for_decision(decision),
    )
}

/// `fact` (its state before) was superseded and kept as history with
/// `invalid_at` set. `metadata` carries the decision's audit fields.
pub fn build_invalidate_event(
    tenant: &TenantContext,
    fact: &Fact,
    provider_name: &Option<String>,
    model_name: &Option<String>,
    metadata: Value,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        Some(fact.id),
        MemoryEventOperation::Invalidate,
        Some(fact.content.clone()),
        None,
        provider_name.clone(),
        model_name.clone(),
        metadata,
    )
}

/// Consolidation v2 NOOP, with the decision's audit fields.
pub fn build_consolidation_noop_event(
    tenant: &TenantContext,
    target_fact_id: Option<uuid::Uuid>,
    provider_name: &Option<String>,
    model_name: &Option<String>,
    metadata: Value,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        target_fact_id,
        MemoryEventOperation::NoOp,
        None,
        None,
        provider_name.clone(),
        model_name.clone(),
        metadata,
    )
}

pub fn build_import_replace_event(
    tenant: &TenantContext,
    provider_name: &Option<String>,
    model_name: &Option<String>,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        None,
        MemoryEventOperation::NoOp,
        None,
        None,
        provider_name.clone(),
        model_name.clone(),
        json!({ "source": "import_replace" }),
    )
}

pub fn build_forget_user_event(
    tenant: &TenantContext,
    provider_name: &Option<String>,
    model_name: &Option<String>,
) -> MemoryEvent {
    MemoryEvent::new(
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        None,
        MemoryEventOperation::ForgetUser,
        None,
        None,
        provider_name.clone(),
        model_name.clone(),
        json!({ "deleted": true }),
    )
}

/// Validates list query limits for memory audit events.
pub fn normalize_event_list_limit(limit: usize) -> MemcoreResult<usize> {
    use crate::ports::{DEFAULT_MEMORY_EVENT_LIST_LIMIT, MAX_MEMORY_EVENT_LIST_LIMIT};
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Ok(DEFAULT_MEMORY_EVENT_LIST_LIMIT);
    }

    if limit > MAX_MEMORY_EVENT_LIST_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_MEMORY_EVENT_LIST_LIMIT}"
        )));
    }

    Ok(limit)
}
