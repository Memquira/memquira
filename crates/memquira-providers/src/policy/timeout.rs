use memquira_common::MemquiraError;

/// Safe provider timeout error without prompts, secrets, or request bodies.
pub fn provider_timeout_error() -> MemquiraError {
    MemquiraError::provider_timeout()
}
