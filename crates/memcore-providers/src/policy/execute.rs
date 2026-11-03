use std::future::Future;
use std::time::{Duration, Instant};

use memcore_common::{MemcoreError, MemcoreResult};

use super::ProviderExecutionPolicy;
use super::retry::{ProviderRetryDecision, backoff_duration, retry_decision_for};
use super::timeout::provider_timeout_error;

/// Longest provider-requested wait (Retry-After) memcore honours before a retry.
/// A longer Retry-After ends the retries instead of holding the caller.
pub const MAX_PROVIDER_RETRY_AFTER: Duration = Duration::from_secs(30);

/// The wait before retry `retry_number`, or `None` when retrying must stop: the provider asked for
/// more than [`MAX_PROVIDER_RETRY_AFTER`], or the wait would end at or past the request deadline
/// . Without a usable Retry-After the policy's backoff applies.
pub fn next_retry_delay(
    policy: &ProviderExecutionPolicy,
    retry_number: usize,
    error: &MemcoreError,
) -> Option<Duration> {
    let delay = match error {
        MemcoreError::ProviderThrottled {
            retry_after_ms: Some(ms),
            ..
        } => {
            let wanted = Duration::from_millis(*ms);
            if wanted > MAX_PROVIDER_RETRY_AFTER {
                return None;
            }
            wanted
        }
        _ => backoff_duration(policy, retry_number),
    };
    if let Some(deadline) = memcore_core::deadline::current_deadline()
        && Instant::now() + delay >= deadline
    {
        return None;
    }
    Some(delay)
}

/// Result of a provider call including retry attempt metadata.
#[derive(Debug, Clone)]
pub struct ProviderExecutionOutcome<T> {
    pub value: T,
    pub attempts: usize,
    pub retries: usize,
    pub timed_out: bool,
}

/// Provider call failure with retry attempt metadata.
#[derive(Debug, Clone)]
pub struct ProviderExecutionFailure {
    pub error: MemcoreError,
    pub attempts: usize,
    pub retries: usize,
    pub timed_out: bool,
}

