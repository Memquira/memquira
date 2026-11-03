use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memcore_common::MemcoreError;

use super::ProviderExecutionPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRetryDecision {
    Retry,
    DoNotRetry,
}

pub fn retry_decision_for(error: &MemcoreError) -> ProviderRetryDecision {
    if is_retryable_provider_error(error) {
        ProviderRetryDecision::Retry
    } else {
        ProviderRetryDecision::DoNotRetry
    }
}

pub fn is_retryable_provider_error(error: &MemcoreError) -> bool {
    match error {
        MemcoreError::RateLimited => true,
        MemcoreError::ProviderThrottled { .. } => true,
        MemcoreError::Timeout(_) => true,
        MemcoreError::ProviderError(message) => is_retryable_provider_message(message),
        MemcoreError::Unauthorized | MemcoreError::Forbidden => false,
        MemcoreError::ValidationError(_)
        | MemcoreError::BadRequest(_)
        | MemcoreError::NotFound(_)
        | MemcoreError::Conflict(_) => false,
        MemcoreError::StorageError(_)
        | MemcoreError::MigrationError(_)
        | MemcoreError::Internal(_)
        | MemcoreError::QuotaExceeded { .. }
        | MemcoreError::ProviderGuardrailViolation { .. } => false,
        // Retrying with the same limit reproduces the same truncated output.
        MemcoreError::ProviderOutputTruncated { .. } => false,
        // A contract violation in the model output is not an outage; not retried.
        MemcoreError::ProviderOutputInvalid { .. } => false,
        // Capacity waits are bounded by the caller's deadline; retrying would extend it.
        MemcoreError::Overloaded(_) => false,
    }
}

/// Retry-exhausted provider failures that should count toward circuit breaker health.
pub fn is_provider_health_failure(error: &MemcoreError) -> bool {
    if error.is_provider_circuit_open() {
        return false;
    }
    is_retryable_provider_error(error)
}

fn is_retryable_provider_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();

    if lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || lower.contains("api key is unauthorized")
    {
        return false;
    }

    if lower.contains("invalid json")
        || lower.contains("malformed")
        || lower.contains("dimension mismatch")
        || lower.contains("validation")
        || lower.contains("unsupported model")
    {
        return false;
    }

    if contains_http_status(message, "400")
        || contains_http_status(message, "401")
        || contains_http_status(message, "403")
        || contains_http_status(message, "422")
    {
        return false;
    }

    if lower.contains("timed out")
        || lower.contains("http request failed")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
    {
        return true;
    }

    if contains_http_status(message, "429")
        || contains_http_status(message, "500")
        || contains_http_status(message, "502")
        || contains_http_status(message, "503")
        || contains_http_status(message, "504")
    {
        return true;
    }

    false
}

fn contains_http_status(message: &str, code: &str) -> bool {
    message.contains(&format!("({code})")) || message.contains(&format!("status {code}"))
}

/// Exponential backoff for retry attempt `retry_number` (1 = first retry delay).
pub fn backoff_duration(policy: &ProviderExecutionPolicy, retry_number: usize) -> Duration {
    let retry_number = retry_number.max(1) as i32;
    let base_ms = policy.initial_backoff.as_millis() as f64;
    let multiplier = f64::from(policy.backoff_multiplier.max(1.0));
    let max_ms = policy.max_backoff.as_millis() as f64;
    let exp_ms = (base_ms * multiplier.powi(retry_number - 1)).min(max_ms);
    let mut delay_ms = exp_ms.max(1.0) as u64;

    if policy.jitter_enabled {
        let jitter_pct = simple_jitter_percent();
        delay_ms = delay_ms.saturating_add(delay_ms.saturating_mul(jitter_pct) / 100);
        delay_ms = delay_ms.min(policy.max_backoff.as_millis() as u64);
    }

    Duration::from_millis(delay_ms.max(1))
}

fn simple_jitter_percent() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    u64::from(nanos % 26)
}

#[cfg(test)]
mod tests {
    use memcore_common::MemcoreError;

    use super::*;

    #[test]
    fn retryable_errors_include_timeout_rate_limit_and_5xx() {
        assert!(is_retryable_provider_error(&MemcoreError::RateLimited));
        assert!(is_retryable_provider_error(
            &MemcoreError::provider_timeout()
        ));
        assert!(is_retryable_provider_error(&MemcoreError::ProviderError(
            "OpenAI API error (503): service unavailable".to_string()
        )));
        assert!(is_retryable_provider_error(&MemcoreError::ProviderError(
            "OpenAI HTTP request failed: connection reset".to_string()
        )));
    }

    #[test]
    fn non_retryable_errors_include_auth_and_validation() {
        assert!(!is_retryable_provider_error(&MemcoreError::Unauthorized));
        assert!(!is_retryable_provider_error(&MemcoreError::Forbidden));
        assert!(!is_retryable_provider_error(
            &MemcoreError::ValidationError("bad input".to_string())
        ));
        assert!(!is_retryable_provider_error(&MemcoreError::ProviderError(
            "OpenAI API key is unauthorized".to_string()
        )));
        assert!(!is_retryable_provider_error(&MemcoreError::ProviderError(
            "OpenAI API error (400): invalid request".to_string()
        )));
    }

    #[test]
    fn backoff_respects_max_and_jitter_toggle() {
        let policy = ProviderExecutionPolicy {
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_millis(500),
            backoff_multiplier: 4.0,
            jitter_enabled: false,
            ..ProviderExecutionPolicy::default()
        };
        assert_eq!(backoff_duration(&policy, 1), Duration::from_millis(250));
        assert_eq!(backoff_duration(&policy, 2), Duration::from_millis(500));
        assert_eq!(backoff_duration(&policy, 3), Duration::from_millis(500));
    }
}
