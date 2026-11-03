use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{OrgPlanConfig, OrgPlanTier};
use serde_json::Value;

pub(crate) fn storage_error(
    context: impl Into<String>,
    error: impl std::fmt::Display,
) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

pub(crate) fn tier_to_storage(value: OrgPlanTier) -> &'static str {
    value.storage_label()
}

pub(crate) fn tier_from_storage(value: &str) -> MemcoreResult<OrgPlanTier> {
    value.parse::<OrgPlanTier>().map_err(|error| {
        MemcoreError::StorageError(format!("invalid org plan tier value '{value}': {error}"))
    })
}

pub(crate) fn optional_metadata_to_str(value: &Option<Value>) -> MemcoreResult<Option<String>> {
    match value {
        Some(metadata) => {
            Ok(Some(serde_json::to_string(metadata).map_err(|error| {
                storage_error("serialize org plan metadata", error)
            })?))
        }
        None => Ok(None),
    }
}

pub(crate) fn optional_metadata_from_str(value: Option<String>) -> MemcoreResult<Option<Value>> {
    match value {
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => Ok(Some(serde_json::from_str(&raw).map_err(|error| {
            storage_error("deserialize org plan metadata", error)
        })?)),
        None => Ok(None),
    }
}

pub(crate) fn validate_plan_for_storage(plan: &OrgPlanConfig) -> MemcoreResult<()> {
    plan.validate()
}
