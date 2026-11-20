use memquira_common::{MemquiraError, MemquiraResult};

pub(crate) fn validate_non_empty(field: &str, value: &str) -> MemquiraResult<()> {
    if value.trim().is_empty() {
        return Err(MemquiraError::ValidationError(format!(
            "{field} cannot be empty"
        )));
    }
    Ok(())
}

pub(crate) fn validate_unit_interval(field: &str, value: f32) -> MemquiraResult<()> {
    if !(0.0..=1.0).contains(&value) {
        return Err(MemquiraError::ValidationError(format!(
            "{field} must be between 0.0 and 1.0"
        )));
    }
    Ok(())
}
