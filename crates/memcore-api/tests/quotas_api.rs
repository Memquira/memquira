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

const ORG_ID: &str = "org_quotas_api";
const USER_ID: &str = "user_quotas_api";
const RAW_API_KEY: &str = "quotas-api-key";
const API_KEY_PEPPER: &str = "quotas-pepper";

fn quota_settings() -> Settings {
    Settings {
        quotas_enabled: true,
        max_users_per_org: 10,
        max_memories_per_user: 10,
        max_memories_per_org: 10,
        daily_provider_request_limit: 100,
        daily_provider_token_limit: 1000,
        ..Settings::default()
    }
}

fn get_request(path: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }
    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }
    builder.body(Body::empty()).expect("request")
}

fn put_plan(body: &str, org_id: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri("/api/v1/admin/org/plan")
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn delete_plan(org_id: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri("/api/v1/admin/org/plan")
        .header("X-Organization-ID", org_id)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .expect("request")
}

fn post_memory(content: &str, org_id: &str, user_id: &str, bearer: &str) -> Request<Body> {
    let body = format!(
        r#"{{
            "user_id": "{user_id}",
            "messages": [{{ "role": "user", "content": "{content}" }}],
            "metadata": {{}}
        }}"#
    );
    Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(body))
        .expect("request")
}

async fn response_parts(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value, String) {
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body_text = String::from_utf8_lossy(&body).to_string();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, json, body_text)
}

