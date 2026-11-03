mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn add_memory_request(body: &str, org_id: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
        .header("content-type", "application/json");

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    let (name, value) = authorization_header();
    builder = builder.header(name, value);

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

const VALID_BODY: &str = r#"{
  "user_id": "user_123",
  "messages": [
    {
      "role": "user",
      "content": "I am learning Rust and building a memory engine."
    }
  ],
  "metadata": {
    "session_id": "session_123"
  }
}"#;

#[tokio::test]
async fn post_memories_succeeds_with_valid_request() {
    let (status, json) =
        response_parts(test_app(), add_memory_request(VALID_BODY, Some("org_123"))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn post_memories_response_includes_operation_summary() {
    let (_, json) =
        response_parts(test_app(), add_memory_request(VALID_BODY, Some("org_123"))).await;

    assert_eq!(json["summary"]["added"], 1);
    assert_eq!(json["summary"]["updated"], 0);
    assert_eq!(json["summary"]["deleted"], 0);
    assert_eq!(json["summary"]["noop"], 0);
}

#[tokio::test]
async fn post_memories_response_includes_memories() {
    let (_, json) =
        response_parts(test_app(), add_memory_request(VALID_BODY, Some("org_123"))).await;

    let memories = json["memories"]
        .as_array()
        .expect("memories should be an array");
    assert_eq!(memories.len(), 1);
    assert!(memories[0]["id"].is_string());
    assert_eq!(
        memories[0]["content"],
        "I am learning Rust and building a memory engine."
    );
    assert_eq!(memories[0]["memory_type"], "Conversation");
    assert!(memories[0]["confidence"].is_number());
    assert!(memories[0]["importance"].is_number());
}

#[tokio::test]
async fn missing_organization_header_returns_error() {
    let (status, json) = response_parts(test_app(), add_memory_request(VALID_BODY, None)).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "missing X-Organization-ID header");
}

#[tokio::test]
async fn empty_user_id_returns_validation_error() {
    let body = r#"{
      "user_id": "",
      "messages": [{ "role": "user", "content": "hello" }],
      "metadata": {}
    }"#;

    let (status, json) =
        response_parts(test_app(), add_memory_request(body, Some("org_123"))).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "user_id cannot be empty");
}

#[tokio::test]
async fn empty_messages_returns_validation_error() {
    let body = r#"{
      "user_id": "user_123",
      "messages": [],
      "metadata": {}
    }"#;

    let (status, json) =
        response_parts(test_app(), add_memory_request(body, Some("org_123"))).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "messages cannot be empty");
}

#[tokio::test]
async fn invalid_role_returns_validation_error() {
    let body = r#"{
      "user_id": "user_123",
      "messages": [{ "role": "moderator", "content": "hello" }],
      "metadata": {}
    }"#;

    let (status, json) =
        response_parts(test_app(), add_memory_request(body, Some("org_123"))).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid message role: moderator");
}

#[tokio::test]
async fn adding_same_memory_twice_does_not_duplicate_listed_memories() {
    let app = test_app();
    let body = r#"{
      "user_id": "user_dedup_api",
      "messages": [{ "role": "user", "content": "I am learning Rust." }],
      "metadata": {}
    }"#;

    let (first_status, first_json) =
        response_parts(app.clone(), add_memory_request(body, Some("org_dedup_api"))).await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first_json["summary"]["added"], 1);

    let (second_status, second_json) =
        response_parts(app.clone(), add_memory_request(body, Some("org_dedup_api"))).await;
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(second_json["summary"]["added"], 0);
    assert_eq!(second_json["summary"]["noop"], 1);

    let list_request = Request::builder()
        .method("GET")
        .uri("/api/v1/users/user_dedup_api/memories")
        .header("X-Organization-ID", "org_dedup_api");
    let (auth_name, auth_value) = authorization_header();
    let list_request = list_request
        .header(auth_name, auth_value)
        .body(Body::empty())
        .expect("request");

    let (list_status, list_json) = response_parts(app, list_request).await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn semantically_similar_memory_is_not_duplicated_on_second_add() {
    let app = test_app();
    let first_body = r#"{
      "user_id": "user_semantic_dedup",
      "messages": [{ "role": "user", "content": "The user prefers working with Rust programming language for backend services." }],
      "metadata": {}
    }"#;
    let second_body = r#"{
      "user_id": "user_semantic_dedup",
      "messages": [{ "role": "user", "content": "The user prefers working with Rust coding language for backend services." }],
      "metadata": {}
    }"#;

    let (first_status, first_json) = response_parts(
        app.clone(),
        add_memory_request(first_body, Some("org_semantic_dedup")),
    )
    .await;
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first_json["summary"]["added"], 1);

    let (second_status, second_json) = response_parts(
        app.clone(),
        add_memory_request(second_body, Some("org_semantic_dedup")),
    )
    .await;
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(second_json["summary"]["added"], 0);
    assert_eq!(second_json["summary"]["noop"], 1);

    let list_request = Request::builder()
        .method("GET")
        .uri("/api/v1/users/user_semantic_dedup/memories")
        .header("X-Organization-ID", "org_semantic_dedup");
    let (auth_name, auth_value) = authorization_header();
    let list_request = list_request
        .header(auth_name, auth_value)
        .body(Body::empty())
        .expect("request");

    let (list_status, list_json) = response_parts(app, list_request).await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_json["memories"].as_array().unwrap().len(), 1);
}
