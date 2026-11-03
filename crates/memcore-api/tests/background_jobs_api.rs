mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_common::hash_api_key;
use memcore_config::{AuthMode, Settings};
use memcore_core::{
    ApiKeyRecord, ApiKeyScope, BackgroundJobKind, BackgroundJobStatus, ProviderCallStatus,
    ProviderUsageCapability, ProviderUsageEventRecord, ProviderUsageQuery, StoredBackgroundJobRun,
};
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const ORG_A: &str = "org_jobs_a";
const ORG_B: &str = "org_jobs_b";
const RAW_API_KEY: &str = "jobs-api-key";
const API_KEY_PEPPER: &str = "jobs-pepper";

fn request(method: &str, uri: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }
    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }
    builder.body(Body::empty()).expect("request")
}

fn bearer_request(method: &str, uri: &str, org_id: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Organization-ID", org_id)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn json_request(method: &str, uri: &str, org_id: &str, body: serde_json::Value) -> Request<Body> {
    let bytes = serde_json::to_vec(&body).expect("json body");
    let (name, value) = authorization_header();
    Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Organization-ID", org_id)
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(bytes))
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
    let text = String::from_utf8_lossy(&body).to_string();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, json, text)
}

fn jobs_settings() -> Settings {
    let mut settings = Settings::default();
    settings.background_job_org_ids = vec![ORG_A.to_string()];
    settings.memory_usage_snapshot_job_enabled = false;
    settings.provider_usage_retention_job_enabled = false;
    settings
}

fn locked_jobs_settings() -> Settings {
    let mut settings = jobs_settings();
    settings.background_job_lock_enabled = true;
    settings.background_job_lock_owner_id = Some("instance-a".to_string());
    settings
}

fn database_auth_settings() -> Settings {
    let mut settings = Settings::sqlite_memory();
    settings.auth_mode = AuthMode::Database;
    settings.api_key_pepper = Some(API_KEY_PEPPER.to_string());
    settings.dev_api_key = String::new();
    settings.background_job_org_ids = vec![ORG_A.to_string()];
    settings
}

fn api_key_record(org_id: &str, raw_key: &str, scopes: Vec<ApiKeyScope>) -> ApiKeyRecord {
    ApiKeyRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        name: "jobs-key".to_string(),
        key_hash: hash_api_key(API_KEY_PEPPER, raw_key),
        scopes,
        created_at: Utc::now(),
        revoked_at: None,
    }
}

