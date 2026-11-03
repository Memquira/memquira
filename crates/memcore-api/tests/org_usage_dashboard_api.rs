mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use memcore_api::{AppState, ProviderWiring, create_app, create_mock_memory_engine_with_wiring};
use memcore_common::hash_api_key;
use memcore_config::{AuthMode, Settings};
use memcore_core::{
    AddMemoryInput, ApiKeyRecord, ApiKeyScope, MemoryMessage, MessageRole, ProviderCallStatus,
    ProviderUsageCapability, ProviderUsageEventRecord, ProviderUsageStore, TenantContext,
};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const ORG_ID: &str = "org_usage_api";
const USER_ID: &str = "user_usage_api";
const RAW_API_KEY: &str = "usage-api-key";
const API_KEY_PEPPER: &str = "usage-pepper";

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

fn post_request(
    path: &str,
    org_id: Option<&str>,
    with_auth: bool,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(path);
    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }
    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
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

fn persistent_state() -> (
    Settings,
    Arc<memcore_core::MemoryEngine>,
    Arc<dyn ProviderUsageStore>,
) {
    let mut settings = Settings::default();
    settings.provider_usage_persistence_enabled = true;
    let wiring = ProviderWiring::for_mock_tests(&settings);
    let store = wiring.usage_store.clone().expect("mock usage store");
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("mock engine"));
    (settings, engine, store)
}

async fn add_memory(engine: &memcore_core::MemoryEngine, content: &str) {
    let tenant = TenantContext::new(ORG_ID, USER_ID).expect("tenant");
    engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");
}

#[allow(clippy::too_many_arguments)]
async fn record_usage(
    store: &dyn ProviderUsageStore,
    org_id: &str,
    provider_name: &str,
    model_name: &str,
    capability: ProviderUsageCapability,
    created_at: chrono::DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
) {
    store
        .record_usage_event(ProviderUsageEventRecord {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            user_id: Some(USER_ID.to_string()),
            provider_name: provider_name.to_string(),
            model_name: Some(model_name.to_string()),
            capability,
            operation_name: "llm_extract_facts".to_string(),
            status: ProviderCallStatus::Success,
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            total_tokens: Some(input_tokens + output_tokens),
            retry_count: 0,
            fallback_used: false,
            circuit_blocked: false,
            timed_out: false,
            estimated_cost_usd: Some(0.001),
            metadata: None,
            created_at,
        })
        .await
        .expect("record usage");
}

