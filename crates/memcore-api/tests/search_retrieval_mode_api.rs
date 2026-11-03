//! `retrieval_mode` on `POST /api/v1/memories/search`.
//!
//! Omitted means `hybrid`, the original fused search; `semantic` and `fulltext` run one leg alone.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_modes_api";
const USER: &str = "user_modes_api";

async fn call(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", ORG)
        .header(name, value)
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn search(query: &str, mode: Option<&str>) -> Value {
    let mut body = json!({ "user_id": USER, "query": query, "limit": 10 });
    if let Some(mode) = mode {
        body["retrieval_mode"] = json!(mode);
    }
    body
}

async fn app_with_memory() -> axum::Router {
    let app = create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        ..Settings::default()
    }));
    let (status, body) = call(
        app.clone(),
        "/api/v1/memories",
        json!({
            "user_id": USER,
            "messages": [{ "role": "user", "content": "I collect vintage postcards and enamel pins." }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app
}

#[tokio::test]
async fn every_mode_is_accepted_and_omitted_means_hybrid() {
    let app = app_with_memory().await;
    let question = "What does the user collect? postcards";
    let (status, omitted) = call(
        app.clone(),
        "/api/v1/memories/search",
        search(question, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{omitted}");
    let (status, hybrid) = call(
        app.clone(),
        "/api/v1/memories/search",
        search(question, Some("hybrid")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(omitted, hybrid);

    for mode in ["semantic", "fulltext", "rrf"] {
        let (status, body) = call(
            app.clone(),
            "/api/v1/memories/search",
            search(question, Some(mode)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{mode}: {body}");
        assert!(body["results"].is_array(), "{mode}: {body}");
    }
    let (_, fulltext) = call(
        app,
        "/api/v1/memories/search",
        search(question, Some("fulltext")),
    )
    .await;
    let contents: Vec<&str> = fulltext["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["content"].as_str())
        .collect();
    assert!(
        contents
            .iter()
            .any(|c| c.to_lowercase().contains("postcards")),
        "{fulltext}"
    );
}

#[tokio::test]
async fn an_unknown_mode_is_rejected() {
    let app = app_with_memory().await;
    let (status, _) = call(
        app,
        "/api/v1/memories/search",
        search("postcards", Some("bm25")),
    )
    .await;
    assert!(status.is_client_error(), "{status}");
}

#[tokio::test]
async fn an_omitted_mode_follows_the_server_strategy() {
    let app = create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        search_strategy: memcore_config::SearchStrategy::Fulltext,
        ..Settings::default()
    }));
    let (status, _) = call(
        app.clone(),
        "/api/v1/memories",
        json!({
            "user_id": USER,
            "messages": [{ "role": "user", "content": "I collect vintage postcards and enamel pins." }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let question = "postcards";
    let (_, omitted) = call(
        app.clone(),
        "/api/v1/memories/search",
        search(question, None),
    )
    .await;
    let (_, fulltext) = call(
        app,
        "/api/v1/memories/search",
        search(question, Some("fulltext")),
    )
    .await;
    assert_eq!(omitted, fulltext);
}

#[tokio::test]
async fn user_names_are_accepted_and_bounded() {
    let app = app_with_memory().await;
    let mut body = search("What does Priya collect? postcards", Some("fulltext"));
    body["user_names"] = json!(["Priya"]);
    let (status, found) = call(app.clone(), "/api/v1/memories/search", body).await;
    assert_eq!(status, StatusCode::OK, "{found}");

    let mut too_many = search("postcards", Some("fulltext"));
    too_many["user_names"] = json!(vec!["x"; 11]);
    let (status, _) = call(app.clone(), "/api/v1/memories/search", too_many).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut too_long = search("postcards", Some("fulltext"));
    too_long["user_names"] = json!(["a".repeat(201)]);
    let (status, _) = call(app, "/api/v1/memories/search", too_long).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The public search limit stays 50 unless an operator raises MEMCORE_SEARCH_MAX_LIMIT.
#[tokio::test]
async fn the_search_limit_follows_memcore_search_max_limit() {
    let default_app = app_with_memory().await;
    let mut body = search("postcards", Some("semantic"));
    body["limit"] = json!(50);
    let (status, _) = call(default_app.clone(), "/api/v1/memories/search", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    body["limit"] = json!(51);
    let (status, _) = call(default_app, "/api/v1/memories/search", body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let raised = create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        search_max_limit: 100,
        ..Settings::default()
    }));
    for (limit, expected) in [
        (51, StatusCode::OK),
        (100, StatusCode::OK),
        (101, StatusCode::BAD_REQUEST),
    ] {
        body["limit"] = json!(limit);
        let (status, response) =
            call(raised.clone(), "/api/v1/memories/search", body.clone()).await;
        assert_eq!(status, expected, "{limit}: {response}");
    }
}

/// `vector_score` is added next to an unchanged `score`; omitted when no vector leg ran.
#[tokio::test]
async fn vector_score_is_additive_and_omitted_without_a_vector_leg() {
    let app = app_with_memory().await;
    let question = "What does the user collect? postcards";
    let (status, semantic) = call(
        app.clone(),
        "/api/v1/memories/search",
        search(question, Some("semantic")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = &semantic["results"][0];
    let score = result["score"].as_f64().unwrap();
    assert!((score - 1.0 / 61.0).abs() < 1e-6, "{result}");
    let raw = result["vector_score"].as_f64().unwrap();
    assert!((-1.0..=1.0).contains(&raw));

    let (_, fulltext) = call(
        app,
        "/api/v1/memories/search",
        search(question, Some("fulltext")),
    )
    .await;
    let results = fulltext["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().all(|r| r.get("vector_score").is_none()));
    assert!(results.iter().all(|r| r["score"].as_f64().is_some()));
}
