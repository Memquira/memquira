mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

const ORG_A: &str = "org_a";
const ORG_B: &str = "org_b";
const USER_A: &str = "user_a";
const USER_B: &str = "user_b";
const MEMORY_CONTENT_A: &str = "First memory for forget user test";
const MEMORY_CONTENT_B: &str = "Second memory for forget user test";

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn post_request(uri: &str, body: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::from(body.to_string()))
        .expect("request should build")
}

fn delete_user_request(uri: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("DELETE").uri(uri);

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }

    builder.body(Body::empty()).expect("request should build")
}

fn get_request(uri: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::empty())
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

async fn seed_memory(app: &axum::Router, org_id: &str, user_id: &str, content: &str) {
    let add_body = format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{}}
        }}"#
    );

    let (status, _) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", &add_body, org_id),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

async fn seed_multiple_memories(app: &axum::Router, org_id: &str, user_id: &str) {
    seed_memory(app, org_id, user_id, MEMORY_CONTENT_A).await;
    seed_memory(app, org_id, user_id, MEMORY_CONTENT_B).await;
}

#[tokio::test]
async fn forget_user_after_adding_multiple_memories() {
    let app = test_app();
    seed_multiple_memories(&app, ORG_A, USER_A).await;

    let (status, json) = response_parts(
        app,
        delete_user_request(&format!("/api/v1/users/{USER_A}"), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["deleted"], true);
}

#[tokio::test]
async fn forgotten_user_memories_no_longer_appear_in_listing() {
    let app = test_app();
    seed_multiple_memories(&app, ORG_A, USER_A).await;

    let (status, _) = response_parts(
        app.clone(),
        delete_user_request(&format!("/api/v1/users/{USER_A}"), Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn forgotten_user_memories_no_longer_appear_in_search() {
    let app = test_app();
    seed_multiple_memories(&app, ORG_A, USER_A).await;

    let (status, _) = response_parts(
        app.clone(),
        delete_user_request(&format!("/api/v1/users/{USER_A}"), Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let search_body = format!(
        r#"{{
          "user_id": "{USER_A}",
          "query": "{MEMORY_CONTENT_A}"
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn forgetting_user_a_does_not_delete_user_b_memories() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT_A).await;
    seed_memory(&app, ORG_A, USER_B, MEMORY_CONTENT_B).await;

    let (status, _) = response_parts(
        app.clone(),
        delete_user_request(&format!("/api/v1/users/{USER_A}"), Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_B}/memories"), ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn forgetting_user_in_org_a_does_not_delete_same_user_in_org_b() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT_A).await;
    seed_memory(&app, ORG_B, USER_A, MEMORY_CONTENT_B).await;

    let (status, _) = response_parts(
        app.clone(),
        delete_user_request(&format!("/api/v1/users/{USER_A}"), Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_B),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn forget_route_requires_authorization_header() {
    let (status, json) = response_parts(
        test_app(),
        delete_user_request(&format!("/api/v1/users/{USER_A}"), Some(ORG_A), false),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn forget_route_requires_organization_header() {
    let (status, json) = response_parts(
        test_app(),
        delete_user_request(&format!("/api/v1/users/{USER_A}"), None, true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn empty_user_id_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        delete_user_request("/api/v1/users/%20", Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "user_id cannot be empty");
}
