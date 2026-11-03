mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, ProviderWiring, create_app, create_mock_memory_engine_with_wiring};
use memcore_config::Settings;
use memcore_core::{AddMemoryInput, MemoryMessage, MessageRole, TenantContext};
use memcore_providers::InMemoryProviderUsageRecorder;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use common::authorization_header;

const ORG_ID: &str = "org_provider_usage";
const USER_ID: &str = "user_provider_usage";

fn get_request(path: &str, org_id: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }
    let (name, value) = authorization_header();
    builder
        .header(name, value)
        .body(Body::empty())
        .expect("request")
}

#[tokio::test]
async fn provider_usage_requires_authorization() {
    let settings = Settings::default();
    let usage = InMemoryProviderUsageRecorder::new();
    let wiring = ProviderWiring::for_tests(usage.clone());
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));
    let app = create_app(AppState::with_memory_engine_and_provider_usage(
        settings, engine, usage,
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/org/provider-usage")
                .header("X-Organization-ID", ORG_ID)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn provider_usage_requires_organization_header() {
    let settings = Settings::default();
    let usage = InMemoryProviderUsageRecorder::new();
    let wiring = ProviderWiring::for_tests(usage.clone());
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));
    let app = create_app(AppState::with_memory_engine_and_provider_usage(
        settings, engine, usage,
    ));

    let response = app
        .oneshot(get_request("/api/v1/admin/org/provider-usage", None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn provider_usage_returns_memory_source_counters() {
    let settings = Settings::default();
    let usage = InMemoryProviderUsageRecorder::new();
    let wiring = ProviderWiring::for_tests(usage.clone());
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));

    let tenant = TenantContext::new(ORG_ID, USER_ID).expect("tenant");
    let _ = engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "metrics provider usage test".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    let app = create_app(AppState::with_memory_engine_and_provider_usage(
        settings, engine, usage,
    ));

    let response = app
        .oneshot(get_request(
            "/api/v1/admin/org/provider-usage?source=memory",
            Some(ORG_ID),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");

    assert_eq!(json["status"], "success");
    assert_eq!(json["source"], "memory");
    assert!(json["summary"]["total_requests"].as_u64().unwrap_or(0) >= 1);
    assert!(json.get("prompt").is_none());
    assert!(json.get("messages").is_none());
    let body_text = String::from_utf8_lossy(&body);
    assert!(!body_text.contains("metrics provider usage test"));
}

#[tokio::test]
async fn provider_usage_persistent_source_returns_stored_events() {
    let mut settings = Settings::default();
    settings.provider_usage_persistence_enabled = true;
    let wiring = ProviderWiring::for_mock_tests(&settings);
    let usage = wiring.usage_recorder.clone();
    let store = wiring.usage_store.clone().expect("mock store");
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));

    let tenant = TenantContext::new(ORG_ID, USER_ID).expect("tenant");
    let _ = engine
        .add_memory(AddMemoryInput {
            tenant,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "persist provider usage".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add memory");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let app = create_app(AppState::with_memory_engine_provider_usage_and_store(
        settings,
        engine,
        usage,
        Some(store),
    ));

    let response = app
        .oneshot(get_request(
            "/api/v1/admin/org/provider-usage?source=persistent",
            Some(ORG_ID),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["source"], "persistent");
    assert!(
        json["events"]
            .as_array()
            .map(|events| !events.is_empty())
            .unwrap_or(false)
    );
    assert!(json["summary"]["total_requests"].as_u64().unwrap_or(0) >= 1);
    let body_text = String::from_utf8_lossy(&body);
    assert!(!body_text.contains("persist provider usage"));
}

#[tokio::test]
async fn provider_usage_invalid_capability_returns_validation_error() {
    let settings = Settings::default();
    let usage = InMemoryProviderUsageRecorder::new();
    let wiring = ProviderWiring::for_tests(usage.clone());
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));
    let app = create_app(AppState::with_memory_engine_and_provider_usage(
        settings, engine, usage,
    ));

    let response = app
        .oneshot(get_request(
            "/api/v1/admin/org/provider-usage?capability=invalid",
            Some(ORG_ID),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn provider_guardrails_status_requires_auth_and_is_safe() {
    let settings = Settings::default();
    let usage = InMemoryProviderUsageRecorder::new();
    let wiring = ProviderWiring::for_tests(usage.clone());
    let engine =
        Arc::new(create_mock_memory_engine_with_wiring(&settings, &wiring).expect("engine"));
    let app = create_app(AppState::with_memory_engine_and_provider_usage(
        settings, engine, usage,
    ));

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/providers/guardrails")
                .header("X-Organization-ID", ORG_ID)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(get_request(
            "/api/v1/admin/providers/guardrails",
            Some(ORG_ID),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["status"], "success");
    assert_eq!(json["enabled"], true);
    assert_eq!(json["real_provider_calls_enabled"], false);
    assert_eq!(json["test_mode"], "mock_only");
    assert!(json["remaining_calls"].as_u64().unwrap_or(0) >= 1);
    let text = String::from_utf8_lossy(&body);
    assert!(!text.to_ascii_lowercase().contains("sk-"));
    assert!(!text.contains("OPENAI_API_KEY"));
    assert!(!text.contains("prompt"));
}
