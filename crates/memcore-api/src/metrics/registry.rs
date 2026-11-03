//! Process-wide Prometheus recorder installation.

use std::sync::{Mutex, OnceLock};

use memcore_common::{MemcoreError, MemcoreResult};
use memcore_config::Settings;
use metrics::{counter, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Stage timings span sub-millisecond merges to multi-second provider calls.
const STAGE_DURATION_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

const HTTP_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Holds optional scrape handle when metrics are enabled for this process/app.
#[derive(Clone, Debug)]
pub struct MetricsExporter {
    handle: Option<PrometheusHandle>,
}

impl MetricsExporter {
    pub fn disabled() -> Self {
        Self { handle: None }
    }

    pub fn from_settings(settings: &Settings) -> Self {
        if !settings.metrics_enabled {
            return Self::disabled();
        }
        match install_metrics_recorder(settings) {
            Ok(Some(handle)) => {
                if settings.metrics_include_process {
                    ensure_build_info();
                }
                Self {
                    handle: Some(handle),
                }
            }
            Ok(None) => Self::disabled(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "metrics enabled but recorder install failed; scrape endpoint will be unavailable"
                );
                Self::disabled()
            }
        }
    }

    pub fn render(&self) -> Option<String> {
        let handle = self.handle.as_ref().or_else(|| PROMETHEUS_HANDLE.get());
        handle.map(|handle| {
            handle.run_upkeep();
            handle.render()
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.handle.is_some()
    }
}

/// Installs the global Prometheus recorder once. Safe across tests.
pub fn install_metrics_recorder(_settings: &Settings) -> MemcoreResult<Option<PrometheusHandle>> {
    if let Some(existing) = PROMETHEUS_HANDLE.get() {
        return Ok(Some(existing.clone()));
    }

    let _guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(existing) = PROMETHEUS_HANDLE.get() {
        return Ok(Some(existing.clone()));
    }

    let builder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("memcore_http_request_duration_seconds".to_string()),
            HTTP_DURATION_BUCKETS,
        )
        .and_then(|builder| {
            builder.set_buckets_for_metric(
                Matcher::Full(memcore_core::observability::STAGE_DURATION_METRIC.to_string()),
                STAGE_DURATION_BUCKETS,
            )
        })
        .map_err(|error| {
            MemcoreError::Internal(format!("metrics bucket config failed: {error}"))
        })?;

    match builder.install_recorder() {
        Ok(handle) => {
            let _ = PROMETHEUS_HANDLE.set(handle.clone());
            super::ops::register_shed_counters();
            Ok(Some(handle))
        }
        Err(error) => {
            if let Some(existing) = PROMETHEUS_HANDLE.get() {
                return Ok(Some(existing.clone()));
            }
            tracing::warn!(
                error = %error,
                "prometheus metrics recorder install failed; continuing without scrape handle"
            );
            Err(MemcoreError::Internal(format!(
                "failed to install prometheus recorder: {error}"
            )))
        }
    }
}

pub fn ensure_build_info() {
    // Gauge with static labels; value 1 means this build is running.
    metrics::gauge!(
        "memcore_build_info",
        "version" => env!("CARGO_PKG_VERSION"),
        "git_sha" => option_env!("MEMCORE_BUILD_GIT_SHA").unwrap_or("unknown"),
        "profile" => option_env!("MEMCORE_BUILD_PROFILE").unwrap_or("unknown"),
    )
    .set(1.0);
}

pub fn record_http_request(method: &str, route: &str, status: u16, duration_secs: f64) {
    let status_label = status.to_string();
    counter!(
        "memcore_http_requests_total",
        "method" => method.to_string(),
        "route" => route.to_string(),
        "status" => status_label.clone(),
    )
    .increment(1);

    histogram!(
        "memcore_http_request_duration_seconds",
        "method" => method.to_string(),
        "route" => route.to_string(),
        "status" => status_label.clone(),
    )
    .record(duration_secs);

    if status >= 400 {
        let class = if status >= 500 { "5xx" } else { "4xx" };
        counter!(
            "memcore_http_request_errors_total",
            "method" => method.to_string(),
            "route" => route.to_string(),
            "status_class" => class.to_string(),
        )
        .increment(1);
    }
}

/// Counts one in-flight request of a bounded class; the gauge is decremented on drop, so a
/// cancelled or panicking request is released too.
pub struct InFlightGuard {
    class: &'static str,
}

impl InFlightGuard {
    pub fn new(class: &'static str) -> Self {
        metrics::gauge!("memcore_http_requests_in_flight", "class" => class).increment(1.0);
        Self { class }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        metrics::gauge!("memcore_http_requests_in_flight", "class" => self.class).decrement(1.0);
    }
}

/// Values that are read, not counted: process memory and the query-embedding cache (H2)
/// totals, refreshed right before each scrape.
pub fn refresh_scrape_gauges(cache: Option<memcore_core::QueryEmbeddingCacheStats>) {
    if let Some(bytes) = super::process::resident_memory_bytes() {
        metrics::gauge!("memcore_process_resident_memory_bytes").set(bytes as f64);
    }
    if let Some(stats) = cache {
        for (outcome, value) in [
            ("request", stats.requests),
            ("hit", stats.hits),
            ("leader", stats.leaders),
            ("follower", stats.followers),
            ("bypassed", stats.bypassed),
            ("failed_flight", stats.failed_flights),
            ("abandoned_flight", stats.abandoned_flights),
            ("follower_failure", stats.follower_failures),
        ] {
            counter!("memcore_query_embedding_cache_total", "outcome" => outcome).absolute(value);
        }
    }
}

pub fn record_auth_failure(reason: &str, method: &str, route: &str) {
    counter!(
        "memcore_auth_failures_total",
        "reason" => reason.to_string(),
        "method" => method.to_string(),
        "route" => route.to_string(),
    )
    .increment(1);
}

pub fn record_rate_limited(method: &str, route: &str) {
    counter!(
        "memcore_rate_limited_requests_total",
        "method" => method.to_string(),
        "route" => route.to_string(),
    )
    .increment(1);
}
