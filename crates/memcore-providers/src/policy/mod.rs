mod execute;
mod retry;
mod timeout;

pub use execute::{ProviderExecutionFailure, ProviderExecutionOutcome, execute_provider_call};
pub use retry::{
    ProviderRetryDecision, backoff_duration, is_provider_health_failure,
    is_retryable_provider_error, retry_decision_for,
};
pub use timeout::provider_timeout_error;

use std::time::Duration;

use memcore_common::MemcoreResult;

/// Timeout and bounded retry policy for external provider calls.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderExecutionPolicy {
    pub timeout: Duration,
    pub max_retries: usize,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f32,
    pub jitter_enabled: bool,
}

impl Default for ProviderExecutionPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_retries: 2,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_millis(2000),
            backoff_multiplier: 2.0,
            jitter_enabled: true,
        }
    }
}

impl ProviderExecutionPolicy {
    /// Fast, deterministic policy for unit tests.
    pub fn for_tests() -> Self {
        Self {
            timeout: Duration::from_millis(200),
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            backoff_multiplier: 2.0,
            jitter_enabled: false,
        }
    }

    pub fn from_config(
        timeout_seconds: u64,
        max_retries: usize,
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
        backoff_multiplier: f32,
        jitter_enabled: bool,
    ) -> MemcoreResult<Self> {
        validate_provider_execution_config(
            timeout_seconds,
            initial_backoff_ms,
            max_backoff_ms,
            backoff_multiplier,
        )?;

        Ok(Self {
            timeout: Duration::from_secs(timeout_seconds),
            max_retries,
            initial_backoff: Duration::from_millis(initial_backoff_ms),
            max_backoff: Duration::from_millis(max_backoff_ms),
            backoff_multiplier,
            jitter_enabled,
        })
    }

    pub fn total_attempts(&self) -> usize {
        self.max_retries.saturating_add(1)
    }
}

pub fn validate_provider_execution_config(
    timeout_seconds: u64,
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
    backoff_multiplier: f32,
) -> MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if timeout_seconds == 0 {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_TIMEOUT_SECONDS must be greater than 0".to_string(),
        ));
    }
    if initial_backoff_ms == 0 {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_INITIAL_BACKOFF_MS must be greater than 0".to_string(),
        ));
    }
    if max_backoff_ms < initial_backoff_ms {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_MAX_BACKOFF_MS must be >= MEMCORE_PROVIDER_INITIAL_BACKOFF_MS"
                .to_string(),
        ));
    }
    if backoff_multiplier < 1.0 {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_BACKOFF_MULTIPLIER must be >= 1.0".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_matches_expected_values() {
        let policy = ProviderExecutionPolicy::default();
        assert_eq!(policy.timeout, Duration::from_secs(30));
        assert_eq!(policy.max_retries, 2);
        assert_eq!(policy.initial_backoff, Duration::from_millis(250));
        assert_eq!(policy.max_backoff, Duration::from_millis(2000));
        assert!((policy.backoff_multiplier - 2.0).abs() < f32::EPSILON);
        assert!(policy.jitter_enabled);
        assert_eq!(policy.total_attempts(), 3);
    }

    #[test]
    fn max_retries_zero_allows_single_attempt() {
        let policy = ProviderExecutionPolicy {
            max_retries: 0,
            ..ProviderExecutionPolicy::default()
        };
        assert_eq!(policy.total_attempts(), 1);
    }

    #[test]
    fn invalid_config_values_fail_validation() {
        assert!(validate_provider_execution_config(0, 250, 2000, 2.0).is_err());
        assert!(validate_provider_execution_config(30, 0, 2000, 2.0).is_err());
        assert!(validate_provider_execution_config(30, 500, 200, 2.0).is_err());
        assert!(validate_provider_execution_config(30, 250, 2000, 0.5).is_err());
    }
}
