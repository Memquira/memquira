mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use memcore_api::{AppState, ProviderWiring, create_app, create_mock_memory_engine_with_wiring};
use memcore_common::hash_api_key;
use memcore_config::{AuthMode, Settings};
use memcore_core::{
    ApiKeyRecord, ApiKeyScope, ProviderCallStatus, ProviderUsageCapability,
    ProviderUsageEventRecord, ProviderUsageStore,
};
use tower::ServiceExt;
use uuid::Uuid;

use common::{DEV_API_KEY, authorization_header};

const ORG_A: &str = "org_pu_retention_a";
const ORG_B: &str = "org_pu_retention_b";
const RAW_API_KEY: &str = "pu-retention-test-key";
const API_KEY_PEPPER: &str = "pu-retention-pepper";
const RETENTION_PATH: &str = "/api/v1/admin/org/provider-usage/retention/apply";

fn sample_event(org_id: &str, created_at: chrono::DateTime<Utc>) -> ProviderUsageEventRecord {
    ProviderUsageEventRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        user_id: Some("user_a".to_string()),
        provider_name: "mock".to_string(),
        model_name: Some("mock-llm".to_string()),
        capability: ProviderUsageCapability::Llm,
        operation_name: "llm_extract_facts".to_string(),
        status: ProviderCallStatus::Success,
        input_tokens: Some(10),
        output_tokens: Some(2),
        total_tokens: Some(12),
        retry_count: 0,
        fallback_used: false,
        circuit_blocked: false,
        timed_out: false,
        estimated_cost_usd: None,
        metadata: None,
        created_at,
    }
}

fn persistence_app() -> (axum::Router, Arc<dyn ProviderUsageStore>) {
    let mut settings = Settings::default();
    settings.provider_usage_persistence_enabled = true;
    let wiring = ProviderWiring::for_mock_tests(&settings);
    let store = wiring.usage_store.clone().expect("mock store");
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));
    let app = create_app(AppState::with_memory_engine_provider_usage_and_store(
        settings,
        engine,
        wiring.usage_recorder,
        Some(store.clone()),
    ));
    (app, store)
}

fn post_retention(body: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(RETENTION_PATH)
        .header("content-type", "application/json");
    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }
    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn response_parts(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, json)
}

async fn seed_events(store: Arc<dyn ProviderUsageStore>, org_id: &str) {
    let now = Utc::now();
    let old = now - chrono::Duration::days(60);
    let recent = now - chrono::Duration::days(1);
    store
        .record_usage_event(sample_event(org_id, old))
        .await
        .expect("record old");
    store
        .record_usage_event(sample_event(org_id, recent))
        .await
        .expect("record recent");
}

#[tokio::test]
async fn endpoint_requires_auth() {
    let (app, _) = persistence_app();
    let (status, _) = response_parts(
        app,
        post_retention(r#"{"retention_days":30}"#, Some(ORG_A), false),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn endpoint_requires_organization_header() {
    let (app, _) = persistence_app();
    let (status, _) =
        response_parts(app, post_retention(r#"{"retention_days":30}"#, None, true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dry_run_defaults_to_true() {
    let (app, store) = persistence_app();
    seed_events(store.clone(), ORG_A).await;

    let (status, json) = response_parts(
        app,
        post_retention(r#"{"retention_days":30}"#, Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["dry_run"], true);
    assert!(json["summary"]["matched_events"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(json["summary"]["deleted_events"], 0);
}

#[tokio::test]
async fn dry_run_does_not_delete_events() {
    let (app, store) = persistence_app();
    seed_events(store.clone(), ORG_A).await;

    let (status, _) = response_parts(
        app.clone(),
        post_retention(r#"{"dry_run":true,"retention_days":30}"#, Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let result = store
        .query_usage(memcore_core::ProviderUsageQuery::new(ORG_A, 10))
        .await
        .expect("query");
    assert_eq!(result.events.len(), 2);
}

#[tokio::test]
async fn non_dry_run_deletes_old_events() {
    let (app, store) = persistence_app();
    seed_events(store.clone(), ORG_A).await;

    let (status, json) = response_parts(
        app,
        post_retention(
            r#"{"dry_run":false,"retention_days":30}"#,
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["dry_run"], false);
    assert!(json["summary"]["deleted_events"].as_u64().unwrap_or(0) >= 1);

    let result = store
        .query_usage(memcore_core::ProviderUsageQuery::new(ORG_A, 10))
        .await
        .expect("query");
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.summary.total_requests, 1);
}

#[tokio::test]
async fn omitted_retention_days_uses_config_default() {
    let (app, store) = persistence_app();
    seed_events(store.clone(), ORG_A).await;

    let (status, json) = response_parts(
        app,
        post_retention(r#"{"dry_run":true}"#, Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["summary"]["cutoff"].is_string());
}

#[tokio::test]
async fn retention_days_zero_disables_cleanup() {
    let (app, store) = persistence_app();
    seed_events(store.clone(), ORG_A).await;

    let (status, json) = response_parts(
        app,
        post_retention(r#"{"dry_run":false,"retention_days":0}"#, Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["matched_events"], 0);
    assert_eq!(json["summary"]["deleted_events"], 0);

    let result = store
        .query_usage(memcore_core::ProviderUsageQuery::new(ORG_A, 10))
        .await
        .expect("query");
    assert_eq!(result.events.len(), 2);
}

#[tokio::test]
async fn invalid_retention_days_returns_validation_error() {
    let (app, _) = persistence_app();
    let (status, _) = response_parts(
        app,
        post_retention(r#"{"dry_run":true,"retention_days":-1}"#, Some(ORG_A), true),
    )
    .await;
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "unexpected status: {status}"
    );
}

#[tokio::test]
async fn endpoint_only_affects_current_org() {
    let (app, store) = persistence_app();
    seed_events(store.clone(), ORG_A).await;
    seed_events(store.clone(), ORG_B).await;

    let (status, _) = response_parts(
        app,
        post_retention(
            r#"{"dry_run":false,"retention_days":30}"#,
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let org_b = store
        .query_usage(memcore_core::ProviderUsageQuery::new(ORG_B, 10))
        .await
        .expect("org_b");
    assert_eq!(org_b.events.len(), 2);
}

#[tokio::test]
async fn endpoint_requires_admin_write_in_database_auth_mode() {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());
    settings.provider_usage_persistence_enabled = true;

    let state = AppState::initialize(settings).await.expect("initialize");
    state
        .api_key_store
        .insert_api_key(ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: ORG_A.to_string(),
            name: "read-only".to_string(),
            key_hash: hash_api_key(API_KEY_PEPPER, RAW_API_KEY),
            scopes: vec![ApiKeyScope::AdminRead],
            created_at: Utc::now(),
            revoked_at: None,
        })
        .await
        .expect("insert key");

    let app = create_app(state);
    let (status, _) = response_parts(
        app,
        Request::builder()
            .method("POST")
            .uri(RETENTION_PATH)
            .header("content-type", "application/json")
            .header("X-Organization-ID", ORG_A)
            .header("Authorization", format!("Bearer {RAW_API_KEY}"))
            .body(Body::from(r#"{"dry_run":true,"retention_days":30}"#))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn dev_auth_allows_retention_without_scope_checks() {
    let (app, _) = persistence_app();
    let (status, _) = response_parts(
        app,
        Request::builder()
            .method("POST")
            .uri(RETENTION_PATH)
            .header("content-type", "application/json")
            .header("X-Organization-ID", ORG_A)
            .header("Authorization", format!("Bearer {DEV_API_KEY}"))
            .body(Body::from(r#"{"dry_run":true,"retention_days":30}"#))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
