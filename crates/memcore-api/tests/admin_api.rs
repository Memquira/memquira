mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_common::hash_api_key;
use memcore_config::{AuthMode, Settings};
use memcore_core::{ApiKeyRecord, ApiKeyScope};
use tower::ServiceExt;
use uuid::Uuid;

use common::{DEV_API_KEY, authorization_header};

const ORG_A: &str = "org_admin_api_a";
const ORG_B: &str = "org_admin_api_b";
const USER_A: &str = "user_admin_a";
const USER_B: &str = "user_admin_b";
const MEMORY_CONTENT: &str = "Admin API test memory content";

const API_KEY_PEPPER: &str = "admin-test-pepper";

fn dev_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn database_auth_settings() -> Settings {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());
    settings.dev_api_key = String::new();
    settings
}

fn admin_api_key_record(org_id: &str, raw_key: &str, name: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: name.to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![ApiKeyScope::AdminRead, ApiKeyScope::MemoryWrite],
        created_at: chrono::Utc::now(),
        revoked_at: None,
    }
}

fn memory_only_api_key_record(org_id: &str, raw_key: &str, name: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: name.to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![ApiKeyScope::MemoryRead, ApiKeyScope::MemoryWrite],
        created_at: chrono::Utc::now(),
        revoked_at: None,
    }
}

async fn seed_record(state: &AppState, record: ApiKeyRecord) {
    state
        .api_key_store
        .insert_api_key(record)
        .await
        .expect("api key should be inserted");
}

async fn database_app_with_admin(org_id: &str, raw_key: &str) -> axum::Router {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(&state, admin_api_key_record(org_id, raw_key, "admin-key")).await;
    create_app(state)
}

fn get_request(uri: &str, org_id: Option<&str>, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }

    builder.body(Body::empty()).expect("request should build")
}

fn post_memory(uri: &str, body: &str, org_id: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id);

    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    } else {
        let (auth_name, auth_value) = authorization_header();
        builder = builder.header(auth_name, auth_value);
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

async fn seed_memory(
    app: &axum::Router,
    org_id: &str,
    user_id: &str,
    content: &str,
    bearer: Option<&str>,
) {
    let add_body = format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{}}
        }}"#
    );
    let (status, _) = response_parts(
        app.clone(),
        post_memory("/api/v1/memories", &add_body, org_id, bearer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn org_summary_requires_authorization() {
    let app = dev_app();
    let (status, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/summary", Some(ORG_A), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn org_summary_requires_organization_header() {
    let app = dev_app();
    let (status, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/summary", None, Some(DEV_API_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn org_summary_works_in_dev_auth_mode() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request("/api/v1/admin/org/summary", Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["summary"]["org_id"], ORG_A);
    assert!(json["summary"]["total_users"].as_u64().unwrap() >= 1);
    assert!(json["summary"]["total_facts"].as_u64().unwrap() >= 1);
    assert!(json["summary"].get("content").is_none());
    assert!(json["summary"].get("input_text").is_none());
}

#[tokio::test]
async fn org_summary_is_scoped_to_organization() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;
    seed_memory(&app, ORG_B, USER_B, "other org memory", None).await;

    let (status, json) = response_parts(
        app,
        get_request("/api/v1/admin/org/summary", Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["org_id"], ORG_A);
    assert_eq!(json["summary"]["total_users"], 1);
}

#[tokio::test]
async fn database_auth_requires_admin_scope_for_org_summary() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(
        &state,
        memory_only_api_key_record(ORG_A, "memory-only-key", "memory-only"),
    )
    .await;
    let app = create_app(state);

    let (status, json) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/summary",
            Some(ORG_A),
            Some("memory-only-key"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["message"], "missing required scope");
}

#[tokio::test]
async fn database_auth_allows_admin_read_for_org_summary() {
    let app = database_app_with_admin(ORG_A, "admin-read-summary").await;
    seed_memory(
        &app,
        ORG_A,
        USER_A,
        MEMORY_CONTENT,
        Some("admin-read-summary"),
    )
    .await;

    let (status, json) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/summary",
            Some(ORG_A),
            Some("admin-read-summary"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn org_users_requires_authorization() {
    let app = dev_app();
    let (status, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/users", Some(ORG_A), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn org_users_requires_organization_header() {
    let app = dev_app();
    let (status, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/users", None, Some(DEV_API_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn org_users_returns_only_current_org_users() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;
    seed_memory(&app, ORG_B, USER_B, "other org memory", None).await;

    let (status, json) = response_parts(
        app,
        get_request("/api/v1/admin/org/users", Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["users"].as_array().unwrap().len(), 1);
    assert_eq!(json["users"][0]["user_id"], USER_A);
    assert!(json["users"][0]["memory_count"].as_u64().unwrap() >= 1);
    assert!(json["users"][0].get("content").is_none());
}

#[tokio::test]
async fn org_users_limit_defaults_to_fifty() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request("/api/v1/admin/org/users", Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["next_cursor"].is_null());
}

#[tokio::test]
async fn org_users_rejects_limit_above_max() {
    let app = dev_app();
    let (status, json) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/users?limit=101",
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn database_auth_requires_admin_scope_for_org_users() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(
        &state,
        memory_only_api_key_record(ORG_A, "memory-only-users", "memory-only"),
    )
    .await;
    let app = create_app(state);

    let (status, json) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/users",
            Some(ORG_A),
            Some("memory-only-users"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["message"], "missing required scope");
}
