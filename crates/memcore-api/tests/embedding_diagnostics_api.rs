//! The embedding provider's admin diagnostics route.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use std::sync::Arc;
use tower::ServiceExt;

use common::authorization_header;

const PATH: &str = "/api/v1/admin/org/embedding/diagnostics";

fn app() -> axum::Router {
    let settings = Settings::default();
    let engine = Arc::new(memcore_api::create_mock_memory_engine(&settings).expect("engine"));
    create_app(AppState::with_memory_engine(settings, engine))
}

#[tokio::test]
async fn diagnostics_require_authorization() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(PATH)
                .header("X-Organization-ID", "org_diag")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_provider_without_diagnostics_reports_null() {
    let (name, value) = authorization_header();
    let response = app()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(PATH)
                .header("X-Organization-ID", "org_diag")
                .header(name, value)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["status"], "success");
    assert_eq!(json["scope"], "process_local");
    assert!(json["diagnostics"].is_null());
}
