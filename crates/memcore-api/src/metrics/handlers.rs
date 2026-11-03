//! `/metrics` scrape handler.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

pub async fn metrics_handler(State(state): State<AppState>) -> Response {
    if !state.settings.metrics_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }

    super::refresh_scrape_gauges(state.memory_engine.query_embedding_cache_stats());
    let Some(body) = state.metrics_exporter.render() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "metrics recorder unavailable",
        )
            .into_response();
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/plain; version=0.0.4; charset=utf-8")
        .body(axum::body::Body::from(body))
        .expect("metrics response should build")
}
