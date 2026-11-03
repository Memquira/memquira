//! Legacy in-process counters kept for compatibility during migration.
//! Prefer [`crate::metrics`] Prometheus facade for new instrumentation.

use std::sync::atomic::{AtomicU64, Ordering};

/// Deprecated process-local counters; scrape output now comes from Prometheus.
#[derive(Debug, Default)]
pub struct Metrics {
    http_requests_total: AtomicU64,
    api_errors_total: AtomicU64,
    request_duration_ms_sum: AtomicU64,
    request_duration_ms_count: AtomicU64,
    memory_add_requests_total: AtomicU64,
    memory_search_requests_total: AtomicU64,
    context_requests_total: AtomicU64,
}

impl Metrics {
    pub fn record_request(&self, path: &str, status: u16, latency_ms: u64) {
        self.http_requests_total.fetch_add(1, Ordering::Relaxed);
        self.request_duration_ms_sum
            .fetch_add(latency_ms, Ordering::Relaxed);
        self.request_duration_ms_count
            .fetch_add(1, Ordering::Relaxed);

        if status >= 400 {
            self.api_errors_total.fetch_add(1, Ordering::Relaxed);
        }

        match path {
            "/api/v1/memories" => {
                self.memory_add_requests_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            "/api/v1/memories/search" => {
                self.memory_search_requests_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            "/api/v1/context" => {
                self.context_requests_total.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}