async fn seed_memory(app: &axum::Router, content: &str, user_id: &str) {
    let (status, _, _) = response_parts(
        app.clone(),
        post_memory(content, ORG_ID, user_id, DEV_API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn quotas_requires_authorization() {
    let app = create_app(AppState::new(quota_settings()));
    let (status, _, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/quotas", Some(ORG_ID), false),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn quotas_requires_organization_header() {
    let app = create_app(AppState::new(quota_settings()));
    let (status, _, _) =
        response_parts(app, get_request("/api/v1/admin/org/quotas", None, true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn quotas_requires_admin_scope_in_database_auth_mode() {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());
    settings.quotas_enabled = true;

    let state = AppState::initialize(settings).await.expect("initialize");
    state
        .api_key_store
        .insert_api_key(ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: ORG_ID.to_string(),
            name: "memory-only".to_string(),
            key_hash: hash_api_key(API_KEY_PEPPER, RAW_API_KEY),
            scopes: vec![ApiKeyScope::MemoryRead],
            created_at: Utc::now(),
            revoked_at: None,
        })
        .await
        .expect("insert key");

    let app = create_app(state);
    let (status, _, _) = response_parts(
        app,
        Request::builder()
            .method("GET")
            .uri("/api/v1/admin/org/quotas")
            .header("X-Organization-ID", ORG_ID)
            .header("Authorization", format!("Bearer {RAW_API_KEY}"))
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn quotas_returns_configured_limits_and_org_usage_without_content() {
    let app = create_app(AppState::new(quota_settings()));
    seed_memory(&app, "sensitive quota memory", USER_ID).await;

    let (status, json, body) = response_parts(
        app,
        get_request("/api/v1/admin/org/quotas", Some(ORG_ID), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["quotas"]["source"], "global_config");
    assert_eq!(json["quotas"]["limits"]["enabled"], true);
    assert_eq!(json["quotas"]["limits"]["max_memories_per_org"], 10);
    assert_eq!(json["quotas"]["usage"]["org_id"], ORG_ID);
    assert_eq!(json["quotas"]["usage"]["total_users"], 1);
    assert_eq!(json["quotas"]["usage"]["total_memories"], 1);
    assert!(!body.contains("sensitive quota memory"));
}

#[tokio::test]
async fn quotas_optional_user_id_returns_user_memory_count() {
    let app = create_app(AppState::new(quota_settings()));
    seed_memory(&app, "user count one", USER_ID).await;

    let (status, json, _) = response_parts(
        app,
        get_request(
            &format!("/api/v1/admin/org/quotas?user_id={USER_ID}"),
            Some(ORG_ID),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["quotas"]["usage"]["user_memory_count"], 1);
}

#[tokio::test]
async fn memory_write_succeeds_when_under_quota() {
    let mut settings = quota_settings();
    settings.max_memories_per_org = 2;
    settings.max_memories_per_user = 2;
    let app = create_app(AppState::new(settings));

    let (status, _, _) = response_parts(
        app,
        post_memory("under quota", ORG_ID, USER_ID, DEV_API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn memory_write_returns_quota_exceeded_for_org_limit() {
    let mut settings = quota_settings();
    settings.max_memories_per_org = 1;
    settings.max_memories_per_user = 10;
    let app = create_app(AppState::new(settings));
    seed_memory(&app, "first org memory", USER_ID).await;

    let (status, json, _) = response_parts(
        app,
        post_memory("blocked org memory", ORG_ID, "other_user", DEV_API_KEY),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["error"]["code"], "QUOTA_EXCEEDED");
    assert_eq!(json["error"]["details"]["kind"], "MemoriesPerOrg");
}

#[tokio::test]
async fn memory_write_returns_quota_exceeded_for_user_limit() {
    let mut settings = quota_settings();
    settings.max_memories_per_org = 10;
    settings.max_memories_per_user = 1;
    let app = create_app(AppState::new(settings));
    seed_memory(&app, "first user memory", USER_ID).await;

    let (status, json, _) = response_parts(
        app,
        post_memory("blocked user memory", ORG_ID, USER_ID, DEV_API_KEY),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["error"]["code"], "QUOTA_EXCEEDED");
    assert_eq!(json["error"]["details"]["kind"], "MemoriesPerUser");
}

#[tokio::test]
async fn quota_disabled_mode_preserves_existing_behavior() {
    let settings = Settings {
        quotas_enabled: false,
        max_memories_per_org: 1,
        max_memories_per_user: 1,
        ..Settings::default()
    };
    let app = create_app(AppState::new(settings));
    seed_memory(&app, "first disabled quota memory", USER_ID).await;

    let (status, _, _) = response_parts(
        app,
        post_memory("second disabled quota memory", ORG_ID, USER_ID, DEV_API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn org_plan_get_put_delete_and_quota_source() {
    let mut settings = quota_settings();
    settings.max_memories_per_org = 10;
    let app = create_app(AppState::new(settings));

    let (status, json, _) = response_parts(
        app.clone(),
        get_request("/api/v1/admin/org/plan", Some(ORG_ID), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["plan"], serde_json::Value::Null);
    assert_eq!(json["resolved_source"], "global_config");

    let body = r#"{
        "tier": "Pro",
        "limits": {
            "max_users_per_org": 100,
            "max_memories_per_user": 5000,
            "max_memories_per_org": 1,
            "daily_provider_request_limit": 10000,
            "daily_provider_token_limit": 5000000
        },
        "is_active": true,
        "metadata": {"note": "admin-set"}
    }"#;
    let (status, json, body_text) =
        response_parts(app.clone(), put_plan(body, ORG_ID, DEV_API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["plan"]["tier"], "Pro");
    assert_eq!(json["plan"]["limits"]["max_memories_per_org"], 1);
    assert!(!body_text.contains("payment"));

    let (status, json, _) = response_parts(
        app.clone(),
        get_request("/api/v1/admin/org/quotas", Some(ORG_ID), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["quotas"]["source"], "org_plan");
    assert_eq!(json["quotas"]["limits"]["max_memories_per_org"], 1);

    seed_memory(&app, "first plan-limited memory", USER_ID).await;
    let (status, json, _) = response_parts(
        app.clone(),
        post_memory("blocked by org plan", ORG_ID, "other_user", DEV_API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["error"]["details"]["kind"], "MemoriesPerOrg");

    let (status, json, _) = response_parts(app.clone(), delete_plan(ORG_ID, DEV_API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["deleted"], true);

    let (status, json, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/quotas", Some(ORG_ID), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["quotas"]["source"], "global_config");
    assert_eq!(json["quotas"]["limits"]["max_memories_per_org"], 10);
}

#[tokio::test]
async fn org_plan_rejects_invalid_tier_negative_limit_and_secret_metadata() {
    let app = create_app(AppState::new(quota_settings()));

    let invalid_tier = r#"{
        "tier": "Gold",
        "limits": {},
        "is_active": true
    }"#;
    let (status, _, _) =
        response_parts(app.clone(), put_plan(invalid_tier, ORG_ID, DEV_API_KEY)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let negative_limit = r#"{
        "tier": "Pro",
        "limits": {"max_users_per_org": -1},
        "is_active": true
    }"#;
    let (status, _, _) =
        response_parts(app.clone(), put_plan(negative_limit, ORG_ID, DEV_API_KEY)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let secret_metadata = r#"{
        "tier": "Pro",
        "limits": {},
        "is_active": true,
        "metadata": {"api_key": "do-not-store"}
    }"#;
    let (status, _, _) = response_parts(app, put_plan(secret_metadata, ORG_ID, DEV_API_KEY)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
