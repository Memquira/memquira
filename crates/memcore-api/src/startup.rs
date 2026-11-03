//! Server startup safety: wait (bounded) for Qdrant / Postgres before building the application
//! state, so starting memcore before its dependencies does not fail permanently.
//!
//! Only reachability is retried. A dependency that answers with a fatal error (invalid URL,
//! authentication, missing database) stops startup at once, and so does any configuration,
//! migration or schema error raised later while the state is built.

use std::future::Future;
use std::time::Duration;

use memcore_common::{MemcoreError, MemcoreResult};
use memcore_config::Settings;
#[cfg(feature = "qdrant")]
use memcore_config::VectorBackend;
use memcore_storage::ProbeOutcome;

/// Upper bound for one probe attempt.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// A dependency the server needs before it can build its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    Qdrant(String),
    Postgres(String),
}

impl Dependency {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Qdrant(_) => "Qdrant",
            Self::Postgres(_) => "Postgres",
        }
    }
}

/// The network dependencies the configured backends need (embedded SQLite needs none).
pub fn startup_dependencies(settings: &Settings) -> Vec<Dependency> {
    #[allow(unused_mut)]
    let mut dependencies = Vec::new();
    #[cfg(feature = "qdrant")]
    if settings.vector_backend == VectorBackend::Qdrant && !settings.qdrant_url.trim().is_empty() {
        dependencies.push(Dependency::Qdrant(settings.qdrant_url.trim().to_string()));
    }
    #[cfg(feature = "postgres")]
    {
        use memcore_config::{EventBackend, FactBackend};
        let uses_postgres = settings.fact_backend == FactBackend::Postgres
            || settings.event_backend == EventBackend::Postgres;
        if let Some(url) = settings
            .postgres_url
            .as_ref()
            .filter(|url| !url.trim().is_empty())
            && uses_postgres
        {
            dependencies.push(Dependency::Postgres(url.trim().to_string()));
        }
    }
    let _ = settings;
    dependencies
}

/// One probe of one dependency, bounded by [`PROBE_TIMEOUT`].
pub async fn probe_dependency(dependency: &Dependency) -> ProbeOutcome {
    let probe = async {
        match dependency {
            #[cfg(feature = "qdrant")]
            Dependency::Qdrant(url) => memcore_storage::probe_qdrant(url, PROBE_TIMEOUT).await,
            #[cfg(feature = "postgres")]
            Dependency::Postgres(url) => memcore_storage::probe_postgres(url).await,
            #[allow(unreachable_patterns)]
            other => ProbeOutcome::Fatal(format!(
                "{} support is not compiled into this binary",
                other.name()
            )),
        }
    };
    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(outcome) => outcome,
        Err(_) => ProbeOutcome::Unavailable(format!("{} probe timed out", dependency.name())),
    }
}

/// Waits until every configured dependency answers. Gives up once
/// `MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS` has passed; the worst case is that deadline plus one
/// probe attempt ([`PROBE_TIMEOUT`]).
pub async fn wait_for_dependencies(settings: &Settings) -> MemcoreResult<()> {
    let dependencies = startup_dependencies(settings);
    let deadline = Duration::from_secs(settings.startup_dependency_timeout_seconds);
    wait_until_ready(&dependencies, deadline, |dependency| {
        let dependency = dependency.clone();
        async move { probe_dependency(&dependency).await }
    })
    .await
}

/// The retry loop, generic over the probe so it can be tested without real services. Backoff starts
/// at 500 ms and doubles up to 5 s; the loop never sleeps past the deadline.
pub async fn wait_until_ready<P, F>(
    dependencies: &[Dependency],
    deadline: Duration,
    mut probe: P,
) -> MemcoreResult<()>
where
    P: FnMut(&Dependency) -> F,
    F: Future<Output = ProbeOutcome>,
{
    let started = tokio::time::Instant::now();
    let mut backoff = INITIAL_BACKOFF;
    let mut pending: Vec<&Dependency> = dependencies.iter().collect();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut still_pending = Vec::new();
        let mut last_reason = String::new();
        for dependency in pending {
            match probe(dependency).await {
                ProbeOutcome::Ready => {
                    if attempt > 1 {
                        tracing::info!(
                            dependency = dependency.name(),
                            attempt,
                            "startup dependency became reachable"
                        );
                    }
                }
                ProbeOutcome::Fatal(reason) => {
                    return Err(MemcoreError::ValidationError(format!(
                        "startup dependency {} failed: {reason}",
                        dependency.name()
                    )));
                }
                ProbeOutcome::Unavailable(reason) => {
                    last_reason = reason;
                    still_pending.push(dependency);
                }
            }
        }
        if still_pending.is_empty() {
            return Ok(());
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            let names: Vec<&str> = still_pending.iter().map(|d| d.name()).collect();
            return Err(MemcoreError::ValidationError(format!(
                "startup dependency {} not reachable after {}s \
                 (MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS={}): {last_reason}",
                names.join(" and "),
                elapsed.as_secs(),
                deadline.as_secs()
            )));
        }
        tracing::warn!(
            dependencies = ?still_pending.iter().map(|d| d.name()).collect::<Vec<_>>(),
            attempt,
            retry_in_ms = backoff.as_millis() as u64,
            reason = %last_reason,
            "waiting for startup dependencies"
        );
        tokio::time::sleep(backoff.min(deadline - elapsed)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
        pending = still_pending;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn qdrant() -> Vec<Dependency> {
        vec![Dependency::Qdrant("http://127.0.0.1:6334".to_string())]
    }

    #[tokio::test(start_paused = true)]
    async fn waits_until_a_dependency_comes_online() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let result = wait_until_ready(&qdrant(), Duration::from_secs(60), move |_| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 3 {
                    ProbeOutcome::Unavailable("connection refused".to_string())
                } else {
                    ProbeOutcome::Ready
                }
            }
        })
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_the_deadline_with_a_clear_error() {
        let started = tokio::time::Instant::now();
        let result = wait_until_ready(&qdrant(), Duration::from_secs(10), |_| async {
            ProbeOutcome::Unavailable("connection refused".to_string())
        })
        .await;
        let message = result.expect_err("must stop at the deadline").to_string();
        assert!(
            message.contains("Qdrant not reachable after 10s"),
            "{message}"
        );
        assert!(message.contains("MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS=10"));
        assert!(message.contains("connection refused"));
        assert!(started.elapsed() <= Duration::from_secs(11), "bounded wait");
    }

    #[tokio::test(start_paused = true)]
    async fn fatal_errors_are_not_retried() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let result = wait_until_ready(&qdrant(), Duration::from_secs(60), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { ProbeOutcome::Fatal("authentication failed".to_string()) }
        })
        .await;
        assert!(
            result
                .expect_err("fatal")
                .to_string()
                .contains("authentication failed")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_timeout_makes_a_single_attempt() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let result = wait_until_ready(&qdrant(), Duration::ZERO, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { ProbeOutcome::Unavailable("connection refused".to_string()) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_network_dependencies_for_embedded_mock_settings() {
        assert!(startup_dependencies(&Settings::default()).is_empty());
        assert!(wait_for_dependencies(&Settings::default()).await.is_ok());
    }
}
