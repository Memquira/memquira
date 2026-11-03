//! Helpers shared by the SQL ingest journal stores (`memcore_core::ports::
//! ingest_journal`).

use chrono::{DateTime, TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::IngestJournalStep;
use uuid::Uuid;

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

/// A rollback claimed the operation before the request could append to or commit it.
pub(crate) fn claimed_conflict() -> MemcoreError {
    MemcoreError::Conflict(
        "the write was rolled back by recovery before it completed; retry the request".to_string(),
    )
}

pub(crate) fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub(crate) fn datetime_from_ms(ms: i64) -> MemcoreResult<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .ok_or_else(|| storage_error("ingest journal time", format!("out of range: {ms}")))
}

pub(crate) fn step_payload(step: &IngestJournalStep) -> MemcoreResult<String> {
    serde_json::to_string(step).map_err(|error| storage_error("ingest journal step", error))
}

pub(crate) fn step_from_payload(payload: &str) -> MemcoreResult<IngestJournalStep> {
    serde_json::from_str(payload).map_err(|error| storage_error("ingest journal step", error))
}

/// The two fact-id columns of a step row (the in-flight lookup).
pub(crate) fn step_fact_columns(step: &IngestJournalStep) -> (Uuid, Option<Uuid>) {
    let ids = step.fact_ids();
    (ids[0], ids.get(1).copied())
}
