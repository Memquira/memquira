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
const MEMORY_CONTENT: &str = "Listed memory content for user A";

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

fn get_request(uri: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }

    builder.body(Body::empty()).expect("request should build")
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

async fn seed_memory_for_user(app: &axum::Router, org_id: &str, user_id: &str, content: &str) {
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

#[tokio::test]
async fn list_memories_after_adding_memory() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn list_response_contains_memories_array() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (_, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    let memories = json["memories"]
        .as_array()
        .expect("memories should be an array");
    assert_eq!(memories[0]["content"], MEMORY_CONTENT);
    assert!(memories[0]["id"].is_string());
    assert_eq!(memories[0]["memory_type"], "Conversation");
    assert!(memories[0]["metadata"].is_object());
    assert!(json["next_cursor"].is_null());
}

#[tokio::test]
async fn list_route_requires_authorization_header() {
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            false,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn list_route_requires_organization_header() {
    let (status, json) = response_parts(
        test_app(),
        get_request(&format!("/api/v1/users/{USER_A}/memories"), None, true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn invalid_memory_type_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &format!("/api/v1/users/{USER_A}/memories?memory_type=NotValid"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid memory type")
    );
}

#[tokio::test]
async fn limit_defaults_when_omitted() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, _) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn limit_above_max_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &format!("/api/v1/users/{USER_A}/memories?limit=200"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "limit cannot exceed 100");
}

#[tokio::test]
async fn invalid_cursor_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &format!("/api/v1/users/{USER_A}/memories?cursor=opaque-token"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid cursor");
}

#[tokio::test]
async fn include_deleted_defaults_to_false() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn user_a_cannot_list_user_b_memories() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_B}/memories"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn org_a_cannot_list_org_b_memories() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories"),
            Some(ORG_B),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn keyword_search_finds_matching_content() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, "learning Rust async patterns").await;
    seed_memory_for_user(&app, ORG_A, USER_A, "python data science").await;

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories?q=rust"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
    assert!(
        json["memories"][0]["content"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("rust")
    );
}

#[tokio::test]
async fn empty_q_behaves_like_no_search() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &format!("/api/v1/users/{USER_A}/memories?q=%20%20"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn long_q_returns_validation_error() {
    let long = "a".repeat(201);
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &format!("/api/v1/users/{USER_A}/memories?q={long}"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "q must be 200 characters or less");
}
