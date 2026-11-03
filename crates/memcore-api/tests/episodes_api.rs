//! `POST /api/v1/episodes/search` and episode fields in the lifecycle responses.
//!
//! With `MEMCORE_EPISODES_ENABLED` off (the default) the endpoint answers 400 and every existing
//! response keeps its shape; on, added messages become searchable episodes that are exported (v2),
//! imported, counted and covered by retention.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_episodes_api";
const USER: &str = "user_episodes_api";

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
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn app(episodes_enabled: bool) -> axum::Router {
    create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        episodes_enabled,
        retention_enabled: true,
        fact_retention_days: 365,
        ..Settings::default()
    }))
}

async fn add(app: &axum::Router, user: &str) {
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v1/memories",
        Some(json!({
            "user_id": user,
            "session_id": "S1",
            "session_timestamp": "2023-05-08T13:00:00Z",
            "messages": [
                { "role": "user", "name": "Priya", "message_id": "D1:1",
                  "content": "I adopted a rescue greyhound named Juniper last spring." },
                { "role": "user", "name": "Priya", "message_id": "D1:2", "content": "Thanks!" }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn search(user: &str, query: &str, limit: usize) -> Value {
    json!({ "user_id": user, "query": query, "limit": limit })
}

#[tokio::test]
async fn disabled_episodes_answer_400_and_keep_existing_responses() {
    let app = app(false);
    add(&app, USER).await;
    let (status, _) = call(
        app.clone(),
        "POST",
        "/api/v1/episodes/search",
        Some(search(USER, "greyhound", 10)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, summary) = call(app.clone(), "GET", "/api/v1/admin/org/summary", None).await;
    assert!(
        summary["summary"].get("total_episodes").is_none(),
        "{summary}"
    );
    let (_, export) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/users/{USER}/export"),
        None,
    )
    .await;
    assert_eq!(export["export"]["format_version"], "memcore.user_export.v1");
    assert!(export["export"].get("episodes").is_none());
    let (_, retention) = call(
        app,
        "POST",
        &format!("/api/v1/users/{USER}/retention/apply"),
        Some(json!({ "dry_run": true })),
    )
    .await;
    assert!(
        retention["summary"].get("episodes_matched").is_none(),
        "{retention}"
    );
}

#[tokio::test]
async fn episodes_are_searchable_scoped_and_validated() {
    let app = app(true);
    add(&app, USER).await;
    add(&app, "another_user").await;

    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v1/episodes/search",
        Some(search(USER, "Juniper the greyhound", 10)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let results = body["results"].as_array().unwrap();
    // "Thanks!" is shorter than 20 characters: stored, not embedded, not searchable.
    assert_eq!(results.len(), 1, "{body}");
    let hit = &results[0];
    assert_eq!(hit["message_id"], "D1:1");
    assert_eq!(hit["session_id"], "S1");
    assert_eq!(hit["speaker"], "Priya");
    assert_eq!(hit["role"], "user");
    assert_eq!(hit["observed_at"], "2023-05-08T13:00:00Z");
    assert!(hit["score"].as_f64().is_some());
    // Episode search is semantic only, so the raw vector similarity equals `score`.
    assert_eq!(hit["vector_score"], hit["score"]);
    assert!(hit["episode_id"].as_str().is_some());

    for bad in [
        search(USER, "greyhound", 0),
        search(USER, "greyhound", 51),
        search(USER, "   ", 10),
        search("  ", "greyhound", 10),
    ] {
        let (status, _) = call(app.clone(), "POST", "/api/v1/episodes/search", Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn episodes_appear_in_summary_export_import_and_retention() {
    let source = app(true);
    add(&source, USER).await;

    let (_, summary) = call(source.clone(), "GET", "/api/v1/admin/org/summary", None).await;
    assert_eq!(summary["summary"]["total_episodes"], 2, "{summary}");

    let (status, export) = call(
        source.clone(),
        "GET",
        &format!("/api/v1/users/{USER}/export"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(export["export"]["format_version"], "memcore.user_export.v2");
    assert_eq!(export["export"]["episodes"].as_array().unwrap().len(), 2);

    let (_, retention) = call(
        source,
        "POST",
        &format!("/api/v1/users/{USER}/retention/apply"),
        Some(json!({ "dry_run": true })),
    )
    .await;
    assert_eq!(retention["summary"]["episodes_matched"], 0, "{retention}");

    let target = app(true);
    let (status, imported) = call(
        target.clone(),
        "POST",
        &format!("/api/v1/users/{USER}/import"),
        Some(json!({ "export": export["export"], "mode": "append" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["summary"]["imported_episodes"], 2);
    let (_, found) = call(
        target,
        "POST",
        "/api/v1/episodes/search",
        Some(search(USER, "Juniper the greyhound", 10)),
    )
    .await;
    assert_eq!(found["results"].as_array().unwrap().len(), 1, "{found}");

    // A server with episodes off still restores them as data (search stays off).
    let disabled = app(false);
    let (status, restored) = call(
        disabled,
        "POST",
        &format!("/api/v1/users/{USER}/import"),
        Some(json!({ "export": export["export"], "mode": "append" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["summary"]["imported_episodes"], 2);
    assert_eq!(restored["summary"]["skipped_episodes"], 0);
}

#[tokio::test]
async fn openapi_documents_episode_search() {
    let (status, doc) = call(app(false), "GET", "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(doc["paths"].get("/api/v1/episodes/search").is_some());
}

/// Episodes written with episodes on are still exported after
/// a restart with episodes off, a replace import restores them, and search stays off.
#[tokio::test]
async fn sqlite_export_and_import_keep_episodes_after_they_are_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memcore.db");
    std::fs::File::create(&db).unwrap();
    let database_url = format!("sqlite:{}", db.to_string_lossy().replace('\\', "/"));
    let settings = |episodes_enabled| Settings {
        fact_backend: memcore_config::FactBackend::Sqlite,
        event_backend: memcore_config::EventBackend::Sqlite,
        database_url: database_url.clone(),
        structured_extraction_enabled: false,
        episodes_enabled,
        rate_limit_enabled: false,
        ..Settings::default()
    };
    let on = create_app(AppState::initialize(settings(true)).await.unwrap());
    add(&on, USER).await;
    add(&on, "user_other").await;
    drop(on);

    let off = create_app(AppState::initialize(settings(false)).await.unwrap());
    let (status, export) = call(
        off.clone(),
        "GET",
        &format!("/api/v1/users/{USER}/export"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export}");
    assert_eq!(export["export"]["format_version"], "memcore.user_export.v2");
    let episodes = export["export"]["episodes"]
        .as_array()
        .expect("episodes exported")
        .clone();
    assert_eq!(episodes.len(), 2);

    let (status, imported) = call(
        off.clone(),
        "POST",
        &format!("/api/v1/users/{USER}/import"),
        Some(json!({ "export": export["export"], "mode": "replace" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["summary"]["imported_episodes"], 2);
    let (_, again) = call(
        off.clone(),
        "GET",
        &format!("/api/v1/users/{USER}/export"),
        None,
    )
    .await;
    assert_eq!(again["export"]["episodes"].as_array().unwrap().len(), 2);

    // Episode behaviour stays off.
    let (status, _) = call(
        off.clone(),
        "POST",
        "/api/v1/episodes/search",
        Some(search(USER, "greyhound", 5)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // The other user is untouched; forget-user still deletes a user's episodes with the flag off.
    let (_, other) = call(off.clone(), "GET", "/api/v1/users/user_other/export", None).await;
    assert_eq!(other["export"]["episodes"].as_array().unwrap().len(), 2);
    let (status, _) = call(off.clone(), "DELETE", "/api/v1/users/user_other", None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, gone) = call(off, "GET", "/api/v1/users/user_other/export", None).await;
    assert!(gone["export"].get("episodes").is_none(), "{gone}");
}
