mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_common::hash_api_key;
use memcore_config::{AuthMode, Settings};
use memcore_core::{ApiKeyRecord, ApiKeyScope};
use tower::ServiceExt;
use uuid::Uuid;

use common::DEV_API_KEY;

const VALID_ADD_BODY: &str = r#"{
  "user_id": "user_123",
  "messages": [{ "role": "user", "content": "hello" }],
  "metadata": {}
}"#;

const ORG_A: &str = "org_db_auth_a";
const RAW_API_KEY: &str = "database-test-key";
const API_KEY_PEPPER: &str = "test-pepper";

fn sample_api_key_record(raw_key: &str, name: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: ORG_A.to_string(),
        name: name.to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![
            ApiKeyScope::MemoryRead,
            ApiKeyScope::MemoryWrite,
            ApiKeyScope::MemoryDelete,
            ApiKeyScope::UserDelete,
            ApiKeyScope::AuditRead,
        ],
        created_at: Utc::now(),
        revoked_at: None,
    }
}

async fn seed_database_api_key(state: &AppState, raw_key: &str, name: &str) {
    state
        .api_key_store
        .insert_api_key(sample_api_key_record(raw_key, name))
        .await
        .expect("api key should be inserted");
}

fn database_auth_settings() -> Settings {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());
    settings.dev_api_key = String::new();
    settings
}

async fn database_auth_app() -> axum::Router {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_database_api_key(&state, RAW_API_KEY, "integration-test").await;

    create_app(state)
}

fn post_request(uri: &str, body: &str, org_id: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id);

    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
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
async fn dev_auth_still_works() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, _) = response_parts(
        app,
        post_request(
            "/api/v1/memories",
            VALID_ADD_BODY,
            "org_dev",
            Some(DEV_API_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn database_auth_succeeds_with_valid_stored_key() {
    let app = database_auth_app().await;
    let (status, _) = response_parts(
        app,
        post_request("/api/v1/memories", VALID_ADD_BODY, ORG_A, Some(RAW_API_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn database_auth_rejects_missing_bearer_token() {
    let app = database_auth_app().await;
    let (status, json) = response_parts(
        app,
        post_request("/api/v1/memories", VALID_ADD_BODY, ORG_A, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn database_auth_rejects_invalid_key() {
    let app = database_auth_app().await;
    let (status, _) = response_parts(
        app,
        post_request(
            "/api/v1/memories",
            VALID_ADD_BODY,
            ORG_A,
            Some("not-a-valid-key"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn database_auth_rejects_org_header_mismatch() {
    let app = database_auth_app().await;
    let (status, json) = response_parts(
        app,
        post_request(
            "/api/v1/memories",
            VALID_ADD_BODY,
            "org_other",
            Some(RAW_API_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["code"], "FORBIDDEN");
}

#[tokio::test]
async fn database_auth_allows_matching_org_header() {
    let app = database_auth_app().await;
    let (status, _) = response_parts(
        app,
        post_request("/api/v1/memories", VALID_ADD_BODY, ORG_A, Some(RAW_API_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn database_auth_rejects_revoked_key() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    let record = sample_api_key_record("revoked-key", "revoked");
    let key_id = record.id;
    state
        .api_key_store
        .insert_api_key(record)
        .await
        .expect("insert should succeed");
    state
        .api_key_store
        .revoke_api_key(ORG_A, key_id)
        .await
        .expect("revoke should succeed");

    let app = create_app(state);
    let (status, _) = response_parts(
        app,
        post_request(
            "/api/v1/memories",
            VALID_ADD_BODY,
            ORG_A,
            Some("revoked-key"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
