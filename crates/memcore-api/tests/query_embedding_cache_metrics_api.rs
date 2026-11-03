//! The query-embedding cache's admin metrics.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use std::sync::Arc;
use tower::ServiceExt;

use common::authorization_header;

const ORG_ID: &str = "org_qe_metrics";
const PATH: &str = "/api/v1/admin/org/cache/query-embedding/metrics";

fn app(cache_capacity: usize) -> axum::Router {
    let settings = Settings::default();
    let engine = Arc::new(
        memcore_api::create_mock_memory_engine(&settings)
            .expect("engine")
            .with_query_embedding_cache(cache_capacity),
    );
    create_app(AppState::with_memory_engine(settings, engine))
}

fn get_metrics(org_id: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(PATH);
    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }
    let (name, value) = authorization_header();
    builder
        .header(name, value)
        .body(Body::empty())
        .expect("request")
}

fn post_search(query: &str) -> Request<Body> {
    let body = format!(r#"{{"user_id": "user_qe", "query": "{query}", "limit": 5}}"#);
    let (name, value) = authorization_header();
    Request::builder()
        .method("POST")
        .uri("/api/v1/memories/search")
        .header("X-Organization-ID", ORG_ID)
        .header(name, value)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request")
}

async fn json(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&body).expect("json")
}

#[tokio::test]
async fn metrics_require_authorization_and_an_organization() {
    let response = app(64)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(PATH)
                .header("X-Organization-ID", ORG_ID)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app(64).oneshot(get_metrics(None)).await.expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_cache_is_reported_off_by_default() {
    let response = app(0)
        .oneshot(get_metrics(Some(ORG_ID)))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let json = json(response).await;
    assert_eq!(json["status"], "success");
    assert_eq!(json["scope"], "process_local");
    assert_eq!(json["enabled"], false);
    assert_eq!(json["metrics"]["requests"], 0);
    assert_eq!(json["metrics"]["provider_operations"], 0);
}

#[tokio::test]
async fn a_repeated_search_query_counts_one_provider_operation() {
    let app = app(64);
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(post_search("where is the copper kettle"))
            .await
            .expect("search");
        assert_eq!(response.status(), StatusCode::OK);
    }
    let json = json(
        app.oneshot(get_metrics(Some(ORG_ID)))
            .await
            .expect("metrics"),
    )
    .await;
    assert_eq!(json["enabled"], true);
    let metrics = &json["metrics"];
    assert_eq!(metrics["requests"], 2);
    assert_eq!(metrics["leaders"], 1);
    assert_eq!(metrics["hits"], 1);
    assert_eq!(
        metrics["bypassed"], 0,
        "the mock provider has a known identity"
    );
    assert_eq!(metrics["provider_operations"], 1);
    assert_eq!(metrics["entries"], 1);
    assert!(json.get("query").is_none(), "no query text is exposed");
}
