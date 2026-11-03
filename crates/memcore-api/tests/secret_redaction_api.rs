mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use memcore_api::response::ErrorBody;
use memcore_api::{AppState, create_app};
use memcore_common::{MemcoreError, Redactor, safe_error_message};
use memcore_config::Settings;
use memcore_core::{
    BackgroundJobKind, BackgroundJobRun, BackgroundJobStatus, sanitize_background_job_error_message,
};
use serde_json::json;
use tower::ServiceExt;

use common::authorization_header;

async fn send(app: axum::Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
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

#[test]
fn settings_debug_redacts_secrets() {
    let settings = Settings {
        database_url: "postgres://user:secret@localhost/memcore".to_string(),
        postgres_url: Some("postgres://user:secret@localhost/memcore".to_string()),
        redis_url: Some("redis://:pass@localhost:6379/0".to_string()),
        openai_api_key: Some("sk-live-abcdef".to_string()),
        api_key_pepper: Some("pepper-value".to_string()),
        dev_api_key: "dev-secret-key".to_string(),
        ..Settings::default()
    };
    let debug = format!("{settings:?}");
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("pepper-value"));
    assert!(!debug.contains("sk-live-abcdef"));
    assert!(!debug.contains("dev-secret-key"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn provider_and_auth_errors_are_safe() {
    let provider = MemcoreError::ProviderError("OPENAI_API_KEY=sk-abc invalid".to_string());
    let message = safe_error_message(&provider);
    assert!(!message.contains("sk-abc"));
    assert!(!message.contains("OPENAI_API_KEY"));

    let auth_echo =
        MemcoreError::ValidationError("Authorization: Bearer leaked-token is invalid".to_string());
    let redacted = safe_error_message(&auth_echo);
    assert!(!redacted.contains("leaked-token"));
}

#[test]
fn migration_and_backup_errors_do_not_expose_urls() {
    let migration = MemcoreError::MigrationError(
        "failed applying migration via postgres://user:pass@db/memcore SQL CREATE TABLE"
            .to_string(),
    );
    assert_eq!(safe_error_message(&migration), "database migration failed");

    let backup =
        MemcoreError::StorageError("backup failed for sqlite://./data/memcore.db".to_string());
    assert_eq!(safe_error_message(&backup), "database operation failed");
}

#[test]
fn job_history_error_message_is_redacted() {
    let message = sanitize_background_job_error_message(
        "job failed Bearer abc redis://:secret@localhost:6379",
    );
    assert!(!message.contains("abc"));
    assert!(!message.contains("secret"));
    assert!(message.contains("[REDACTED]"));

    let run = BackgroundJobRun {
        id: uuid::Uuid::new_v4(),
        kind: BackgroundJobKind::MemoryUsageSnapshot,
        status: BackgroundJobStatus::Failed,
        started_at: chrono::Utc::now(),
        finished_at: Some(chrono::Utc::now()),
        duration_ms: Some(1),
        attempt_count: 1,
        max_attempts: 1,
        retried: false,
        error_code: Some("STORAGE_ERROR".to_string()),
        error_message: Some("failed postgres://user:pass@localhost/db".to_string()),
        org_count: 0,
        affected_count: 0,
    };
    let response = memcore_api::dto::jobs::BackgroundJobRunResponse::from(run);
    let exposed = response.error_message.unwrap_or_default();
    assert!(!exposed.contains("pass"));
}

#[tokio::test]
async fn ready_endpoint_does_not_expose_connection_strings() {
    let app = create_app(AppState::new(Settings {
        database_url: "postgres://user:secret@localhost/memcore".to_string(),
        ..Settings::default()
    }));
    let (status, json) = send(
        app,
        Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json.to_string();
    assert!(!body.contains("secret"));
    assert!(!body.contains("postgres://"));
    assert!(json["checks"]["database"]["connected"].is_boolean());
}

#[tokio::test]
async fn api_key_list_omits_key_hash_and_plaintext() {
    let app = create_app(AppState::new(Settings::default()));
    let (auth_header, auth_value) = authorization_header();
    let (status, json) = send(
        app,
        Request::builder()
            .uri("/api/v1/api-keys")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_redaction")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json.to_string();
    assert!(!body.contains("key_hash"));
    assert!(!body.contains("raw_key"));
}

#[tokio::test]
async fn unsupported_media_type_does_not_echo_body() {
    let app = create_app(AppState::new(Settings::default()));
    let (auth_header, auth_value) = authorization_header();
    let leaked = "Bearer super-secret-body-token";
    let (status, json) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/memories")
            .header(auth_header, auth_value)
            .header("X-Organization-ID", "org_redaction")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(leaked))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let body = json.to_string();
    assert!(!body.contains("super-secret-body-token"));
    assert_eq!(json["error"]["code"], "UNSUPPORTED_MEDIA_TYPE");
}

#[test]
fn error_body_from_storage_uses_safe_message() {
    let (_, body) = ErrorBody::from_memcore_error(MemcoreError::StorageError(
        "failed postgres://user:secret@db/memcore".to_string(),
    ));
    assert_eq!(body.error.message, "database operation failed");
    assert!(!body.error.message.contains("secret"));
}

#[test]
fn redactor_json_redacts_nested_secrets() {
    let value = Redactor::redact_json(json!({
        "user": {"api_key": "x", "name": "a"},
        "prompt": "hello"
    }));
    assert_eq!(value["user"]["api_key"], "[REDACTED]");
    assert_eq!(value["user"]["name"], "a");
    assert_eq!(value["prompt"], "hello");
}
