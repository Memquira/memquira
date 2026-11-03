//! P2-20 temporal safety through the API: the time memcore receives a request never becomes an
//! event time.
//!
//! A canned model returns what a model anchored to "now" would say (for "last year": the year before
//! the ingestion day). Explicit dates stay usable without any client time; relative dates resolve
//! only against a client message or session timestamp; with neither, the event time, `valid_at` and
//! `observed_at` stay empty and the sentence keeps its own wording.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use memcore_core::{
    CandidateFact, FactStructure, MemoryEngine, MemoryType, StructuredExtractionConfig,
    TemporalGranularity, TemporalValue,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_p2_20";

/// "Now", as a model without a client anchor would use it.
fn ingestion_year_guess() -> TemporalValue {
    TemporalValue::new(
        Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap(),
        TemporalGranularity::Year,
        Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap(),
    )
    .unwrap()
}

fn candidate(
    content: &str,
    text: &str,
    cites: &[&str],
    model: Option<TemporalValue>,
) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Event, 0.9, 0.9, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some("Priya".to_string()),
            event_time: model,
            event_time_text: Some(text.to_string()),
            source_message_ids: cites.iter().map(|id| id.to_string()).collect(),
            ..FactStructure::default()
        })
}

fn app(candidates: Vec<CandidateFact>) -> axum::Router {
    let settings = Settings::default();
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new().with_extraction_candidates(candidates)),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_structured_extraction(Some(StructuredExtractionConfig::default()));
    create_app(AppState::with_memory_engine(settings, Arc::new(engine)))
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", ORG)
        .header(name, value)
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Posts `body` and returns (the add summary, the single exported fact).
async fn ingest(app: axum::Router, user: &str, body: Value) -> (Value, Value) {
    let (status, added) = call(app.clone(), "POST", "/api/v1/memories", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{added}");
    let (status, exported) = call(app, "GET", &format!("/api/v1/users/{user}/export"), None).await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let facts = exported["export"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(facts.len(), 1, "{exported}");
    (added["summary"].clone(), facts[0].clone())
}

fn dog_message(extra: Value) -> Value {
    let mut message = json!({
        "role": "user",
        "name": "Priya",
        "message_id": "m1",
        "content": "I adopted a dog last year."
    });
    for (key, value) in extra.as_object().unwrap() {
        message[key] = value.clone();
    }
    message
}

#[tokio::test]
async fn no_client_timestamp_leaves_a_relative_date_unknown() {
    let app = app(vec![candidate(
        "Priya adopted a dog last year.",
        "last year",
        &["m1"],
        Some(ingestion_year_guess()),
    )]);
    let (summary, fact) = ingest(
        app,
        "u_none",
        json!({ "user_id": "u_none", "messages": [dog_message(json!({}))] }),
    )
    .await;
    assert_eq!(summary["temporal_resolution"]["unanchored"], 1, "{summary}");
    assert!(fact["event_time"].is_null(), "{fact}");
    assert!(fact["valid_at"].is_null(), "{fact}");
    assert!(
        fact["observed_at"].is_null(),
        "receipt time is not an observation time: {fact}"
    );
    assert_eq!(fact["event_time_text"], "last year");
    assert_eq!(fact["content"], "Priya adopted a dog last year.");
}

#[tokio::test]
async fn an_explicit_date_is_kept_without_any_client_timestamp() {
    let app = app(vec![candidate(
        "Priya bought a car in March 2024.",
        "March 2024",
        &["m1"],
        None,
    )]);
    let (summary, fact) = ingest(
        app,
        "u_explicit",
        json!({
            "user_id": "u_explicit",
            "messages": [dog_message(json!({ "content": "I bought a car in March 2024." }))]
        }),
    )
    .await;
    assert_eq!(
        summary["temporal_resolution"]["deterministic_only"], 1,
        "{summary}"
    );
    assert_eq!(
        fact["event_time"]["start"], "2024-03-01T00:00:00Z",
        "{fact}"
    );
    assert_eq!(fact["event_time"]["end"], "2024-03-31T23:59:59Z");
    assert_eq!(fact["event_time"]["granularity"], "month");
    assert_eq!(fact["valid_at"], "2024-03-01T00:00:00Z");
    assert!(fact["observed_at"].is_null(), "{fact}");
    assert_eq!(fact["content"], "In March 2024, Priya bought a car.");
}

#[tokio::test]
async fn a_session_timestamp_anchors_a_relative_date() {
    let app = app(vec![candidate(
        "Priya adopted a dog last year.",
        "last year",
        &["m1"],
        Some(ingestion_year_guess()),
    )]);
    let (summary, fact) = ingest(
        app,
        "u_session",
        json!({
            "user_id": "u_session",
            "session_timestamp": "2023-05-08T09:00:00Z",
            "messages": [dog_message(json!({}))]
        }),
    )
    .await;
    assert_eq!(
        summary["temporal_resolution"]["deterministic_override"], 1,
        "{summary}"
    );
    assert_eq!(
        fact["event_time"]["start"], "2022-01-01T00:00:00Z",
        "{fact}"
    );
    assert_eq!(fact["event_time"]["anchor"], "2023-05-08T09:00:00Z");
    assert_eq!(fact["observed_at"], "2023-05-08T09:00:00Z");
    assert_eq!(fact["valid_at"], "2022-01-01T00:00:00Z");
    assert_eq!(fact["content"], "In 2022, Priya adopted a dog.");
}

#[tokio::test]
async fn a_message_timestamp_anchors_a_relative_date() {
    // The provider reports the cited message's own time as the fact's observation time.
    let mut dog = candidate(
        "Priya adopted a dog last year.",
        "last year",
        &["m1"],
        Some(ingestion_year_guess()),
    );
    dog.structure.observed_at = Some(Utc.with_ymd_and_hms(2021, 3, 2, 10, 0, 0).unwrap());
    let app = app(vec![dog]);
    let (_, fact) = ingest(
        app,
        "u_message",
        json!({
            "user_id": "u_message",
            "messages": [dog_message(json!({ "timestamp": "2021-03-02T10:00:00Z" }))]
        }),
    )
    .await;
    assert_eq!(
        fact["event_time"]["start"], "2020-01-01T00:00:00Z",
        "{fact}"
    );
    assert_eq!(fact["observed_at"], "2021-03-02T10:00:00Z");
}

/// The architecture client (scenario S3) sends role and content only, with its own time
/// in free-form `metadata`. memcore does not read times out of metadata, so this is the no-timestamp
/// case: no relative expression may be dated from the receipt time.
#[tokio::test]
async fn an_architecture_style_client_without_timestamps_gets_no_receipt_dated_events() {
    let app = app(vec![
        candidate(
            "Priya adopted a dog last year.",
            "last year",
            &[],
            Some(ingestion_year_guess()),
        ),
        candidate(
            "Priya visited her aunt yesterday.",
            "yesterday",
            &[],
            Some(ingestion_year_guess()),
        ),
    ]);
    let body = json!({
        "user_id": "p05_c0_priya",
        "messages": [
            { "role": "user", "content": "I adopted a dog last year." },
            { "role": "user", "content": "Visited my aunt yesterday." }
        ],
        "metadata": {
            "timestamp": "2023-05-08T13:56:00Z",
            "session_id": "session_1",
            "conversation_index": 0
        }
    });
    let (status, added) = call(app.clone(), "POST", "/api/v1/memories", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(
        added["summary"]["temporal_resolution"]["unanchored"], 2,
        "{added}"
    );
    let (_, exported) = call(app, "GET", "/api/v1/users/p05_c0_priya/export", None).await;
    let facts = exported["export"]["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 2);
    for fact in facts {
        assert!(fact["event_time"].is_null(), "{fact}");
        assert!(fact["valid_at"].is_null(), "{fact}");
        assert!(fact["observed_at"].is_null(), "{fact}");
    }
}
