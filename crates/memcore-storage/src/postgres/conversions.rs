use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    Fact, FactStructure, MemoryEvent, MemoryEventOperation, MemorySource, MemoryType,
    TemporalGranularity, TemporalValue,
};
use serde_json::Value;
use uuid::Uuid;

pub(crate) fn memory_type_to_str(value: MemoryType) -> &'static str {
    value.as_str()
}

pub(crate) fn memory_type_from_str(value: &str) -> MemcoreResult<MemoryType> {
    MemoryType::parse_label(value)
        .map_err(|_| MemcoreError::StorageError(format!("invalid memory_type value: {value}")))
}

pub(crate) fn memory_source_to_str(value: MemorySource) -> &'static str {
    match value {
        MemorySource::UserMessage => "user_message",
        MemorySource::AssistantMessage => "assistant_message",
        MemorySource::ApiImport => "api_import",
        MemorySource::Manual => "manual",
        MemorySource::System => "system",
    }
}

pub(crate) fn memory_source_from_str(value: &str) -> MemcoreResult<MemorySource> {
    match value {
        "user_message" => Ok(MemorySource::UserMessage),
        "assistant_message" => Ok(MemorySource::AssistantMessage),
        "api_import" => Ok(MemorySource::ApiImport),
        "manual" => Ok(MemorySource::Manual),
        "system" => Ok(MemorySource::System),
        _ => Err(MemcoreError::StorageError(format!(
            "invalid source value: {value}"
        ))),
    }
}

pub(crate) fn row_to_fact(
    id: Uuid,
    org_id: String,
    user_id: String,
    memory_type: String,
    content: String,
    summary: Option<String>,
    source: String,
    confidence: f64,
    importance: f64,
    valid_at: Option<DateTime<Utc>>,
    invalid_at: Option<DateTime<Utc>>,
    recorded_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    metadata: Value,
) -> MemcoreResult<Fact> {
    Ok(Fact {
        id,
        org_id,
        user_id,
        memory_type: memory_type_from_str(&memory_type)?,
        content,
        summary,
        source: memory_source_from_str(&source)?,
        confidence: confidence as f32,
        importance: importance as f32,
        valid_at,
        invalid_at,
        recorded_at,
        updated_at,
        metadata,
        structure: FactStructure::default(),
        superseded_at: None,
        superseded_by: None,
    })
}

/// The Part B columns as Postgres stores them.
pub(crate) struct StructureColumns {
    pub subject: Option<String>,
    pub event_time_start: Option<DateTime<Utc>>,
    pub event_time_end: Option<DateTime<Utc>>,
    pub event_time_granularity: Option<String>,
    pub event_time_text: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub source_message_ids: Option<Value>,
}

pub(crate) fn structure_to_columns(structure: &FactStructure) -> StructureColumns {
    let event_time = structure.event_time.as_ref();
    StructureColumns {
        subject: structure.subject.clone(),
        event_time_start: event_time.map(|value| value.start),
        event_time_end: event_time.map(|value| value.end),
        event_time_granularity: event_time.map(|value| value.granularity.as_str().to_string()),
        event_time_text: structure.event_time_text.clone(),
        // Only a client-supplied observation time is stored (P2-20); see the SQLite conversion.
        observed_at: structure.observed_at,
        session_id: structure.session_id.clone(),
        source_message_ids: if structure.source_message_ids.is_empty() {
            None
        } else {
            Some(Value::from(structure.source_message_ids.clone()))
        },
    }
}

pub(crate) fn structure_from_columns(columns: StructureColumns) -> MemcoreResult<FactStructure> {
    let event_time = match (
        columns.event_time_start,
        columns.event_time_end,
        columns.event_time_granularity,
    ) {
        (Some(start), Some(end), Some(granularity)) => {
            let granularity = granularity
                .parse::<TemporalGranularity>()
                .map_err(|error| MemcoreError::StorageError(error.to_string()))?;
            Some(
                TemporalValue::new(
                    start,
                    end,
                    granularity,
                    columns.observed_at.unwrap_or(start),
                )
                .map_err(|error| MemcoreError::StorageError(error.to_string()))?,
            )
        }
        _ => None,
    };
    let source_message_ids = match columns.source_message_ids {
        Some(value) => serde_json::from_value::<Vec<String>>(value).map_err(|error| {
            MemcoreError::StorageError(format!("invalid source_message_ids: {error}"))
        })?,
        None => Vec::new(),
    };
    Ok(FactStructure {
        subject: columns.subject,
        event_time,
        event_time_text: columns.event_time_text,
        observed_at: columns.observed_at,
        session_id: columns.session_id,
        source_message_ids,
    })
}

