use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::cache::ContextCacheConfig;

/// Aggregate process-local context cache counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextCacheMetricsSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub sets: u64,
    pub invalidations: u64,
    pub invalidated_entries: u64,
    pub stale_served: u64,
    pub refresh_started: u64,
    pub refresh_succeeded: u64,
    pub refresh_failed: u64,
    /// Background refreshes not started because the per-process refresh budget was full;
    /// the stale entry was served as usual.
    #[serde(default)]
    pub refresh_skipped: u64,
    pub stampede_waits: u64,
    pub stampede_timeouts: u64,
    pub compute_errors: u64,
}

/// Records aggregate context cache observability counters (no per-key or tenant breakdown).
pub trait ContextCacheMetricsRecorder: Send + Sync {
    fn record_hit(&self);
    fn record_miss(&self);
    fn record_set(&self);
    fn record_invalidation(&self, entries: usize);
    fn record_stale_served(&self);
    fn record_refresh_started(&self);
    fn record_refresh_succeeded(&self);
    fn record_refresh_failed(&self);
    /// A background refresh skipped because the refresh budget was full.
    fn record_refresh_skipped(&self) {}
    fn record_stampede_wait(&self);
    fn record_stampede_timeout(&self);
    fn record_compute_error(&self);
    fn snapshot(&self) -> ContextCacheMetricsSnapshot;
}

/// Thread-safe in-process metrics using atomic counters.
#[derive(Debug, Default)]
pub struct InMemoryContextCacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    sets: AtomicU64,
    invalidations: AtomicU64,
    invalidated_entries: AtomicU64,
    stale_served: AtomicU64,
    refresh_skipped: AtomicU64,
    refresh_started: AtomicU64,
    refresh_succeeded: AtomicU64,
    refresh_failed: AtomicU64,
    stampede_waits: AtomicU64,
    stampede_timeouts: AtomicU64,
    compute_errors: AtomicU64,
}

impl InMemoryContextCacheMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    fn load_snapshot(&self) -> ContextCacheMetricsSnapshot {
        ContextCacheMetricsSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            sets: self.sets.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
            invalidated_entries: self.invalidated_entries.load(Ordering::Relaxed),
            stale_served: self.stale_served.load(Ordering::Relaxed),
            refresh_started: self.refresh_started.load(Ordering::Relaxed),
            refresh_succeeded: self.refresh_succeeded.load(Ordering::Relaxed),
            refresh_failed: self.refresh_failed.load(Ordering::Relaxed),
            refresh_skipped: self.refresh_skipped.load(Ordering::Relaxed),
            stampede_waits: self.stampede_waits.load(Ordering::Relaxed),
            stampede_timeouts: self.stampede_timeouts.load(Ordering::Relaxed),
            compute_errors: self.compute_errors.load(Ordering::Relaxed),
        }
    }
}

impl ContextCacheMetricsRecorder for InMemoryContextCacheMetrics {
    fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    fn record_set(&self) {
        self.sets.fetch_add(1, Ordering::Relaxed);
    }

    fn record_invalidation(&self, entries: usize) {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        self.invalidated_entries
            .fetch_add(entries as u64, Ordering::Relaxed);
    }

    fn record_stale_served(&self) {
        self.stale_served.fetch_add(1, Ordering::Relaxed);
    }

    fn record_refresh_started(&self) {
        self.refresh_started.fetch_add(1, Ordering::Relaxed);
    }

