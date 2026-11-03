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

use common::{DEV_API_KEY, authorization_header};

const ORG_A: &str = "org_api_keys_a";
const ORG_B: &str = "org_api_keys_b";
const API_KEY_PEPPER: &str = "test-pepper";

const VALID_CREATE_BODY: &str = r#"{
  "name": "Production backend key",
  "scopes": ["MemoryRead", "MemoryWrite", "MemoryDelete"]
}"#;

const VALID_ADD_BODY: &str = r#"{
  "user_id": "user_123",
  "messages": [{ "role": "user", "content": "hello" }],
  "metadata": {}
}"#;

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
        scopes: vec![
            ApiKeyScope::AdminRead,
            ApiKeyScope::AdminWrite,
            ApiKeyScope::MemoryRead,
            ApiKeyScope::MemoryWrite,
        ],
        created_at: Utc::now(),
        revoked_at: None,
    }
}

fn read_only_api_key_record(org_id: &str, raw_key: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: "read-only".to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![ApiKeyScope::AdminRead, ApiKeyScope::MemoryRead],
        created_at: Utc::now(),
        revoked_at: None,
    }
}

fn memory_only_api_key_record(org_id: &str, raw_key: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: "memory-only".to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![
            ApiKeyScope::MemoryRead,
            ApiKeyScope::MemoryWrite,
            ApiKeyScope::MemoryDelete,
        ],
        created_at: Utc::now(),
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

fn request(
    method: &str,
    uri: &str,
    body: Option<&str>,
    org_id: &str,
    bearer: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Organization-ID", org_id);

    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }

    if let Some(body) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request should build")
    } else {
        builder.body(Body::empty()).expect("request should build")
    }
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
async fn create_api_key_succeeds_in_dev_auth_mode() {
    let (status, json) = response_parts(
        dev_app(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(VALID_CREATE_BODY),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert!(json["raw_key"].as_str().unwrap().starts_with("mc_live_"));
    assert_eq!(json["api_key"]["org_id"], ORG_A);
}

#[tokio::test]
async fn create_response_includes_raw_key_not_key_hash() {
    let (status, json) = response_parts(
        dev_app(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(VALID_CREATE_BODY),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json.get("raw_key").is_some());
    assert!(json["api_key"].get("key_hash").is_none());
}

#[tokio::test]
async fn list_api_keys_does_not_include_raw_key_or_key_hash() {
    let app = dev_app();
    let (create_status, create_json) = response_parts(
        app.clone(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(VALID_CREATE_BODY),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK);
    let created_id = create_json["api_key"]["id"].as_str().unwrap();

    let (status, json) = response_parts(
        app,
        request("GET", "/api/v1/api-keys", None, ORG_A, Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["api_keys"].as_array().unwrap().len(), 1);
    let item = &json["api_keys"][0];
    assert_eq!(item["id"], created_id);
    assert!(item.get("raw_key").is_none());
    assert!(item.get("key_hash").is_none());
}

#[tokio::test]
async fn revoke_api_key_succeeds_in_dev_mode() {
    let app = dev_app();
    let (_, create_json) = response_parts(
        app.clone(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(VALID_CREATE_BODY),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;
    let key_id = create_json["api_key"]["id"].as_str().unwrap();

    let (status, json) = response_parts(
        app,
        request(
            "DELETE",
            &format!("/api/v1/api-keys/{key_id}"),
            None,
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["revoked"], true);
}

#[tokio::test]
async fn revoked_api_key_cannot_authenticate_in_database_mode() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");

    let admin_raw = "admin-bootstrap";
    seed_record(&state, admin_api_key_record(ORG_A, admin_raw, "admin")).await;

    let app = create_app(state.clone());
    let (create_status, create_json) = response_parts(
        app,
        request(
            "POST",
            "/api/v1/api-keys",
            Some(VALID_CREATE_BODY),
            ORG_A,
            Some(admin_raw),
        ),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK);
    let raw_key = create_json["raw_key"].as_str().unwrap().to_string();
    let key_id = create_json["api_key"]["id"].as_str().unwrap();

    let auth_app = create_app(state.clone());
    let (auth_status, _) = response_parts(
        auth_app,
        request(
            "POST",
            "/api/v1/memories",
            Some(VALID_ADD_BODY),
            ORG_A,
            Some(&raw_key),
        ),
    )
    .await;
    assert_eq!(auth_status, StatusCode::OK);

    let revoke_app = create_app(state.clone());
    let (revoke_status, _) = response_parts(
        revoke_app,
        request(
            "DELETE",
            &format!("/api/v1/api-keys/{key_id}"),
            None,
            ORG_A,
            Some(admin_raw),
        ),
    )
    .await;
    assert_eq!(revoke_status, StatusCode::OK);

    let rejected_app = create_app(state);
    let (status, _) = response_parts(
        rejected_app,
        request(
            "POST",
            "/api/v1/memories",
            Some(VALID_ADD_BODY),
            ORG_A,
            Some(&raw_key),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invalid_scope_returns_validation_error() {
    let body = r#"{"name": "bad", "scopes": ["NotARealScope"]}"#;
    let (status, json) = response_parts(
        dev_app(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(body),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid API key scope");
}

#[tokio::test]
async fn empty_name_returns_validation_error() {
    let body = r#"{"name": "   ", "scopes": ["MemoryRead"]}"#;
    let (status, json) = response_parts(
        dev_app(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(body),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "name cannot be empty");
}

#[tokio::test]
async fn empty_scopes_returns_validation_error() {
    let body = r#"{"name": "valid", "scopes": []}"#;
    let (status, json) = response_parts(
        dev_app(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(body),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "scopes cannot be empty");
}

#[tokio::test]
async fn invalid_api_key_id_returns_validation_error() {
    let (status, json) = response_parts(
        dev_app(),
        request(
            "DELETE",
            "/api/v1/api-keys/not-a-uuid",
            None,
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid api_key_id");
}

#[tokio::test]
async fn org_a_cannot_list_org_b_keys() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(&state, admin_api_key_record(ORG_A, "admin-a", "admin-a")).await;
    seed_record(&state, admin_api_key_record(ORG_B, "admin-b", "admin-b")).await;
    seed_record(&state, memory_only_api_key_record(ORG_B, "memory-b-only")).await;

    let app = create_app(state);
    let (status, json) = response_parts(
        app,
        request("GET", "/api/v1/api-keys", None, ORG_A, Some("admin-a")),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    for item in json["api_keys"].as_array().unwrap() {
        assert_eq!(item["org_id"], ORG_A);
    }
}

#[tokio::test]
async fn org_a_cannot_revoke_org_b_key() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    let org_b_key = memory_only_api_key_record(ORG_B, "org-b-key");
    let key_id = org_b_key.id;
    seed_record(&state, admin_api_key_record(ORG_A, "admin-a", "admin-a")).await;
    seed_record(&state, org_b_key).await;

    let app = create_app(state);
    let (status, json) = response_parts(
        app,
        request(
            "DELETE",
            &format!("/api/v1/api-keys/{key_id}"),
            None,
            ORG_A,
            Some("admin-a"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn database_auth_requires_admin_write_for_create_and_revoke() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(&state, read_only_api_key_record(ORG_A, "read-only-key")).await;
    let app = create_app(state);

    let (create_status, create_json) = response_parts(
        app.clone(),
        request(
            "POST",
            "/api/v1/api-keys",
            Some(VALID_CREATE_BODY),
            ORG_A,
            Some("read-only-key"),
        ),
    )
    .await;
    assert_eq!(create_status, StatusCode::FORBIDDEN);
    assert_eq!(create_json["error"]["code"], "FORBIDDEN");
    assert_eq!(create_json["error"]["message"], "missing required scope");

    let (revoke_status, revoke_json) = response_parts(
        app,
        request(
            "DELETE",
            &format!("/api/v1/api-keys/{}", Uuid::new_v4()),
            None,
            ORG_A,
            Some("read-only-key"),
        ),
    )
    .await;
    assert_eq!(revoke_status, StatusCode::FORBIDDEN);
    assert_eq!(revoke_json["error"]["message"], "missing required scope");
}

#[tokio::test]
async fn database_auth_requires_admin_read_or_write_for_list() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(&state, memory_only_api_key_record(ORG_A, "memory-only-key")).await;
    let app = create_app(state);

    let (status, json) = response_parts(
        app,
        request(
            "GET",
            "/api/v1/api-keys",
            None,
            ORG_A,
            Some("memory-only-key"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["code"], "FORBIDDEN");
    assert_eq!(json["error"]["message"], "missing required scope");
}

#[tokio::test]
async fn database_auth_allows_admin_read_for_list() {
    let app = database_app_with_admin(ORG_A, "admin-read-list").await;
    let (status, json) = response_parts(
        app,
        request(
            "GET",
            "/api/v1/api-keys",
            None,
            ORG_A,
            Some("admin-read-list"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn health_ready_version_remain_public_metrics_disabled_by_default() {
    let app = dev_app();
    for path in ["/health", "/ready", "/api/v1/version"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK, "path {path}");
    }

    let metrics = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    assert_eq!(metrics.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn existing_protected_route_auth_still_works() {
    let body = r#"{
      "user_id": "user_123",
      "messages": [{ "role": "user", "content": "hello" }],
      "metadata": {}
    }"#;
    let (status, json) = response_parts(
        dev_app(),
        request(
            "POST",
            "/api/v1/memories",
            Some(body),
            ORG_A,
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[test]
fn dev_api_key_constant_matches_settings() {
    let (name, value) = authorization_header();
    assert_eq!(name, "Authorization");
    assert!(value.starts_with("Bearer "));
}
