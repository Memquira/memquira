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

const ORG_A: &str = "org_admin_audit_a";
const ORG_B: &str = "org_admin_audit_b";
const USER_A: &str = "user_admin_audit_a";
const USER_B: &str = "user_admin_audit_b";
const MEMORY_CONTENT: &str = "Admin audit search test content";

const API_KEY_PEPPER: &str = "admin-audit-test-pepper";

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

fn admin_api_key_record(org_id: &str, raw_key: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: "admin-key".to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![ApiKeyScope::AdminRead, ApiKeyScope::MemoryWrite],
        created_at: chrono::Utc::now(),
        revoked_at: None,
    }
}

fn audit_read_api_key_record(org_id: &str, raw_key: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: "audit-read-key".to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes: vec![ApiKeyScope::AuditRead, ApiKeyScope::MemoryWrite],
        created_at: chrono::Utc::now(),
        revoked_at: None,
    }
}

fn memory_only_api_key_record(org_id: &str, raw_key: &str) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: "memory-only".to_string(),
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
) -> Uuid {
    let add_body = format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{}}
        }}"#
    );
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id);

    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    } else {
        let (auth_name, auth_value) = authorization_header();
        builder = builder.header(auth_name, auth_value);
    }

    let (status, json) = response_parts(
        app.clone(),
        builder
            .body(Body::from(add_body))
            .expect("request should build"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    json["memories"][0]["id"]
        .as_str()
        .expect("memory id")
        .parse()
        .expect("valid uuid")
}

fn admin_audit_uri(query: &str) -> String {
    if query.is_empty() {
        "/api/v1/admin/org/memory-events".to_string()
    } else {
        format!("/api/v1/admin/org/memory-events?{query}")
    }
}

#[tokio::test]
async fn admin_audit_requires_authorization() {
    let app = dev_app();
    let (status, _) =
        response_parts(app, get_request(&admin_audit_uri(""), Some(ORG_A), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_audit_requires_organization_header() {
    let app = dev_app();
    let (status, _) = response_parts(
        app,
        get_request(&admin_audit_uri(""), None, Some(DEV_API_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_audit_works_in_dev_auth_mode() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_audit_returns_events_across_users_in_org() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;
    seed_memory(&app, ORG_A, USER_B, "second user memory", None).await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert!(events.len() >= 2);
    let user_ids: Vec<&str> = events
        .iter()
        .filter_map(|event| event["user_id"].as_str())
        .collect();
    assert!(user_ids.contains(&USER_A));
    assert!(user_ids.contains(&USER_B));
}

#[tokio::test]
async fn admin_audit_user_id_filter() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;
    seed_memory(
        &app,
        ORG_A,
        USER_B,
        "Other user memory content for audit test",
        None,
    )
    .await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri(&format!("user_id={USER_A}")),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["user_id"] == USER_A));
}

#[tokio::test]
async fn admin_audit_fact_id_filter() {
    let app = dev_app();
    let fact_id = seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri(&format!("fact_id={fact_id}")),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|event| event["fact_id"].as_str() == Some(fact_id.to_string().as_str()))
    );
}

#[tokio::test]
async fn admin_audit_operation_filter() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("operation=Add"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["operation"] == "Add"));
}

#[tokio::test]
async fn admin_audit_invalid_fact_id_returns_validation_error() {
    let app = dev_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("fact_id=not-a-uuid"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid fact_id");
}

#[tokio::test]
async fn admin_audit_invalid_operation_returns_validation_error() {
    let app = dev_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("operation=InvalidOp"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid operation");
}

#[tokio::test]
async fn admin_audit_does_not_expose_input_text() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    for event in json["events"].as_array().unwrap() {
        assert!(event.get("input_text").is_none());
    }
}

#[tokio::test]
async fn org_a_cannot_see_org_b_audit_events() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;
    seed_memory(
        &app,
        ORG_B,
        USER_B,
        "Organization B memory content for audit test",
        None,
    )
    .await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        json["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["user_id"] != USER_B)
    );
}

#[tokio::test]
async fn database_auth_requires_admin_or_audit_scope() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(
        &state,
        memory_only_api_key_record(ORG_A, "memory-only-audit"),
    )
    .await;
    let app = create_app(state);

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some("memory-only-audit")),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["message"], "missing required scope");
}

#[tokio::test]
async fn database_auth_allows_audit_read_scope() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(&state, audit_read_api_key_record(ORG_A, "audit-read-key")).await;
    let app = create_app(state);

    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, Some("audit-read-key")).await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some("audit-read-key")),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn database_auth_allows_admin_read_scope() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("app state should initialize");
    seed_record(&state, admin_api_key_record(ORG_A, "admin-read-audit")).await;
    let app = create_app(state);

    seed_memory(
        &app,
        ORG_A,
        USER_A,
        MEMORY_CONTENT,
        Some("admin-read-audit"),
    )
    .await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri(""), Some(ORG_A), Some("admin-read-audit")),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn admin_audit_limit_defaults_and_rejects_above_max() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app.clone(),
        get_request(&admin_audit_uri(""), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["next_cursor"].is_null());

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("limit=101"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn admin_audit_invalid_cursor_returns_validation_error() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("cursor=opaque-token"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid cursor");
}

#[tokio::test]
async fn admin_audit_accepts_created_after() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("created_after=2020-01-01T00:00:00Z"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_audit_accepts_created_before() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("created_before=2099-01-01T00:00:00Z"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_audit_accepts_both_date_filters() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, None).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri(
                "created_after=2020-01-01T00:00:00Z&created_before=2099-01-01T00:00:00Z",
            ),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_audit_invalid_created_after_returns_validation_error() {
    let app = dev_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("created_after=not-valid"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid created_after timestamp");
}

#[tokio::test]
async fn admin_audit_invalid_created_before_returns_validation_error() {
    let app = dev_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri("created_before=not-valid"),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid created_before timestamp");
}

#[tokio::test]
async fn admin_audit_invalid_date_range_returns_validation_error() {
    let app = dev_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &admin_audit_uri(
                "created_after=2026-06-01T00:00:00Z&created_before=2026-01-01T00:00:00Z",
            ),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json["error"]["message"],
        "created_after must be earlier than created_before"
    );
}

#[tokio::test]
async fn admin_audit_keyword_search_finds_matching_events() {
    let app = dev_app();
    seed_memory(
        &app,
        ORG_A,
        USER_A,
        "Rust audit keyword content",
        Some(DEV_API_KEY),
    )
    .await;
    seed_memory(
        &app,
        ORG_A,
        USER_B,
        "python only content",
        Some(DEV_API_KEY),
    )
    .await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri("q=rust"), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
    for event in json["events"].as_array().unwrap() {
        assert!(event.get("input_text").is_none());
    }
}

#[tokio::test]
async fn admin_audit_empty_q_behaves_like_no_search() {
    let app = dev_app();
    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT, Some(DEV_API_KEY)).await;

    let (status, json) = response_parts(
        app,
        get_request(&admin_audit_uri("q=%20%20"), Some(ORG_A), Some(DEV_API_KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_audit_long_q_returns_validation_error() {
    let long = "a".repeat(201);
    let (status, json) = response_parts(
        dev_app(),
        get_request(
            &admin_audit_uri(&format!("q={long}")),
            Some(ORG_A),
            Some(DEV_API_KEY),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "q must be 200 characters or less");
}