pub async fn execute_provider_call<F, Fut, T>(
    operation_name: &'static str,
    policy: &ProviderExecutionPolicy,
    mut operation: F,
) -> Result<ProviderExecutionOutcome<T>, ProviderExecutionFailure>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = MemcoreResult<T>>,
{
    let max_attempts = policy.total_attempts();
    let timeout_ms = policy.timeout.as_millis();
    let mut last_error: Option<MemcoreError> = None;
    let mut any_timed_out = false;
    let mut attempts_made = 0_usize;

    for attempt in 0..max_attempts {
        let attempt_number = attempt + 1;
        attempts_made = attempt_number;
        let started = Instant::now();

        match tokio::time::timeout(policy.timeout, operation()).await {
            Ok(Ok(result)) => {
                tracing::debug!(
                    operation_name = operation_name,
                    attempt_number = attempt_number,
                    max_attempts = max_attempts,
                    timeout_ms = timeout_ms,
                    success = true,
                    duration_ms = started.elapsed().as_millis(),
                    "provider call succeeded"
                );
                return Ok(ProviderExecutionOutcome {
                    value: result,
                    attempts: attempt_number,
                    retries: attempt,
                    timed_out: any_timed_out,
                });
            }
            Ok(Err(error)) => {
                let retryable = matches!(retry_decision_for(&error), ProviderRetryDecision::Retry);
                tracing::warn!(
                    operation_name = operation_name,
                    attempt_number = attempt_number,
                    max_attempts = max_attempts,
                    timeout_ms = timeout_ms,
                    retryable = retryable,
                    success = false,
                    error_code = error.code(),
                    duration_ms = started.elapsed().as_millis(),
                    "provider call failed"
                );
                if !retryable || attempt == policy.max_retries {
                    last_error = Some(error);
                    break;
                }

                let retry_number = attempt + 1;
                let Some(delay) = next_retry_delay(policy, retry_number, &error) else {
                    last_error = Some(error);
                    break;
                };
                last_error = Some(error);
                tokio::time::sleep(delay).await;
            }
            Err(_elapsed) => {
                any_timed_out = true;
                let error = provider_timeout_error();
                let retryable = matches!(retry_decision_for(&error), ProviderRetryDecision::Retry);
                tracing::warn!(
                    operation_name = operation_name,
                    attempt_number = attempt_number,
                    max_attempts = max_attempts,
                    timeout_ms = timeout_ms,
                    retryable = retryable,
                    success = false,
                    error_code = error.code(),
                    duration_ms = started.elapsed().as_millis(),
                    "provider call timed out"
                );
                if !retryable || attempt == policy.max_retries {
                    last_error = Some(error);
                    break;
                }

                let retry_number = attempt + 1;
                let Some(delay) = next_retry_delay(policy, retry_number, &error) else {
                    last_error = Some(error);
                    break;
                };
                last_error = Some(error);
                tokio::time::sleep(delay).await;
            }
        }
    }

    match last_error {
        Some(error) if error.is_provider_timeout() => Err(ProviderExecutionFailure {
            error,
            attempts: attempts_made,
            retries: attempts_made.saturating_sub(1),
            timed_out: true,
        }),
        // Upstream throttling keeps its type (503 PROVIDER_THROTTLED), even after retries.
        Some(error @ MemcoreError::ProviderThrottled { .. }) => Err(ProviderExecutionFailure {
            error,
            attempts: attempts_made,
            retries: attempts_made.saturating_sub(1),
            timed_out: any_timed_out,
        }),
        Some(error) if !super::retry::is_retryable_provider_error(&error) => {
            Err(ProviderExecutionFailure {
                error,
                attempts: attempts_made,
                retries: attempts_made.saturating_sub(1),
                timed_out: any_timed_out,
            })
        }
        Some(error) => Err(ProviderExecutionFailure {
            error: MemcoreError::ProviderError(format!(
                "{operation_name} failed after {max_attempts} attempts: {}",
                error.message()
            )),
            attempts: attempts_made,
            retries: attempts_made.saturating_sub(1),
            timed_out: any_timed_out,
        }),
        None => Err(ProviderExecutionFailure {
            error: MemcoreError::Internal(format!("{operation_name} failed without an error")),
            attempts: attempts_made,
            retries: attempts_made.saturating_sub(1),
            timed_out: any_timed_out,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use memcore_common::MemcoreError;

    use super::*;
    use crate::policy::ProviderExecutionPolicy;

    fn test_policy(max_retries: usize) -> ProviderExecutionPolicy {
        ProviderExecutionPolicy {
            max_retries,
            timeout: Duration::from_millis(200),
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            jitter_enabled: false,
            ..ProviderExecutionPolicy::default()
        }
    }

    #[tokio::test]
    async fn successful_operation_returns_immediately() {
        let policy = test_policy(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let result = execute_provider_call("test_success", &policy, || {
            let calls = calls_for_closure.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<i32, MemcoreError>(42)
            }
        })
        .await
        .expect("success");

        assert_eq!(result.value, 42);
        assert_eq!(result.retries, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_retryable_error_is_not_retried() {
        let policy = test_policy(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let failure = execute_provider_call("test_non_retryable", &policy, || {
            let calls = calls_for_closure.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(MemcoreError::ValidationError("invalid payload".to_string()))
            }
        })
        .await
        .expect_err("should fail");

        assert!(matches!(failure.error, MemcoreError::ValidationError(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retryable_error_is_retried_until_success() {
        let policy = test_policy(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let result = execute_provider_call("test_retry_success", &policy, || {
            let calls = calls_for_closure.clone();
            async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt < 3 {
                    Err::<&str, _>(MemcoreError::ProviderError(
                        "OpenAI API error (503): unavailable".to_string(),
                    ))
                } else {
                    Ok("ok")
                }
            }
        })
        .await
        .expect("should eventually succeed");

        assert_eq!(result.value, "ok");
        assert_eq!(result.retries, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retries_exhausted_returns_final_error() {
        let policy = test_policy(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let failure = execute_provider_call("test_retry_exhausted", &policy, || {
            let calls = calls_for_closure.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(MemcoreError::ProviderError(
                    "OpenAI API error (500): internal".to_string(),
                ))
            }
        })
        .await
        .expect_err("should fail");

        assert!(matches!(failure.error, MemcoreError::ProviderError(_)));
        assert_eq!(failure.retries, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn max_retries_zero_performs_only_one_attempt() {
        let policy = test_policy(0);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let _ = execute_provider_call("test_no_retry", &policy, || {
            let calls = calls_for_closure.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(MemcoreError::RateLimited)
            }
        })
        .await
        .expect_err("should fail");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn timeout_returns_provider_timeout_error() {
        let policy = ProviderExecutionPolicy {
            timeout: Duration::from_millis(20),
            max_retries: 0,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
            jitter_enabled: false,
            ..ProviderExecutionPolicy::default()
        };

        let failure = execute_provider_call("test_timeout", &policy, || async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        })
        .await
        .expect_err("should time out");

        assert!(failure.error.is_provider_timeout());
        assert_eq!(failure.error.code(), "provider_timeout");
        assert!(failure.timed_out);
    }

    #[tokio::test]
    async fn timeout_is_retried_when_policy_allows() {
        let policy = ProviderExecutionPolicy {
            timeout: Duration::from_millis(30),
            max_retries: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
            jitter_enabled: false,
            ..ProviderExecutionPolicy::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let result = execute_provider_call("test_timeout_retry", &policy, || {
            let calls = calls_for_closure.clone();
            async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 1 {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    Ok("late")
                } else {
                    Ok("fast")
                }
            }
        })
        .await
        .expect("second attempt should succeed");

        assert_eq!(result.value, "fast");
        assert_eq!(result.retries, 1);
        assert!(result.timed_out);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