pub(crate) fn api_key_scope_to_str(value: memcore_core::ApiKeyScope) -> &'static str {
    match value {
        memcore_core::ApiKeyScope::MemoryRead => "memory_read",
        memcore_core::ApiKeyScope::MemoryWrite => "memory_write",
        memcore_core::ApiKeyScope::MemoryDelete => "memory_delete",
        memcore_core::ApiKeyScope::UserDelete => "user_delete",
        memcore_core::ApiKeyScope::AuditRead => "audit_read",
        memcore_core::ApiKeyScope::AdminRead => "admin_read",
        memcore_core::ApiKeyScope::AdminWrite => "admin_write",
    }
}

pub(crate) fn api_key_scope_from_str(value: &str) -> MemcoreResult<memcore_core::ApiKeyScope> {
    match value {
        "memory_read" => Ok(memcore_core::ApiKeyScope::MemoryRead),
        "memory_write" => Ok(memcore_core::ApiKeyScope::MemoryWrite),
        "memory_delete" => Ok(memcore_core::ApiKeyScope::MemoryDelete),
        "user_delete" => Ok(memcore_core::ApiKeyScope::UserDelete),
        "audit_read" => Ok(memcore_core::ApiKeyScope::AuditRead),
        "admin_read" => Ok(memcore_core::ApiKeyScope::AdminRead),
        "admin_write" => Ok(memcore_core::ApiKeyScope::AdminWrite),
        _ => Err(MemcoreError::StorageError(format!(
            "invalid api key scope value: {value}"
        ))),
    }
}

pub(crate) fn memory_event_operation_to_str(value: MemoryEventOperation) -> &'static str {
    match value {
        MemoryEventOperation::Add => "add",
        MemoryEventOperation::Update => "update",
        MemoryEventOperation::Delete => "delete",
        MemoryEventOperation::NoOp => "no_op",
        MemoryEventOperation::ForgetUser => "forget_user",
        MemoryEventOperation::Invalidate => "invalidate",
        MemoryEventOperation::EntityMerge => "entity_merge",
        MemoryEventOperation::EntitySplit => "entity_split",
    }
}

pub(crate) fn memory_event_operation_from_str(value: &str) -> MemcoreResult<MemoryEventOperation> {
    match value {
        "add" => Ok(MemoryEventOperation::Add),
        "update" => Ok(MemoryEventOperation::Update),
        "delete" => Ok(MemoryEventOperation::Delete),
        "no_op" => Ok(MemoryEventOperation::NoOp),
        "forget_user" => Ok(MemoryEventOperation::ForgetUser),
        "invalidate" => Ok(MemoryEventOperation::Invalidate),
        "entity_merge" => Ok(MemoryEventOperation::EntityMerge),
        "entity_split" => Ok(MemoryEventOperation::EntitySplit),
        _ => Err(MemcoreError::StorageError(format!(
            "invalid memory event operation value: {value}"
        ))),
    }
}

pub(crate) fn row_to_memory_event(
    id: Uuid,
    org_id: String,
    user_id: String,
    fact_id: Option<Uuid>,
    operation: String,
    input_text: Option<String>,
    previous_content: Option<String>,
    new_content: Option<String>,
    provider_name: Option<String>,
    model_name: Option<String>,
    metadata: Value,
    created_at: DateTime<Utc>,
) -> MemcoreResult<MemoryEvent> {
    Ok(MemoryEvent {
        id,
        org_id,
        user_id,
        fact_id,
        operation: memory_event_operation_from_str(&operation)?,
        input_text,
        previous_content,
        new_content,
        provider_name,
        model_name,
        metadata,
        created_at,
    })
}

#[cfg(test)]
mod memory_event_operation_tests {
    use super::*;

    #[test]
    fn every_memory_event_operation_round_trips_including_invalidate() {
        for operation in [
            MemoryEventOperation::Add,
            MemoryEventOperation::Update,
            MemoryEventOperation::Delete,
            MemoryEventOperation::NoOp,
            MemoryEventOperation::ForgetUser,
            MemoryEventOperation::Invalidate,
            MemoryEventOperation::EntityMerge,
            MemoryEventOperation::EntitySplit,
        ] {
            let label = memory_event_operation_to_str(operation);
            assert_eq!(memory_event_operation_from_str(label).unwrap(), operation);
        }
        assert_eq!(
            memory_event_operation_to_str(MemoryEventOperation::Invalidate),
            "invalidate"
        );
    }
}
