mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::{DEV_API_KEY, authorization_header};

const VALID_ADD_BODY: &str = r#"{
  "user_id": "user_123",
  "messages": [{ "role": "user", "content": "hello" }],
  "metadata": {}
}"#;

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn test_app_auth_disabled() -> axum::Router {
    let mut settings = Settings::default();
    settings.auth_enabled = false;
    create_app(AppState::new(settings))
}

fn post_request(uri: &str, body: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }

    builder
        .body(Body::from(body.to_string()))
        .expect("request should build")
}

async fn response_parts(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.expect("router should respond");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, json)
}

#[tokio::test]
async fn health_works_without_auth() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn ready_works_without_auth() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn add_memory_fails_without_authorization_header() {
    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories", VALID_ADD_BODY, Some("org_123"), false),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
    assert_eq!(json["error"]["message"], "missing authorization header");
}

#[tokio::test]
async fn add_memory_fails_with_invalid_authorization_format() {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
        .header("content-type", "application/json")
        .header("X-Organization-ID", "org_123")
        .header("Authorization", "Token not-bearer")
        .body(Body::from(VALID_ADD_BODY.to_string()))
        .expect("request should build");

    let (status, json) = response_parts(test_app(), request).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["message"], "invalid authorization header");
}

#[tokio::test]
async fn add_memory_fails_with_invalid_api_key() {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
        .header("content-type", "application/json")
        .header("X-Organization-ID", "org_123")
        .header("Authorization", "Bearer wrong_key")
        .body(Body::from(VALID_ADD_BODY.to_string()))
        .expect("request should build");

    let (status, json) = response_parts(test_app(), request).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["message"], "invalid api key");
}

#[tokio::test]
async fn add_memory_succeeds_with_valid_api_key() {
    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories", VALID_ADD_BODY, Some("org_123"), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn search_memory_requires_auth() {
    let search_body = r#"{
      "user_id": "user_123",
      "query": "Rust"
    }"#;

    let (status, json) = response_parts(
        test_app(),
        post_request(
            "/api/v1/memories/search",
            search_body,
            Some("org_123"),
            false,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn build_context_requires_auth() {
    let context_body = r#"{
      "user_id": "user_123",
      "query": "Rust"
    }"#;

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", context_body, Some("org_123"), false),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn auth_disabled_allows_protected_routes_without_authorization() {
    let (status, json) = response_parts(
        test_app_auth_disabled(),
        post_request("/api/v1/memories", VALID_ADD_BODY, Some("org_123"), false),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[test]
fn dev_api_key_matches_config_default() {
    assert_eq!(DEV_API_KEY, Settings::default().dev_api_key);
}
