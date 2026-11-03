mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use memcore_core::USER_EXPORT_FORMAT_VERSION;
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const ORG_A: &str = "org_export_a";
const ORG_B: &str = "org_export_b";
const USER_A: &str = "user_export_a";
const USER_B: &str = "user_export_b";
const MEMORY_CONTENT: &str = "Export API test memory content";

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

async fn seed_memory(app: &axum::Router, org_id: &str, user_id: &str, content: &str) -> Uuid {
    let add_body = format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{}}
        }}"#
    );

    let (status, json) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", &add_body, org_id),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    json["memories"][0]["id"]
        .as_str()
        .expect("memory id should be present")
        .parse()
        .expect("memory id should be a valid uuid")
}

fn export_uri(user_id: &str, query: &str) -> String {
    if query.is_empty() {
        format!("/api/v1/users/{user_id}/export")
    } else {
        format!("/api/v1/users/{user_id}/export?{query}")
    }
}

#[tokio::test]
async fn export_user_after_adding_memories() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["export"]["format_version"], USER_EXPORT_FORMAT_VERSION);
    assert_eq!(json["export"]["org_id"], ORG_A);
    assert_eq!(json["export"]["user_id"], USER_A);
}

#[tokio::test]
async fn export_includes_facts() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    let facts = json["export"]["facts"]
        .as_array()
        .expect("facts should be an array");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["content"], MEMORY_CONTENT);
}

#[tokio::test]
async fn export_includes_memory_events_by_default() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    let events = json["export"]["memory_events"]
        .as_array()
        .expect("memory_events should be an array");
    assert!(!events.is_empty());
}

#[tokio::test]
async fn export_excludes_memory_events_when_disabled() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &export_uri(USER_A, "include_events=false"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = json["export"]["memory_events"]
        .as_array()
        .expect("memory_events should be an array");
    assert!(events.is_empty());
}

#[tokio::test]
async fn export_does_not_expose_input_text() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    let events = json["export"]["memory_events"]
        .as_array()
        .expect("memory_events should be an array");
    for event in events {
        assert!(event.get("input_text").is_none());
    }
}

#[tokio::test]
async fn export_does_not_expose_api_key_fields() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    let body = serde_json::to_string(&json).expect("serialize");
    assert!(!body.contains("key_hash"));
    assert!(!body.contains("raw_key"));
}

#[tokio::test]
async fn export_requires_authorization_header() {
    let app = test_app();

    let (status, _) = response_parts(
        app,
        get_request(&export_uri(USER_A, ""), Some(ORG_A), false),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn export_requires_organization_header() {
    let app = test_app();

    let (status, _) = response_parts(app, get_request(&export_uri(USER_A, ""), None, true)).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn user_a_cannot_export_user_b_data_via_wrong_path() {
    let app = test_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;
    seed_memory(&app, ORG_A, USER_B, "user b secret").await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    let facts = json["export"]["facts"]
        .as_array()
        .expect("facts should be an array");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["content"], MEMORY_CONTENT);
}

#[tokio::test]
async fn org_a_cannot_export_org_b_data() {
    let app = test_app();
    seed_memory(&app, ORG_B, USER_A, "org b secret").await;

    let (status, json) =
        response_parts(app, get_request(&export_uri(USER_A, ""), Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    let facts = json["export"]["facts"]
        .as_array()
        .expect("facts should be an array");
    assert!(facts.is_empty());
}

#[tokio::test]
async fn include_deleted_false_excludes_deleted_fact() {
    let app = test_app();
    let memory_id = seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let delete_uri = format!("/api/v1/users/{USER_A}/memories/{memory_id}");
    let (delete_status, _) = response_parts(
        app.clone(),
        Request::builder()
            .method("DELETE")
            .uri(delete_uri)
            .header("X-Organization-ID", ORG_A)
            .header(authorization_header().0, authorization_header().1)
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(
            &export_uri(USER_A, "include_deleted=false"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["export"]["facts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn include_deleted_true_includes_deleted_fact() {
    let app = test_app();
    let memory_id = seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let delete_uri = format!("/api/v1/users/{USER_A}/memories/{memory_id}");
    let (delete_status, _) = response_parts(
        app.clone(),
        Request::builder()
            .method("DELETE")
            .uri(delete_uri)
            .header("X-Organization-ID", ORG_A)
            .header(authorization_header().0, authorization_header().1)
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(
            &export_uri(USER_A, "include_deleted=true"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["export"]["facts"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn health_ready_version_remain_public_metrics_disabled_by_default() {
    let app = test_app();

    for path in ["/health", "/ready", "/api/v1/version"] {
        let (status, _) = response_parts(
            app.clone(),
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await;
        assert!(
            status.is_success(),
            "unexpected status for {path}: {status}"
        );
    }

    let (metrics_status, _) = response_parts(
        app,
        Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;
    assert_eq!(metrics_status, StatusCode::NOT_FOUND);
}
