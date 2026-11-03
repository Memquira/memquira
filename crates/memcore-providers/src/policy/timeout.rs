use memcore_common::MemcoreError;

/// Safe provider timeout error without prompts, secrets, or request bodies.
pub fn provider_timeout_error() -> MemcoreError {
    MemcoreError::provider_timeout()
}
