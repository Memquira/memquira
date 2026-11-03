mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{DEFAULT_REQUEST_ID_HEADER, Settings};
use tower::ServiceExt;

use common::authorization_header;

const CUSTOM_REQUEST_ID: &str = "req_custom_12345";

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings {
        metrics_enabled: true,
        metrics_require_auth: false,
        ..Settings::default()
    }))
}

fn app_with_metrics_disabled() -> axum::Router {
    let settings = Settings {
        metrics_enabled: false,
        ..Settings::default()
    };
    create_app(AppState::new(settings))
}

async fn send(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let response = app.oneshot(request).await.expect("router should respond");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, headers, json)
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
    let text = String::from_utf8_lossy(&body).to_string();
    (status, text)
}

#[tokio::test]
async fn request_id_is_generated_when_missing() {
    let app = test_app();
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    let request_id = headers
        .get(DEFAULT_REQUEST_ID_HEADER)
        .expect("response should include request id header");
    assert!(!request_id.to_str().unwrap().is_empty());
}

#[tokio::test]
async fn request_id_is_preserved_when_provided() {
    let app = test_app();
    let (_, headers, _) = send(
        app,
        Request::builder()
            .uri("/health")
            .header(DEFAULT_REQUEST_ID_HEADER, CUSTOM_REQUEST_ID)
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert_eq!(
        headers
            .get(DEFAULT_REQUEST_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap(),
        CUSTOM_REQUEST_ID
    );
}

#[tokio::test]
async fn health_route_still_works() {
    let app = test_app();
    let (status, _, json) = send(
        app,
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn ready_route_still_works() {
    let app = test_app();
    let (status, _, json) = send(
        app,
        Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ready");
}

#[tokio::test]
async fn metrics_route_returns_prometheus_text() {
    let app = test_app();

    let (status, _text) = send_text(
        app.clone(),
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, text) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("memcore_http_requests_total"));
    assert!(
        text.contains("memcore_http_request_duration_seconds")
            || text.contains("memcore_api_errors_total")
            || text.contains("# TYPE")
    );
}

#[tokio::test]
async fn metrics_route_returns_not_found_when_disabled() {
    let app = app_with_metrics_disabled();
    let (status, _) = send_text(
        app,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn protected_route_still_requires_auth() {
    let app = test_app();
    let (status, _, json) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header("content-type", "application/json")
            .header("X-Organization-ID", "org_test")
            .body(Body::from(
                r#"{"user_id":"u1","messages":[{"role":"user","content":"hi"}],"metadata":{}}"#
                    .to_string(),
            ))
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
    assert!(json["error"]["request_id"].is_string());
}

#[tokio::test]
async fn validation_error_includes_request_id() {
    let app = test_app();
    let (auth_name, auth_value) = authorization_header();

    let (status, _, json) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/context")
            .header("content-type", "application/json")
            .header("X-Organization-ID", "org_test")
            .header(auth_name, auth_value)
            .header(DEFAULT_REQUEST_ID_HEADER, CUSTOM_REQUEST_ID)
            .body(Body::from(
                r#"{"user_id":"u1","query":"","max_memories":10,"include_metadata":false}"#
                    .to_string(),
            ))
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["request_id"], CUSTOM_REQUEST_ID);
}
