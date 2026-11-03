mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use memcore_core::{ContextCacheConfig, InMemoryContextCache};
use std::sync::Arc;
use tower::ServiceExt;

use common::authorization_header;

const ORG_ID: &str = "org_metrics_a";
const USER_ID: &str = "user_metrics_a";

fn app_with_cache() -> axum::Router {
    let settings = Settings::default();
    let engine = Arc::new(
        memcore_api::create_mock_memory_engine(&settings)
            .expect("engine")
            .with_context_cache(
                Arc::new(InMemoryContextCache::new(100)),
                ContextCacheConfig {
                    enabled: true,
                    ttl_seconds: 300,
                    max_entries: 100,
                    metrics_enabled: true,
                    ..Default::default()
                },
            ),
    );
    create_app(AppState::with_memory_engine(settings, engine))
}

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

fn post_context(org_id: &str, query: &str) -> Request<Body> {
    let body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{query}"
        }}"#
    );
    let (name, value) = authorization_header();
    Request::builder()
        .method("POST")
        .uri("/api/v1/context")
        .header("X-Organization-ID", org_id)
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request")
}

#[tokio::test]
async fn context_cache_metrics_requires_authorization() {
    let app = app_with_cache();
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/admin/org/cache/context/metrics")
                .header("X-Organization-ID", ORG_ID)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn context_cache_metrics_requires_organization_header() {
    let app = app_with_cache();
    let response = app
        .oneshot(get_request("/api/v1/admin/org/cache/context/metrics", None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn context_cache_metrics_returns_aggregate_counters() {
    let app = app_with_cache();

    let _ = app
        .clone()
        .oneshot(post_context(ORG_ID, "metrics test query"))
        .await
        .expect("context");

    let response = app
        .oneshot(get_request(
            "/api/v1/admin/org/cache/context/metrics",
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
    assert_eq!(json["scope"], "process_local");
    assert!(json["metrics"]["misses"].as_u64().unwrap_or(0) >= 1);
    assert!(json["metrics"]["sets"].as_u64().unwrap_or(0) >= 1);
    assert!(json.get("query").is_none());
    assert!(json.get("context").is_none());
}

#[tokio::test]
async fn repeated_context_hit_increments_hit_metric() {
    let app = app_with_cache();
    let query = "repeat metrics query";

    let _ = app
        .clone()
        .oneshot(post_context(ORG_ID, query))
        .await
        .expect("first");
    let _ = app
        .clone()
        .oneshot(post_context(ORG_ID, query))
        .await
        .expect("second");

    let response = app
        .oneshot(get_request(
            "/api/v1/admin/org/cache/context/metrics",
            Some(ORG_ID),
        ))
        .await
        .expect("metrics");
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(json["metrics"]["hits"].as_u64().unwrap_or(0) >= 1);
}
