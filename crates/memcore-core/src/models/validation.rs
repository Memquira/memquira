use memcore_common::{MemcoreError, MemcoreResult};

pub(crate) fn validate_non_empty(field: &str, value: &str) -> MemcoreResult<()> {
    if value.trim().is_empty() {
        return Err(MemcoreError::ValidationError(format!(
            "{field} cannot be empty"
        )));
    }
    Ok(())
}

pub(crate) fn validate_unit_interval(field: &str, value: f32) -> MemcoreResult<()> {
    if !(0.0..=1.0).contains(&value) {
        return Err(MemcoreError::ValidationError(format!(
            "{field} must be between 0.0 and 1.0"
        )));
    }
    Ok(())
}
