//! Prometheus metrics endpoint and label safety tests.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

fn metrics_settings(require_auth: bool) -> Settings {
    Settings {
        metrics_enabled: true,
        metrics_path: "/metrics".to_string(),
        metrics_require_auth: require_auth,
        metrics_include_process: true,
        ..Settings::default()
    }
}

async fn send_text(app: axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.oneshot(request).await.expect("router should respond");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn metrics_disabled_returns_not_found() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, _) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_requires_auth_by_default_when_enabled() {
    let app = create_app(AppState::new(metrics_settings(true)));
    let (status, body) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.to_lowercase().contains("memcore_dev_key"));
    assert!(!body.to_lowercase().contains("bearer "));
}

#[tokio::test]
async fn metrics_returns_prometheus_text_when_authed() {
    let app = create_app(AppState::new(metrics_settings(true)));
    let (auth_name, auth_value) = authorization_header();

    let _ = send_text(
        app.clone(),
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    let (status, text) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .header(auth_name, auth_value)
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("memcore_http_requests_total"));
    assert!(text.contains("memcore_http_request_duration_seconds"));
    assert!(!text.contains("memcore_dev_key"));
    assert!(!text.contains("postgres://"));
    assert!(!text.contains("redis://"));
    assert!(!text.contains("Bearer "));
    assert!(!text.contains("sk-live"));
    assert!(!text.contains("User likes green tea"));
}

#[tokio::test]
async fn metrics_allows_unauthenticated_when_configured() {
    let app = create_app(AppState::new(metrics_settings(false)));

    // Record a request through this app before scraping. Every test in this
    // binary shares one process-global Prometheus recorder, so without a metric
    // of its own this test only passed when a concurrent test happened to
    // populate the registry first — and failed whenever it won that race and
    // scraped an empty one.
    let (status, _) = send_text(
        app.clone(),
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, text) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("memcore_http_requests_total"),
        "the /health request this test made should be counted in its own scrape"
    );
}

#[tokio::test]
async fn http_metrics_increment_on_requests() {
    let app = create_app(AppState::new(metrics_settings(false)));

    for _ in 0..3 {
        let (status, _) = send_text(
            app.clone(),
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, text) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("memcore_http_requests_total"));
    assert!(text.contains("/health") || text.contains("route="));
}

#[tokio::test]
async fn auth_failure_increments_metric() {
    let app = create_app(AppState::new(metrics_settings(false)));

    let (status, _) = send_text(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header("content-type", "application/json")
            .header("X-Organization-ID", "org_metrics")
            .body(Body::from(
                r#"{"user_id":"u1","messages":[{"role":"user","content":"hi"}],"metadata":{}}"#,
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, text) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert!(text.contains("memcore_auth_failures_total"));
}

#[tokio::test]
async fn memory_create_increments_operation_metric() {
    let app = create_app(AppState::new(metrics_settings(false)));
    let (auth_name, auth_value) = authorization_header();

    let (status, _) = send_text(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header("content-type", "application/json")
            .header("X-Organization-ID", "org_metrics")
            .header(auth_name, auth_value)
            .body(Body::from(
                r#"{"user_id":"user_metrics","messages":[{"role":"user","content":"User prefers concise technical summaries."}],"metadata":{}}"#,
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, text) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert!(text.contains("memcore_memory_create_total"));
    assert!(!text.contains("User prefers concise technical summaries"));
}
