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

pub(crate) fn datetime_to_str(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

/// `datetime_to_str`'s layout with chrono's signed year: `to_rfc3339` writes a year outside
/// 0000-9999 with a sign and more digits ("-0010-03-01T00:00:00+00:00"), which RFC 3339 parsing
/// rejects.
const SIGNED_YEAR_RFC3339: &str = "%Y-%m-%dT%H:%M:%S%.f%:z";

/// Reads back every value `datetime_to_str` writes, so a stored row can always be read: one row
/// with such a year (an event time resolved from "March 1-10") otherwise failed every read that
/// listed it, and with it every later ingestion of the same memory type (observed in an internal long-conversation evaluation).
pub(crate) fn datetime_from_str(value: &str) -> MemcoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .or_else(|error| DateTime::parse_from_str(value, SIGNED_YEAR_RFC3339).map_err(|_| error))
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| {
            MemcoreError::StorageError(format!("invalid datetime value '{value}': {error}"))
        })
}

pub(crate) fn optional_datetime_to_str(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(datetime_to_str)
}

pub(crate) fn optional_datetime_from_str(
    value: Option<String>,
) -> MemcoreResult<Option<DateTime<Utc>>> {
    match value {
        Some(raw) => Ok(Some(datetime_from_str(&raw)?)),
        None => Ok(None),
    }
}

pub(crate) fn metadata_to_str(value: &Value) -> MemcoreResult<String> {
    serde_json::to_string(value).map_err(|error| {
        MemcoreError::StorageError(format!("failed to serialize metadata: {error}"))
    })
}

pub(crate) fn metadata_from_str(value: &str) -> MemcoreResult<Value> {
    serde_json::from_str(value).map_err(|error| {
        MemcoreError::StorageError(format!("failed to deserialize metadata: {error}"))
    })
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

pub(crate) fn optional_uuid_to_str(value: Option<Uuid>) -> Option<String> {
    value.map(|id| id.to_string())
}

pub(crate) fn optional_uuid_from_str(value: Option<String>) -> MemcoreResult<Option<Uuid>> {
    match value {
        Some(raw) => Ok(Some(Uuid::parse_str(&raw).map_err(|error| {
            MemcoreError::StorageError(format!("invalid fact_id '{raw}': {error}"))
        })?)),
        None => Ok(None),
    }
}

pub(crate) fn row_to_memory_event(
    id: String,
    org_id: String,
    user_id: String,
    fact_id: Option<String>,
    operation: String,
    input_text: Option<String>,
    previous_content: Option<String>,
    new_content: Option<String>,
    provider_name: Option<String>,
    model_name: Option<String>,
    metadata: String,
    created_at: String,
) -> MemcoreResult<MemoryEvent> {
    Ok(MemoryEvent {
        id: Uuid::parse_str(&id).map_err(|error| {
            MemcoreError::StorageError(format!("invalid memory event id '{id}': {error}"))
        })?,
        org_id,
        user_id,
        fact_id: optional_uuid_from_str(fact_id)?,
        operation: memory_event_operation_from_str(&operation)?,
        input_text,
        previous_content,
        new_content,
        provider_name,
        model_name,
        metadata: metadata_from_str(&metadata)?,
        created_at: datetime_from_str(&created_at)?,
    })
}

pub(crate) fn row_to_fact(
    id: String,
    org_id: String,
    user_id: String,
    memory_type: String,
    content: String,
    summary: Option<String>,
    source: String,
    confidence: f64,
    importance: f64,
    valid_at: Option<String>,
    invalid_at: Option<String>,
    recorded_at: String,
    updated_at: String,
    metadata: String,
) -> MemcoreResult<Fact> {
    Ok(Fact {
        id: Uuid::parse_str(&id).map_err(|error| {
            MemcoreError::StorageError(format!("invalid fact id '{id}': {error}"))
        })?,
        org_id,
        user_id,
        memory_type: memory_type_from_str(&memory_type)?,
        content,
        summary,
        source: memory_source_from_str(&source)?,
        confidence: confidence as f32,
        importance: importance as f32,
        valid_at: optional_datetime_from_str(valid_at)?,
        invalid_at: optional_datetime_from_str(invalid_at)?,
        recorded_at: datetime_from_str(&recorded_at)?,
        updated_at: datetime_from_str(&updated_at)?,
        metadata: metadata_from_str(&metadata)?,
        structure: FactStructure::default(),
        superseded_at: None,
        superseded_by: None,
    })
}

/// The Part B columns as SQLite stores them (all TEXT, all nullable).
pub(crate) struct StructureColumns {
    pub subject: Option<String>,
    pub event_time_start: Option<String>,
    pub event_time_end: Option<String>,
    pub event_time_granularity: Option<String>,
    pub event_time_text: Option<String>,
    pub observed_at: Option<String>,
    pub session_id: Option<String>,
    pub source_message_ids: Option<String>,
}

pub(crate) fn structure_to_columns(structure: &FactStructure) -> MemcoreResult<StructureColumns> {
    let event_time = structure.event_time.as_ref();
    Ok(StructureColumns {
        subject: structure.subject.clone(),
        event_time_start: event_time.map(|value| datetime_to_str(value.start)),
        event_time_end: event_time.map(|value| datetime_to_str(value.end)),
        event_time_granularity: event_time.map(|value| value.granularity.as_str().to_string()),
        event_time_text: structure.event_time_text.clone(),
        // Only a client-supplied observation time is stored (P2-20). An event time settled without
        // one (an absolute date) carries its own start as anchor, which is not an observation time;
        // reading it back without `observed_at` restores exactly that anchor.
        observed_at: structure.observed_at.map(datetime_to_str),
        session_id: structure.session_id.clone(),
        source_message_ids: if structure.source_message_ids.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&structure.source_message_ids).map_err(|error| {
                    MemcoreError::StorageError(format!(
                        "failed to encode source_message_ids: {error}"
                    ))
                })?,
            )
        },
    })
}

