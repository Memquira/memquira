mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

const ORG_ID: &str = "org_middleware_test";
const USER_ID: &str = "user_123";

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn protected_post(uri: &str, body: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
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

const ADD_BODY: &str = r#"{
  "user_id": "user_123",
  "messages": [{ "role": "user", "content": "tenant middleware test" }],
  "metadata": {}
}"#;

const SEARCH_BODY: &str = r#"{
  "user_id": "user_123",
  "query": "tenant middleware test"
}"#;

const CONTEXT_BODY: &str = r#"{
  "user_id": "user_123",
  "query": "tenant middleware test"
}"#;

#[tokio::test]
async fn health_works_without_auth_and_tenant_header() {
    let response = test_app()
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
async fn ready_works_without_auth_and_tenant_header() {
    let response = test_app()
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
async fn protected_route_fails_without_authorization_header() {
    let (status, json) = response_parts(
        test_app(),
        protected_post("/api/v1/memories", ADD_BODY, Some(ORG_ID), false),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn protected_route_fails_without_tenant_header_when_auth_valid() {
    let (status, json) = response_parts(
        test_app(),
        protected_post("/api/v1/memories", ADD_BODY, None, true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "missing X-Organization-ID header");
}

#[tokio::test]
async fn protected_route_fails_with_empty_tenant_header() {
    let (status, json) = response_parts(
        test_app(),
        protected_post("/api/v1/memories", ADD_BODY, Some("   "), true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(
        json["error"]["message"],
        "X-Organization-ID cannot be empty"
    );
}

#[tokio::test]
async fn add_memory_succeeds_with_valid_auth_and_tenant_header() {
    let (status, json) = response_parts(
        test_app(),
        protected_post("/api/v1/memories", ADD_BODY, Some(ORG_ID), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn search_memory_succeeds_with_valid_auth_and_tenant_header() {
    let app = test_app();

    let (add_status, _) = response_parts(
        app.clone(),
        protected_post("/api/v1/memories", ADD_BODY, Some(ORG_ID), true),
    )
    .await;
    assert_eq!(add_status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        protected_post("/api/v1/memories/search", SEARCH_BODY, Some(ORG_ID), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn build_context_succeeds_with_valid_auth_and_tenant_header() {
    let app = test_app();

    let (add_status, _) = response_parts(
        app.clone(),
        protected_post("/api/v1/memories", ADD_BODY, Some(ORG_ID), true),
    )
    .await;
    assert_eq!(add_status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        protected_post("/api/v1/context", CONTEXT_BODY, Some(ORG_ID), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn trimmed_tenant_header_is_used_by_handler() {
    let org_with_whitespace = "  org_trim_test  ";
    let add_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "trim org test" }}],
          "metadata": {{}}
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        protected_post(
            "/api/v1/memories",
            &add_body,
            Some(org_with_whitespace),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["memories"][0]["content"], "trim org test");
}