    fn record_refresh_succeeded(&self) {
        self.refresh_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    fn record_refresh_failed(&self) {
        self.refresh_failed.fetch_add(1, Ordering::Relaxed);
    }

    fn record_refresh_skipped(&self) {
        self.refresh_skipped.fetch_add(1, Ordering::Relaxed);
    }

    fn record_stampede_wait(&self) {
        self.stampede_waits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_stampede_timeout(&self) {
        self.stampede_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    fn record_compute_error(&self) {
        self.compute_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> ContextCacheMetricsSnapshot {
        self.load_snapshot()
    }
}

/// No-op recorder used when cache metrics are disabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopContextCacheMetrics;

impl ContextCacheMetricsRecorder for NoopContextCacheMetrics {
    fn record_hit(&self) {}
    fn record_miss(&self) {}
    fn record_set(&self) {}
    fn record_invalidation(&self, _entries: usize) {}
    fn record_stale_served(&self) {}
    fn record_refresh_started(&self) {}
    fn record_refresh_succeeded(&self) {}
    fn record_refresh_failed(&self) {}
    fn record_stampede_wait(&self) {}
    fn record_stampede_timeout(&self) {}
    fn record_compute_error(&self) {}
    fn snapshot(&self) -> ContextCacheMetricsSnapshot {
        ContextCacheMetricsSnapshot::default()
    }
}

/// Builds the metrics recorder for the given cache configuration.
pub fn context_cache_metrics_recorder(
    config: &ContextCacheConfig,
) -> Arc<dyn ContextCacheMetricsRecorder> {
    if config.metrics_active() {
        Arc::new(InMemoryContextCacheMetrics::new())
    } else {
        Arc::new(NoopContextCacheMetrics)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn counters_start_at_zero() {
        let metrics = InMemoryContextCacheMetrics::new();
        assert_eq!(metrics.snapshot(), ContextCacheMetricsSnapshot::default());
    }

    #[test]
    fn record_hit_increments_hits() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_hit();
        assert_eq!(metrics.snapshot().hits, 1);
    }

    #[test]
    fn record_miss_increments_misses() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_miss();
        assert_eq!(metrics.snapshot().misses, 1);
    }

    #[test]
    fn record_set_increments_sets() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_set();
        assert_eq!(metrics.snapshot().sets, 1);
    }

    #[test]
    fn record_invalidation_increments_both_counters() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_invalidation(3);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.invalidations, 1);
        assert_eq!(snapshot.invalidated_entries, 3);
    }

    #[test]
    fn record_stale_served_increments_stale_served() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_stale_served();
        assert_eq!(metrics.snapshot().stale_served, 1);
    }

    #[test]
    fn record_refresh_started_increments_refresh_started() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_refresh_started();
        assert_eq!(metrics.snapshot().refresh_started, 1);
    }

    #[test]
    fn record_refresh_succeeded_increments_refresh_succeeded() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_refresh_succeeded();
        assert_eq!(metrics.snapshot().refresh_succeeded, 1);
    }

    #[test]
    fn record_refresh_failed_increments_refresh_failed() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_refresh_failed();
        assert_eq!(metrics.snapshot().refresh_failed, 1);
    }

    #[test]
    fn record_stampede_wait_increments_stampede_waits() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_stampede_wait();
        assert_eq!(metrics.snapshot().stampede_waits, 1);
    }

    #[test]
    fn record_stampede_timeout_increments_stampede_timeouts() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_stampede_timeout();
        assert_eq!(metrics.snapshot().stampede_timeouts, 1);
    }

    #[test]
    fn record_compute_error_increments_compute_errors() {
        let metrics = InMemoryContextCacheMetrics::new();
        metrics.record_compute_error();
        assert_eq!(metrics.snapshot().compute_errors, 1);
    }

    #[test]
    fn noop_recorder_keeps_zero_snapshot() {
        let metrics = NoopContextCacheMetrics;
        metrics.record_hit();
        metrics.record_miss();
        assert_eq!(metrics.snapshot(), ContextCacheMetricsSnapshot::default());
    }

    #[tokio::test]
    async fn concurrent_increments_are_safe() {
        let metrics = Arc::new(InMemoryContextCacheMetrics::new());
        let mut handles = Vec::new();

        for _ in 0..32 {
            let metrics = metrics.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    metrics.record_hit();
                    metrics.record_miss();
                }
            }));
        }

        for handle in handles {
            handle.await.expect("join");
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.hits, 3200);
        assert_eq!(snapshot.misses, 3200);
    }
}
