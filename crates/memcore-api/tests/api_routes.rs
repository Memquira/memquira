use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

async fn response_json(app: axum::Router, uri: &str) -> serde_json::Value {
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();

    serde_json::from_slice(&body).expect("response should be valid json")
}

#[tokio::test]
async fn health_returns_200() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_response_contains_status_ok() {
    let json = response_json(test_app(), "/health").await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["service"], "memcore");
    assert!(json["version"].is_string());
}

#[tokio::test]
async fn ready_returns_200() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn ready_response_contains_config_derived_values() {
    let json = response_json(test_app(), "/ready").await;
    assert_eq!(json["status"], "ready");
    assert_eq!(json["environment"], "development");
    assert_eq!(json["storage_mode"], "embedded");
    assert_eq!(json["vector_backend"], "mock");
    assert_eq!(json["fact_backend"], "mock");
    assert_eq!(json["checks"]["database"]["connected"], true);
    assert_eq!(json["checks"]["database"]["migrations_clean"], true);
    assert_eq!(
        json["checks"]["database"]["pending_migrations"],
        serde_json::Value::Null
    );
    assert_eq!(json["checks"]["providers"]["configured"], true);

    let body = serde_json::to_string(&json).expect("ready response should serialize");
    assert!(!body.contains("sqlite://"));
    assert!(!body.contains("postgres://"));
}