pub(crate) fn structure_from_columns(columns: StructureColumns) -> MemcoreResult<FactStructure> {
    let observed_at = optional_datetime_from_str(columns.observed_at)?;
    let event_time = match (
        columns.event_time_start,
        columns.event_time_end,
        columns.event_time_granularity,
    ) {
        (Some(start), Some(end), Some(granularity)) => {
            let start = datetime_from_str(&start)?;
            let end = datetime_from_str(&end)?;
            let granularity = granularity
                .parse::<TemporalGranularity>()
                .map_err(|error| MemcoreError::StorageError(error.to_string()))?;
            Some(
                TemporalValue::new(start, end, granularity, observed_at.unwrap_or(start))
                    .map_err(|error| MemcoreError::StorageError(error.to_string()))?,
            )
        }
        _ => None,
    };
    let source_message_ids = match columns.source_message_ids {
        Some(text) => serde_json::from_str::<Vec<String>>(&text).map_err(|error| {
            MemcoreError::StorageError(format!("invalid source_message_ids: {error}"))
        })?,
        None => Vec::new(),
    };
    Ok(FactStructure {
        subject: columns.subject,
        event_time,
        event_time_text: columns.event_time_text,
        observed_at,
        session_id: columns.session_id,
        source_message_ids,
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

#[cfg(test)]
mod datetime_round_trip_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn every_written_datetime_reads_back_including_signed_years() {
        let values = [
            Utc.with_ymd_and_hms(2024, 3, 12, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(-10, 3, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(-10, 3, 1, 23, 59, 59).unwrap(),
            Utc.with_ymd_and_hms(0, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(12_345, 6, 7, 8, 9, 10).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 23, 18, 36, 43).unwrap()
                + chrono::Duration::nanoseconds(263_275_676),
        ];
        for value in values {
            let written = datetime_to_str(value);
            assert_eq!(datetime_from_str(&written).unwrap(), value, "{written}");
        }
        // The exact text a store written by the old resolver holds.
        assert_eq!(
            datetime_from_str("-0010-03-01T00:00:00+00:00").unwrap(),
            Utc.with_ymd_and_hms(-10, 3, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn a_value_that_is_no_datetime_is_still_a_storage_error() {
        for value in ["", "yesterday", "2024-13-01T00:00:00+00:00", "-0010-03-01"] {
            let error = datetime_from_str(value).expect_err(value);
            assert!(matches!(error, MemcoreError::StorageError(_)), "{value}");
        }
    }
}
