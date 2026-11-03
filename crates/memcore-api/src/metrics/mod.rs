//! Prometheus metrics foundation for memcore-api.
//!
//! Installs a process-wide Prometheus recorder (idempotent) and exposes scrape
//! rendering via [`PrometheusHandle`]. Metric recording is a no-op when no
//! recorder is installed.

pub mod handlers;
pub mod labels;
pub mod middleware;
pub mod ops;
pub mod process;
pub mod provider;
pub mod registry;

pub use handlers::metrics_handler;
pub use labels::{normalize_route, request_class};
pub use middleware::require_metrics_auth;
pub use ops::record_request_shed;
pub use provider::PrometheusProviderUsageRecorder;
pub use registry::{
    InFlightGuard, MetricsExporter, ensure_build_info, install_metrics_recorder,
    record_auth_failure, record_http_request, record_rate_limited, refresh_scrape_gauges,
};
