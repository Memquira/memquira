use chrono::{DateTime, Utc};
use memcore_common::MemcoreError;
use memcore_core::{
    DEFAULT_LIST_MEMORY_EVENTS_LIMIT, ListMemoryEventsOutput, MemoryEvent, MemoryEventOperation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

pub fn default_list_memory_events_limit() -> usize {
    DEFAULT_LIST_MEMORY_EVENTS_LIMIT
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListMemoryEventsQuery {
    pub operation: Option<String>,
    pub fact_id: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub q: Option<String>,
    #[serde(default = "default_list_memory_events_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListMemoryEventsResponse {
    pub status: &'static str,
    pub events: Vec<MemoryEventItemResponse>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MemoryEventItemResponse {
    pub id: Uuid,
    pub fact_id: Option<Uuid>,
    pub operation: MemoryEventOperationResponse,
    pub previous_content: Option<String>,
    pub new_content: Option<String>,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

/// API-facing operation labels (PascalCase) separate from core snake_case serde.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "PascalCase")]
pub enum MemoryEventOperationResponse {
    Add,
    Update,
    Delete,
    NoOp,
    ForgetUser,
    Invalidate,
    EntityMerge,
    EntitySplit,
}

impl From<MemoryEventOperation> for MemoryEventOperationResponse {
    fn from(value: MemoryEventOperation) -> Self {
        match value {
            MemoryEventOperation::Add => Self::Add,
            MemoryEventOperation::Update => Self::Update,
            MemoryEventOperation::Delete => Self::Delete,
            MemoryEventOperation::NoOp => Self::NoOp,
            MemoryEventOperation::ForgetUser => Self::ForgetUser,
            MemoryEventOperation::Invalidate => Self::Invalidate,
            MemoryEventOperation::EntityMerge => Self::EntityMerge,
            MemoryEventOperation::EntitySplit => Self::EntitySplit,
        }
    }
}

impl From<&MemoryEvent> for MemoryEventItemResponse {
    fn from(event: &MemoryEvent) -> Self {
        Self {
            id: event.id,
            fact_id: event.fact_id,
            operation: event.operation.into(),
            previous_content: event.previous_content.clone(),
            new_content: event.new_content.clone(),
            provider_name: event.provider_name.clone(),
            model_name: event.model_name.clone(),
            metadata: event.metadata.clone(),
            created_at: event.created_at,
        }
    }
}

impl From<ListMemoryEventsOutput> for ListMemoryEventsResponse {
    fn from(output: ListMemoryEventsOutput) -> Self {
        Self {
            status: "success",
            events: output
                .events
                .iter()
                .map(MemoryEventItemResponse::from)
                .collect(),
            next_cursor: output.next_cursor,
        }
    }
}

pub fn parse_memory_event_operation_label(
    label: &str,
) -> Result<MemoryEventOperation, MemcoreError> {
    match label.trim() {
        "Add" => Ok(MemoryEventOperation::Add),
        "Update" => Ok(MemoryEventOperation::Update),
        "Delete" => Ok(MemoryEventOperation::Delete),
        "NoOp" => Ok(MemoryEventOperation::NoOp),
        "ForgetUser" => Ok(MemoryEventOperation::ForgetUser),
        _ => Err(MemcoreError::ValidationError(
            "invalid operation".to_string(),
        )),
    }
}

pub fn parse_optional_rfc3339_timestamp(
    value: Option<&String>,
    field_name: &str,
) -> Result<Option<DateTime<Utc>>, MemcoreError> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let parsed = DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
                MemcoreError::ValidationError(format!("invalid {field_name} timestamp"))
            })?;
            Ok(Some(parsed.with_timezone(&Utc)))
        }
    }
}

pub type OptionalUtcRange = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

pub fn parse_event_date_filters(
    created_after: Option<&String>,
    created_before: Option<&String>,
) -> Result<OptionalUtcRange, MemcoreError> {
    let created_after = parse_optional_rfc3339_timestamp(created_after, "created_after")?;
    let created_before = parse_optional_rfc3339_timestamp(created_before, "created_before")?;
    memcore_core::validate_event_date_range(created_after, created_before)?;
    Ok((created_after, created_before))
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;

    #[test]
    fn parse_created_after_accepts_rfc3339() {
        let (after, before) =
            parse_event_date_filters(Some(&"2026-01-01T00:00:00Z".to_string()), None)
                .expect("parse");
        assert!(after.is_some());
        assert!(before.is_none());
    }

    #[test]
    fn invalid_created_before_returns_validation_error() {
        let error = parse_event_date_filters(None, Some(&"not-a-timestamp".to_string()))
            .expect_err("invalid");
        assert_eq!(
            error,
            MemcoreError::ValidationError("invalid created_before timestamp".to_string())
        );
    }

    #[test]
    fn created_after_must_be_earlier_than_created_before() {
        let error = parse_event_date_filters(
            Some(&"2026-06-01T00:00:00Z".to_string()),
            Some(&"2026-01-01T00:00:00Z".to_string()),
        )
        .expect_err("invalid range");
        assert_eq!(
            error,
            MemcoreError::ValidationError(
                "created_after must be earlier than created_before".to_string()
            )
        );
    }
}