#[tokio::test]
async fn get_jobs_requires_auth_and_org_header() {
    let app = create_app(AppState::new(Settings::default()));

    let (status, json, _) = response_parts(
        app.clone(),
        request("GET", "/api/v1/admin/jobs", Some(ORG_A), false),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");

    let (status, json, _) =
        response_parts(app, request("GET", "/api/v1/admin/jobs", None, true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn get_job_runs_requires_auth_and_org_header() {
    let app = create_app(AppState::new(Settings::default()));

    let (status, json, _) = response_parts(
        app.clone(),
        request("GET", "/api/v1/admin/jobs/runs", Some(ORG_A), false),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");

    let (status, json, _) =
        response_parts(app, request("GET", "/api/v1/admin/jobs/runs", None, true)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn get_jobs_returns_disabled_defaults() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, json, _) =
        response_parts(app, request("GET", "/api/v1/admin/jobs", Some(ORG_A), true)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["jobs"]["jobs_enabled"], false);
    assert_eq!(json["jobs"]["jobs"].as_array().expect("jobs").len(), 3);
    assert!(
        json["jobs"]["recent_runs"]
            .as_array()
            .expect("runs")
            .is_empty()
    );
}

#[tokio::test]
async fn lock_owner_id_is_generated_when_empty() {
    let mut settings = jobs_settings();
    settings.background_job_lock_enabled = true;
    settings.background_job_lock_owner_id = None;
    let state = AppState::new(settings);

    let owner_id = state
        .background_job_lock_owner_id
        .as_deref()
        .expect("owner id should be generated");
    assert!(Uuid::parse_str(owner_id).is_ok());
}

#[tokio::test]
async fn get_jobs_shows_lock_status_when_enabled() {
    let state = AppState::new(locked_jobs_settings());
    let store = state
        .background_job_lock_store
        .clone()
        .expect("lock store should be configured");
    store
        .try_acquire_lock(
            BackgroundJobKind::MemoryUsageSnapshot,
            "instance-a",
            std::time::Duration::from_secs(300),
        )
        .await
        .expect("lock acquire");
    let app = create_app(state);

    let (status, json, body) =
        response_parts(app, request("GET", "/api/v1/admin/jobs", Some(ORG_A), true)).await;
    assert_eq!(status, StatusCode::OK);
    let memory_job = json["jobs"]["jobs"]
        .as_array()
        .expect("jobs")
        .iter()
        .find(|job| job["kind"] == "MemoryUsageSnapshot")
        .expect("memory job");
    assert_eq!(memory_job["lock"]["enabled"], true);
    assert_eq!(memory_job["lock"]["owner_id"], "instance-a");
    assert_eq!(memory_job["lock"]["is_locked"], true);
    assert!(!body.contains("Bearer"));
    assert!(!body.contains("secret"));
}

#[tokio::test]
async fn invalid_job_kind_returns_validation_error() {
    let app = create_app(AppState::new(Settings::default()));
    let (status, json, _) = response_parts(
        app,
        request(
            "POST",
            "/api/v1/admin/jobs/not-a-real-job/run",
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn manual_memory_snapshot_run_works_without_global_runner_enabled() {
    let state = AppState::new(jobs_settings());
    let app = create_app(state);

    let (status, json, body) = response_parts(
        app.clone(),
        request(
            "POST",
            "/api/v1/admin/jobs/memory-usage-snapshot/run",
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run"]["kind"], "MemoryUsageSnapshot");
    assert_eq!(json["run"]["status"], "Succeeded");
    assert_eq!(json["run"]["affected_count"], 1);
    assert_eq!(json["run"]["attempt_count"], 1);
    assert_eq!(json["run"]["max_attempts"], 3);
    assert_eq!(json["run"]["retried"], false);
    assert!(!body.contains("secret"));
    assert!(!body.contains("Bearer"));

    let (status, json, _) = response_parts(
        app,
        request(
            "GET",
            "/api/v1/admin/org/usage/memory/snapshots",
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["snapshots"].as_array().expect("snapshots").len(), 1);
}

#[tokio::test]
async fn manual_job_run_is_skipped_when_distributed_lock_is_held_by_another_owner() {
    let state = AppState::new(locked_jobs_settings());
    let store = state
        .background_job_lock_store
        .clone()
        .expect("lock store should be configured");
    store
        .try_acquire_lock(
            BackgroundJobKind::MemoryUsageSnapshot,
            "instance-b",
            std::time::Duration::from_secs(300),
        )
        .await
        .expect("other owner acquire");
    let app = create_app(state);

    let (status, json, body) = response_parts(
        app,
        request(
            "POST",
            "/api/v1/admin/jobs/memory-usage-snapshot/run",
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run"]["status"], "Skipped");
    assert_eq!(json["run"]["error_code"], "JOB_ALREADY_RUNNING");
    assert!(!body.contains("secret"));
    assert!(!body.contains("Bearer"));
}

#[tokio::test]
async fn manual_job_run_is_persisted_and_queryable() {
    let state = AppState::new(jobs_settings());
    let app = create_app(state);

    let (status, _, _) = response_parts(
        app.clone(),
        request(
            "POST",
            "/api/v1/admin/jobs/memory-usage-snapshot/run",
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json, body) = response_parts(
        app.clone(),
        request(
            "GET",
            "/api/v1/admin/jobs/runs?kind=MemoryUsageSnapshot&status=Succeeded",
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(json["runs"].as_array().expect("runs").len(), 1);
    assert_eq!(json["runs"][0]["kind"], "MemoryUsageSnapshot");
    assert_eq!(json["runs"][0]["status"], "Succeeded");
    assert_eq!(json["runs"][0]["attempt_count"], 1);
    assert_eq!(json["runs"][0]["retried"], false);
    assert!(!body.contains("Bearer"));
    assert!(!body.contains("secret"));

    let (status, json, _) =
        response_parts(app, request("GET", "/api/v1/admin/jobs", Some(ORG_A), true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !json["jobs"]["latest_persisted_runs"]
            .as_array()
            .expect("latest persisted runs")
            .is_empty()
    );
}

#[tokio::test]
async fn job_run_history_filters_and_validation_work() {
    let state = AppState::new(jobs_settings());
    let store = state
        .background_job_run_store
        .clone()
        .expect("history store should be configured");
    let base = Utc::now();
    store
        .insert_run(test_job_run(
            BackgroundJobKind::MemoryUsageSnapshot,
            BackgroundJobStatus::Succeeded,
            base - Duration::days(2),
        ))
        .await
        .expect("insert old run");
    store
        .insert_run(test_job_run(
            BackgroundJobKind::ProviderUsageRetention,
            BackgroundJobStatus::Failed,
            base,
        ))
        .await
        .expect("insert latest run");

    let app = create_app(state);
    let created_after = (base - Duration::days(1)).to_rfc3339();
    let uri = format!(
        "/api/v1/admin/jobs/runs?status=Failed&created_after={}",
        urlencoding_like(&created_after)
    );
    let (status, json, _) =
        response_parts(app.clone(), request("GET", &uri, Some(ORG_A), true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["runs"].as_array().expect("runs").len(), 1);
    assert_eq!(json["runs"][0]["status"], "Failed");

    let (status, json, _) = response_parts(
        app.clone(),
        request(
            "GET",
            "/api/v1/admin/jobs/runs?kind=not-a-job",
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");

    let (status, json, _) = response_parts(
        app.clone(),
        request(
            "GET",
            "/api/v1/admin/jobs/runs?status=not-a-status",
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");

    let (status, json, _) = response_parts(
        app,
        request(
            "GET",
            "/api/v1/admin/jobs/runs?created_after=not-a-date",
            Some(ORG_A),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn job_run_history_retention_dry_run_and_apply_work() {
    let state = AppState::new(jobs_settings());
    let store = state
        .background_job_run_store
        .clone()
        .expect("history store should be configured");
    store
        .insert_run(test_job_run(
            BackgroundJobKind::MemoryUsageSnapshot,
            BackgroundJobStatus::Succeeded,
            Utc::now() - Duration::days(45),
        ))
        .await
        .expect("old insert");
    store
        .insert_run(test_job_run(
            BackgroundJobKind::MemoryUsageSnapshot,
            BackgroundJobStatus::Succeeded,
            Utc::now(),
        ))
        .await
        .expect("new insert");

    let app = create_app(state);
    let (status, json, _) = response_parts(
        app.clone(),
        json_request(
            "POST",
            "/api/v1/admin/jobs/runs/retention/apply",
            ORG_A,
            serde_json::json!({ "dry_run": true, "retention_days": 30 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["matched_runs"], 1);
    assert_eq!(json["summary"]["deleted_runs"], 0);

    let (status, json, _) = response_parts(
        app.clone(),
        request("GET", "/api/v1/admin/jobs/runs", Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["runs"].as_array().expect("runs").len(), 2);

    let (status, json, _) = response_parts(
        app.clone(),
        json_request(
            "POST",
            "/api/v1/admin/jobs/runs/retention/apply",
            ORG_A,
            serde_json::json!({ "dry_run": false, "retention_days": 30 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["summary"]["deleted_runs"], 1);

    let (status, json, _) = response_parts(
        app,
        request("GET", "/api/v1/admin/jobs/runs", Some(ORG_A), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["runs"].as_array().expect("runs").len(), 1);
}

#[tokio::test]
async fn manual_provider_usage_retention_run_deletes_only_configured_org() {
    let mut settings = jobs_settings();
    settings.provider_usage_persistence_enabled = true;
    settings.provider_usage_retention_days = 30;
    let state = AppState::new(settings);
    let store = state
        .provider_usage_store
        .clone()
        .expect("mock provider usage store should be configured");

    store
        .record_usage_event(test_usage_event(ORG_A, Utc::now() - Duration::days(45)))
        .await
        .expect("old org_a usage should insert");
    store
        .record_usage_event(test_usage_event(ORG_B, Utc::now() - Duration::days(45)))
        .await
        .expect("old org_b usage should insert");
    store
        .record_usage_event(test_usage_event(ORG_A, Utc::now()))
        .await
        .expect("new org_a usage should insert");

    let app = create_app(state);
    let (status, json, _) = response_parts(
        app,
        request(
            "POST",
            "/api/v1/admin/jobs/provider-usage-retention/run",
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["run"]["kind"], "ProviderUsageRetention");
    assert_eq!(json["run"]["status"], "Succeeded");
    assert_eq!(json["run"]["affected_count"], 1);

    let org_a = store
        .query_usage(ProviderUsageQuery::new(ORG_A, 10))
        .await
        .expect("org_a usage should query");
    let org_b = store
        .query_usage(ProviderUsageQuery::new(ORG_B, 10))
        .await
        .expect("org_b usage should query");
    assert_eq!(org_a.events.len(), 1);
    assert_eq!(org_b.events.len(), 1);
}

#[tokio::test]
async fn database_auth_scopes_are_enforced_for_job_endpoints() {
    let state = AppState::initialize(database_auth_settings())
        .await
        .expect("state should initialize");
    state
        .api_key_store
        .insert_api_key(api_key_record(
            ORG_A,
            RAW_API_KEY,
            vec![ApiKeyScope::AdminRead],
        ))
        .await
        .expect("api key should insert");
    let app = create_app(state);

    let (status, _, _) = response_parts(
        app.clone(),
        bearer_request("GET", "/api/v1/admin/jobs", ORG_A, RAW_API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json, _) = response_parts(
        app.clone(),
        bearer_request(
            "POST",
            "/api/v1/admin/jobs/memory-usage-snapshot/run",
            ORG_A,
            RAW_API_KEY,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["code"], "FORBIDDEN");

    let (status, _, _) = response_parts(
        app.clone(),
        bearer_request("GET", "/api/v1/admin/jobs/runs", ORG_A, RAW_API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json, _) = response_parts(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/admin/jobs/runs/retention/apply")
            .header("X-Organization-ID", ORG_A)
            .header("Authorization", format!("Bearer {RAW_API_KEY}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"dry_run":true}"#))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["code"], "FORBIDDEN");
}

fn test_job_run(
    kind: BackgroundJobKind,
    status: BackgroundJobStatus,
    started_at: chrono::DateTime<Utc>,
) -> StoredBackgroundJobRun {
    StoredBackgroundJobRun {
        id: Uuid::new_v4(),
        kind,
        status,
        started_at,
        finished_at: Some(started_at + Duration::seconds(1)),
        duration_ms: Some(1000),
        attempt_count: 1,
        max_attempts: 1,
        retried: false,
        error_code: None,
        error_message: None,
        metadata: Some(serde_json::json!({ "org_count": 1, "affected_count": 1 })),
    }
}

fn urlencoding_like(value: &str) -> String {
    value.replace(':', "%3A").replace('+', "%2B")
}

fn test_usage_event(org_id: &str, created_at: chrono::DateTime<Utc>) -> ProviderUsageEventRecord {
    ProviderUsageEventRecord {
        id: Uuid::new_v4(),
        org_id: org_id.to_string(),
        user_id: Some("user_jobs".to_string()),
        provider_name: "mock".to_string(),
        model_name: Some("mock-model".to_string()),
        capability: ProviderUsageCapability::Llm,
        operation_name: "test".to_string(),
        status: ProviderCallStatus::Success,
        input_tokens: Some(1),
        output_tokens: Some(1),
        total_tokens: Some(2),
        retry_count: 0,
        fallback_used: false,
        circuit_blocked: false,
        timed_out: false,
        estimated_cost_usd: None,
        metadata: None,
        created_at,
    }
}
