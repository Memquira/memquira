use std::future::Future;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memcore_common::{MemcoreError, MemcoreResult};

use crate::ShutdownToken;

use super::{BackgroundJobKind, BackgroundJobRun, BackgroundJobStatus};

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundJobRetryPolicy {
    pub enabled: bool,
    pub max_retries: usize,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f32,
    pub jitter_enabled: bool,
}

impl Default for BackgroundJobRetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 2,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_millis(5000),
            backoff_multiplier: 2.0,
            jitter_enabled: true,
        }
    }
}

impl BackgroundJobRetryPolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn total_attempts(&self) -> usize {
        if self.enabled {
            self.max_retries.saturating_add(1)
        } else {
            1
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundJobRetryState {
    pub attempt: usize,
    pub max_attempts: usize,
    pub next_backoff: Option<Duration>,
}

/// Exponential backoff for retry attempt `attempt` (1 = first retry delay).
pub fn calculate_background_job_backoff(
    attempt: usize,
    policy: &BackgroundJobRetryPolicy,
) -> Duration {
    let attempt = attempt.max(1) as i32;
    let base_ms = policy.initial_backoff.as_millis() as f64;
    let multiplier = f64::from(policy.backoff_multiplier.max(1.0));
    let max_ms = policy.max_backoff.as_millis() as f64;
    let exp_ms = (base_ms * multiplier.powi(attempt - 1)).min(max_ms);
    let mut delay_ms = exp_ms.max(1.0) as u64;

    if policy.jitter_enabled {
        let jitter_pct = simple_jitter_percent();
        delay_ms = delay_ms.saturating_add(delay_ms.saturating_mul(jitter_pct) / 100);
        delay_ms = delay_ms.min(policy.max_backoff.as_millis() as u64);
    }

    Duration::from_millis(delay_ms.max(1))
}

pub fn is_retryable_job_error(error: &MemcoreError) -> bool {
    match error {
        MemcoreError::Timeout(_)
        | MemcoreError::RateLimited
        | MemcoreError::Overloaded(_)
        | MemcoreError::ProviderThrottled { .. } => true,
        MemcoreError::StorageError(message) => is_retryable_job_error_message(message),
        MemcoreError::ProviderError(message) => is_retryable_job_error_message(message),
        MemcoreError::Internal(message) => is_retryable_job_error_message(message),
        MemcoreError::Unauthorized
        | MemcoreError::Forbidden
        | MemcoreError::BadRequest(_)
        | MemcoreError::NotFound(_)
        | MemcoreError::Conflict(_)
        | MemcoreError::MigrationError(_)
        | MemcoreError::ValidationError(_)
        | MemcoreError::QuotaExceeded { .. }
        | MemcoreError::ProviderGuardrailViolation { .. }
        | MemcoreError::ProviderOutputTruncated { .. }
        | MemcoreError::ProviderOutputInvalid { .. } => false,
    }
}

pub async fn execute_background_job_with_retries<F, Fut>(
    kind: BackgroundJobKind,
    policy: &BackgroundJobRetryPolicy,
    operation: F,
) -> BackgroundJobRun
where
    F: FnMut() -> Fut,
    Fut: Future<Output = MemcoreResult<BackgroundJobRun>>,
{
    execute_background_job_with_retries_and_shutdown(
        kind,
        policy,
        None,
        Duration::from_secs(30),
        operation,
    )
    .await
}

pub async fn execute_background_job_with_retries_and_shutdown<F, Fut>(
    kind: BackgroundJobKind,
    policy: &BackgroundJobRetryPolicy,
    shutdown_token: Option<ShutdownToken>,
    shutdown_timeout: Duration,
    mut operation: F,
) -> BackgroundJobRun
where
    F: FnMut() -> Fut,
    Fut: Future<Output = MemcoreResult<BackgroundJobRun>>,
{
    let max_attempts = policy.total_attempts();
    let mut attempts_made = 0usize;
    let mut final_error: Option<MemcoreError> = None;
    let started_at = chrono::Utc::now();
    let total_started = Instant::now();

    for attempt in 0..max_attempts {
        if shutdown_token
            .as_ref()
            .is_some_and(ShutdownToken::is_cancelled)
        {
            tracing::warn!(
                job_kind = %kind,
                attempt = attempts_made,
                max_attempts = max_attempts,
                status = BackgroundJobStatus::Cancelled.as_str(),
                error_code = "SHUTDOWN_REQUESTED",
                "background job cancelled before next attempt"
            );
            return cancelled_run(kind, started_at, total_started, attempts_made, max_attempts);
        }

        let attempt_number = attempt + 1;
        attempts_made = attempt_number;
        let attempt_started = Instant::now();
        tracing::info!(
            job_kind = %kind,
            attempt = attempt_number,
            max_attempts = max_attempts,
            "background job attempt started"
        );

        let operation_future = operation();
        tokio::pin!(operation_future);

        let attempt_result = match &shutdown_token {
            Some(token) => {
                tokio::select! {
                    result = &mut operation_future => result,
                    _ = token.cancelled() => {
                        tracing::warn!(
                            job_kind = %kind,
                            attempt = attempt_number,
                            max_attempts = max_attempts,
                            timeout_seconds = shutdown_timeout.as_secs(),
                            "background job shutdown requested during attempt"
                        );
                        match tokio::time::timeout(shutdown_timeout, &mut operation_future).await {
                            Ok(result) => result,
                            Err(_) => {
                                tracing::warn!(
                                    job_kind = %kind,
                                    attempt = attempt_number,
                                    max_attempts = max_attempts,
                                    timeout_seconds = shutdown_timeout.as_secs(),
                                    status = BackgroundJobStatus::Cancelled.as_str(),
                                    error_code = "SHUTDOWN_TIMEOUT",
                                    "background job shutdown timeout reached"
                                );
                                return cancelled_run(
                                    kind,
                                    started_at,
                                    total_started,
                                    attempts_made,
                                    max_attempts,
                                );
                            }
                        }
                    }
                }
            }
            None => operation_future.await,
        };

        match attempt_result {
            Ok(mut run) => {
                run.attempt_count = attempt_number;
                run.max_attempts = max_attempts;
                run.retried = attempt_number > 1;
                if run.status == BackgroundJobStatus::Succeeded {
                    tracing::info!(
                        job_kind = %kind,
                        attempt = attempt_number,
                        max_attempts = max_attempts,
                        status = run.status.as_str(),
                        duration_ms = run.duration_ms,
                        error_code = run.error_code.as_deref(),
                        "background job attempt succeeded"
                    );
                } else if run.status == BackgroundJobStatus::Failed {
                    tracing::warn!(
                        job_kind = %kind,
                        attempt = attempt_number,
                        max_attempts = max_attempts,
                        retryable = false,
                        status = run.status.as_str(),
                        duration_ms = run.duration_ms,
                        error_code = run.error_code.as_deref(),
                        "background job attempt failed"
                    );
                }
                return run;
            }
            Err(error) => {
                let retryable = is_retryable_job_error(&error);
                tracing::warn!(
                    job_kind = %kind,
                    attempt = attempt_number,
                    max_attempts = max_attempts,
                    retryable = retryable,
                    duration_ms = attempt_started.elapsed().as_millis(),
                    error_code = error.code(),
                    "background job attempt failed"
                );

                final_error = Some(error.clone());
                if !policy.enabled || !retryable {
                    tracing::warn!(
                        job_kind = %kind,
                        attempt = attempt_number,
                        max_attempts = max_attempts,
                        retryable = false,
                        error_code = error.code(),
                        "background job non-retryable failure"
                    );
                    break;
                }

                if attempt_number >= max_attempts {
                    tracing::warn!(
                        job_kind = %kind,
                        attempt = attempt_number,
                        max_attempts = max_attempts,
                        error_code = error.code(),
                        "background job retries exhausted"
                    );
                    break;
                }

                let retry_number = attempt_number;
                let backoff = calculate_background_job_backoff(retry_number, policy);
                tracing::info!(
                    job_kind = %kind,
                    attempt = attempt_number,
                    max_attempts = max_attempts,
                    backoff_ms = backoff.as_millis(),
                    error_code = error.code(),
                    "background job retry scheduled"
                );
                match &shutdown_token {
                    Some(token) => {
                        tokio::select! {
                            _ = tokio::time::sleep(backoff) => {}
                            _ = token.cancelled() => {
                                tracing::warn!(
                                    job_kind = %kind,
                                    attempt = attempt_number,
                                    max_attempts = max_attempts,
                                    backoff_ms = backoff.as_millis(),
                                    status = BackgroundJobStatus::Cancelled.as_str(),
                                    error_code = "SHUTDOWN_REQUESTED",
                                    "background job retry backoff interrupted by shutdown"
                                );
                                return cancelled_run(
                                    kind,
                                    started_at,
                                    total_started,
                                    attempts_made,
                                    max_attempts,
                                );
                            }
                        }
                    }
                    None => tokio::time::sleep(backoff).await,
                }
            }
        }
    }

    let error =
        final_error.unwrap_or_else(|| MemcoreError::Internal("background job failed".to_string()));
    let finished_at = chrono::Utc::now();
    let mut run = BackgroundJobRun {
        id: uuid::Uuid::new_v4(),
        kind,
        status: BackgroundJobStatus::Failed,
        started_at,
        finished_at: Some(finished_at),
        duration_ms: Some(total_started.elapsed().as_millis() as u64),
        attempt_count: attempts_made.max(1),
        max_attempts,
        retried: attempts_made > 1,
        error_code: Some(error.code().to_string()),
        error_message: Some(error.message()),
        org_count: 0,
        affected_count: 0,
    };
    if run.duration_ms.is_none() {
        run.duration_ms = Some((finished_at - started_at).num_milliseconds().max(0) as u64);
    }
    tracing::info!(
        job_kind = %kind,
        attempt = run.attempt_count,
        max_attempts = run.max_attempts,
        status = run.status.as_str(),
        duration_ms = run.duration_ms,
        error_code = run.error_code.as_deref(),
        "background job final result"
    );
    run
}

fn cancelled_run(
    kind: BackgroundJobKind,
    started_at: chrono::DateTime<chrono::Utc>,
    total_started: Instant,
    attempts_made: usize,
    max_attempts: usize,
) -> BackgroundJobRun {
    let finished_at = chrono::Utc::now();
    let mut run = BackgroundJobRun::cancelled(kind, "shutdown requested");
    run.started_at = started_at;
    run.finished_at = Some(finished_at);
    run.duration_ms = Some(total_started.elapsed().as_millis() as u64);
    run.attempt_count = attempts_made;
    run.max_attempts = max_attempts;
    run.retried = attempts_made > 1;
    tracing::info!(
        job_kind = %kind,
        attempt = run.attempt_count,
        max_attempts = run.max_attempts,
        status = run.status.as_str(),
        duration_ms = run.duration_ms,
        error_code = run.error_code.as_deref(),
        "background job final result"
    );
    run
}

fn is_retryable_job_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();

    if lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid config")
        || lower.contains("validation")
        || lower.contains("quota")
        || lower.contains("unsupported")
        || lower.contains("missing required")
    {
        return false;
    }

    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("database is locked")
        || lower.contains("deadlock")
        || lower.contains("lock unavailable")
        || lower.contains("unavailable")
        || contains_http_status(message, "429")
        || contains_http_status(message, "500")
        || contains_http_status(message, "502")
        || contains_http_status(message, "503")
        || contains_http_status(message, "504")
}

fn contains_http_status(message: &str, code: &str) -> bool {
    message.contains(&format!("({code})")) || message.contains(&format!("status {code}"))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn test_policy(max_retries: usize) -> BackgroundJobRetryPolicy {
        BackgroundJobRetryPolicy {
            enabled: true,
            max_retries,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(4),
            backoff_multiplier: 2.0,
            jitter_enabled: false,
        }
    }

    fn successful_run() -> BackgroundJobRun {
        BackgroundJobRun::running(BackgroundJobKind::MemoryUsageSnapshot)
            .finish(BackgroundJobStatus::Succeeded)
    }

    #[test]
    fn backoff_grows_caps_and_is_deterministic_without_jitter() {
        let policy = BackgroundJobRetryPolicy {
            enabled: true,
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(250),
            backoff_multiplier: 2.0,
            jitter_enabled: false,
        };

        assert_eq!(
            calculate_background_job_backoff(1, &policy),
            Duration::from_millis(100)
        );
        assert_eq!(
            calculate_background_job_backoff(2, &policy),
            Duration::from_millis(200)
        );
        assert_eq!(
            calculate_background_job_backoff(3, &policy),
            Duration::from_millis(250)
        );
        assert_eq!(
            calculate_background_job_backoff(3, &policy),
            Duration::from_millis(250)
        );
    }

    #[test]
    fn max_retries_zero_means_one_attempt() {
        assert_eq!(test_policy(0).total_attempts(), 1);
    }

    #[test]
    fn retryable_classification_is_conservative() {
        assert!(is_retryable_job_error(&MemcoreError::Timeout(
            "job timed out".to_string()
        )));
        assert!(is_retryable_job_error(&MemcoreError::StorageError(
            "database is locked".to_string()
        )));
        assert!(!is_retryable_job_error(&MemcoreError::ValidationError(
            "invalid job".to_string()
        )));
        assert!(!is_retryable_job_error(&MemcoreError::Unauthorized));
        assert!(!is_retryable_job_error(&MemcoreError::quota_exceeded(
            "quota",
            "DailyProviderRequests",
            1,
            1,
            1
        )));
    }

    #[tokio::test]
    async fn success_on_first_attempt_does_not_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let run = execute_background_job_with_retries(
            BackgroundJobKind::MemoryUsageSnapshot,
            &test_policy(2),
            || {
                let calls = calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(successful_run())
                }
            },
        )
        .await;

        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(run.attempt_count, 1);
        assert!(!run.retried);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retryable_failure_eventually_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let run = execute_background_job_with_retries(
            BackgroundJobKind::MemoryUsageSnapshot,
            &test_policy(2),
            || {
                let calls = calls_for_closure.clone();
                async move {
                    let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if attempt < 3 {
                        Err(MemcoreError::StorageError("database is locked".to_string()))
                    } else {
                        Ok(successful_run())
                    }
                }
            },
        )
        .await;

        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(run.attempt_count, 3);
        assert!(run.retried);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retryable_failure_exhausts_retries_and_fails() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let run = execute_background_job_with_retries(
            BackgroundJobKind::MemoryUsageSnapshot,
            &test_policy(2),
            || {
                let calls = calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(MemcoreError::StorageError(
                        "service unavailable".to_string(),
                    ))
                }
            },
        )
        .await;

        assert_eq!(run.status, BackgroundJobStatus::Failed);
        assert_eq!(run.attempt_count, 3);
        assert!(run.retried);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_retryable_failure_does_not_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let run = execute_background_job_with_retries(
            BackgroundJobKind::MemoryUsageSnapshot,
            &test_policy(2),
            || {
                let calls = calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(MemcoreError::ValidationError("bad config".to_string()))
                }
            },
        )
        .await;

        assert_eq!(run.status, BackgroundJobStatus::Failed);
        assert_eq!(run.attempt_count, 1);
        assert!(!run.retried);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn disabled_retry_policy_performs_one_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();
        let policy = BackgroundJobRetryPolicy {
            enabled: false,
            ..test_policy(2)
        };
        let run = execute_background_job_with_retries(
            BackgroundJobKind::MemoryUsageSnapshot,
            &policy,
            || {
                let calls = calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(MemcoreError::StorageError(
                        "service unavailable".to_string(),
                    ))
                }
            },
        )
        .await;

        assert_eq!(run.status, BackgroundJobStatus::Failed);
        assert_eq!(run.attempt_count, 1);
        assert!(!run.retried);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_before_first_attempt_prevents_execution() {
        let token = ShutdownToken::new();
        token.cancel();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let run = execute_background_job_with_retries_and_shutdown(
            BackgroundJobKind::MemoryUsageSnapshot,
            &test_policy(2),
            Some(token),
            Duration::from_millis(10),
            || {
                let calls = calls_for_closure.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(successful_run())
                }
            },
        )
        .await;

        assert_eq!(run.status, BackgroundJobStatus::Cancelled);
        assert_eq!(run.attempt_count, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn shutdown_during_backoff_stops_retries() {
        let token = ShutdownToken::new();
        let child = token.child_token();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let handle = tokio::spawn(async move {
            execute_background_job_with_retries_and_shutdown(
                BackgroundJobKind::MemoryUsageSnapshot,
                &BackgroundJobRetryPolicy {
                    initial_backoff: Duration::from_secs(60),
                    max_backoff: Duration::from_secs(60),
                    jitter_enabled: false,
                    ..test_policy(2)
                },
                Some(child),
                Duration::from_millis(10),
                || {
                    let calls = calls_for_closure.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err(MemcoreError::StorageError(
                            "service unavailable".to_string(),
                        ))
                    }
                },
            )
            .await
        });

        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        token.cancel();

        let run = tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .expect("shutdown should interrupt backoff")
            .expect("retry task should complete");
        assert_eq!(run.status, BackgroundJobStatus::Cancelled);
        assert_eq!(run.attempt_count, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_during_attempt_waits_for_completion_within_timeout() {
        let token = ShutdownToken::new();
        let child = token.child_token();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let handle = tokio::spawn(async move {
            execute_background_job_with_retries_and_shutdown(
                BackgroundJobKind::MemoryUsageSnapshot,
                &test_policy(2),
                Some(child),
                Duration::from_millis(100),
                || {
                    let calls = calls_for_closure.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        Ok(successful_run())
                    }
                },
            )
            .await
        });

        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        token.cancel();

        let run = handle.await.expect("retry task should complete");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(run.attempt_count, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