#[tokio::test]
async fn dashboard_endpoint_requires_auth_and_org_header() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, _, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/usage/dashboard", Some(ORG_ID), false),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let app = create_app(AppState::new(Settings::default()));
    let (status, _, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/usage/dashboard", None, true),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dashboard_endpoint_requires_admin_scope_in_database_auth_mode() {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());

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
            .uri("/api/v1/admin/org/usage/dashboard")
            .header("X-Organization-ID", ORG_ID)
            .header("Authorization", format!("Bearer {RAW_API_KEY}"))
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn dashboard_default_days_and_custom_ranges_work_without_exposing_content() {
    let (settings, engine, store) = persistent_state();
    add_memory(&engine, "dashboard private memory").await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let app = create_app(AppState::with_memory_engine_provider_usage_and_store(
        settings,
        engine,
        memcore_providers::InMemoryProviderUsageRecorder::new(),
        Some(store),
    ));

    let (status, json, body) = response_parts(
        app.clone(),
        get_request("/api/v1/admin/org/usage/dashboard", Some(ORG_ID), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["dashboard"]["org_id"], ORG_ID);
    assert_eq!(json["dashboard"]["memory"]["total_users"], 1);
    assert!(
        json["dashboard"]["provider"]["total_requests"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(!body.contains("dashboard private memory"));

    let (status, json, _) = response_parts(
        app.clone(),
        get_request(
            "/api/v1/admin/org/usage/dashboard?days=7",
            Some(ORG_ID),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["dashboard"]["org_id"], ORG_ID);

    let path = "/api/v1/admin/org/usage/dashboard?created_after=2026-06-01T00:00:00Z&created_before=2026-06-18T00:00:00Z";
    let (status, json, _) = response_parts(app, get_request(path, Some(ORG_ID), true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["dashboard"]["window_start"], "2026-06-01T00:00:00Z");
    assert_eq!(json["dashboard"]["window_end"], "2026-06-18T00:00:00Z");
}

#[tokio::test]
async fn dashboard_date_validation_errors_use_existing_error_style() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, json, _) = response_parts(
        app.clone(),
        get_request(
            "/api/v1/admin/org/usage/dashboard?days=91",
            Some(ORG_ID),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "days must be between 1 and 90");

    let (status, json, _) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/usage/dashboard?created_after=not-a-date&created_before=2026-06-18T00:00:00Z",
            Some(ORG_ID),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid created_after timestamp");
}

#[tokio::test]
async fn memory_snapshot_endpoints_require_auth_and_org_header() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, _, _) = response_parts(
        app,
        post_request(
            "/api/v1/admin/org/usage/memory/snapshots",
            Some(ORG_ID),
            false,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let app = create_app(AppState::new(Settings::default()));
    let (status, _, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/usage/memory/snapshots", None, true),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn memory_snapshot_endpoints_require_admin_scopes_in_database_auth_mode() {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());

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
    let create_request = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/org/usage/memory/snapshots")
        .header("X-Organization-ID", ORG_ID)
        .header("Authorization", format!("Bearer {RAW_API_KEY}"))
        .body(Body::empty())
        .expect("request");
    let (status, _, _) = response_parts(app.clone(), create_request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let list_request = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/org/usage/memory/snapshots")
        .header("X-Organization-ID", ORG_ID)
        .header("Authorization", format!("Bearer {RAW_API_KEY}"))
        .body(Body::empty())
        .expect("request");
    let (status, _, _) = response_parts(app, list_request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn memory_snapshot_create_list_and_dashboard_latest_work_without_exposing_content() {
    let (settings, engine, store) = persistent_state();
    add_memory(&engine, "snapshot private memory").await;
    let app = create_app(AppState::with_memory_engine_provider_usage_and_store(
        settings,
        engine,
        memcore_providers::InMemoryProviderUsageRecorder::new(),
        Some(store),
    ));

    let older_body = json!({ "captured_at": "2026-06-17T10:00:00Z" });
    let newer_body = json!({ "captured_at": "2026-06-18T10:00:00Z" });
    let (status, json, body) = response_parts(
        app.clone(),
        post_request(
            "/api/v1/admin/org/usage/memory/snapshots",
            Some(ORG_ID),
            true,
            Some(older_body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["snapshot"]["org_id"], ORG_ID);
    assert_eq!(json["snapshot"]["total_users"], 1);
    assert_eq!(json["snapshot"]["active_memories"], 1);
    assert!(!body.contains("snapshot private memory"));

    let (status, _, _) = response_parts(
        app.clone(),
        post_request(
            "/api/v1/admin/org/usage/memory/snapshots",
            Some(ORG_ID),
            true,
            Some(newer_body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let list_path = "/api/v1/admin/org/usage/memory/snapshots?created_after=2026-06-17T00:00:00Z&created_before=2026-06-19T00:00:00Z&limit=1";
    let (status, json, body) =
        response_parts(app.clone(), get_request(list_path, Some(ORG_ID), true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["snapshots"].as_array().expect("snapshots").len(), 1);
    assert_eq!(json["snapshots"][0]["captured_at"], "2026-06-18T10:00:00Z");
    assert!(json["next_cursor"].is_string());
    assert!(!body.contains("snapshot private memory"));

    let (status, json, _) = response_parts(
        app,
        get_request("/api/v1/admin/org/usage/dashboard", Some(ORG_ID), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["dashboard"]["memory"]["latest_snapshot"]["captured_at"],
        "2026-06-18T10:00:00Z"
    );
    assert_eq!(
        json["dashboard"]["memory"]["latest_snapshot"]["total_memories"],
        1
    );
}

#[tokio::test]
async fn memory_snapshot_validation_errors_use_existing_error_style() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, json, _) = response_parts(
        app.clone(),
        post_request(
            "/api/v1/admin/org/usage/memory/snapshots",
            Some(ORG_ID),
            true,
            Some(json!({ "captured_at": "not-a-date" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid captured_at timestamp");

    let (status, json, _) = response_parts(
        app.clone(),
        get_request(
            "/api/v1/admin/org/usage/memory/snapshots?created_after=2026-06-19T00:00:00Z&created_before=2026-06-18T00:00:00Z",
            Some(ORG_ID),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json["error"]["message"],
        "created_after must be earlier than created_before"
    );

    let (status, json, _) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/usage/memory/snapshots?limit=101",
            Some(ORG_ID),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "limit cannot exceed 100");
}

#[tokio::test]
async fn provider_daily_endpoint_filters_and_preserves_privacy() {
    let (settings, engine, store) = persistent_state();
    let start = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    record_usage(
        store.as_ref(),
        ORG_ID,
        "mock",
        "model-a",
        ProviderUsageCapability::Llm,
        start,
        10,
        2,
    )
    .await;
    record_usage(
        store.as_ref(),
        ORG_ID,
        "other",
        "model-b",
        ProviderUsageCapability::Embedding,
        start + chrono::Duration::days(1),
        20,
        0,
    )
    .await;
    record_usage(
        store.as_ref(),
        "org_other_usage_api",
        "mock",
        "model-a",
        ProviderUsageCapability::Llm,
        start,
        999,
        999,
    )
    .await;
    let app = create_app(AppState::with_memory_engine_provider_usage_and_store(
        settings,
        engine,
        memcore_providers::InMemoryProviderUsageRecorder::new(),
        Some(store),
    ));

    let path = "/api/v1/admin/org/usage/provider/daily?created_after=2026-06-01T00:00:00Z&created_before=2026-06-04T00:00:00Z&provider_name=mock&model_name=model-a&capability=llm";
    let (status, json, body) = response_parts(app, get_request(path, Some(ORG_ID), true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["usage"]["org_id"], ORG_ID);
    assert_eq!(
        json["usage"]["buckets"].as_array().expect("buckets").len(),
        1
    );
    assert_eq!(json["usage"]["buckets"][0]["total_requests"], 1);
    assert_eq!(json["usage"]["buckets"][0]["total_tokens"], 12);
    assert!(!body.contains("prompt"));
    assert!(!body.contains("Authorization"));
}

#[tokio::test]
async fn provider_daily_endpoint_rejects_invalid_capability_and_requires_auth() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, _, _) = response_parts(
        app.clone(),
        get_request(
            "/api/v1/admin/org/usage/provider/daily",
            Some(ORG_ID),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, json, _) = response_parts(
        app,
        get_request(
            "/api/v1/admin/org/usage/provider/daily?capability=bad",
            Some(ORG_ID),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid capability: bad");
}
