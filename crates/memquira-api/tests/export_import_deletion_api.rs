//! `GET /api/v1/users/{user}/export?include_deleted=true` then `POST /api/v1/users/{user}/import`:
//! the export marks the deleted memory (`deleted_at`) and lists its withheld message, and the
//! import (same user, on a fresh server) stores it deleted, so neither the memory nor its message comes back through
//! `POST /api/v1/memories/context`. An export without deleted memories restores the kept memory
//! and skips the withheld message. Old exports without the new fields still import. Invented data
//! only (mock providers).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memquira_api::{AppState, create_app};
use memquira_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_export_deletion_api";
const SOURCE: &str = "user_export_source";

fn app() -> axum::Router {
    create_app(AppState::new(Settings {
        episodes_enabled: true,
        structured_extraction_enabled: true,
        search_max_limit: 100,
        ..Settings::default()
    }))
}

async fn call(
    app: &axum::Router,
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
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!({})))
}

async fn add(app: &axum::Router, user: &str, id: &str, text: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/memories",
        Some(json!({
            "user_id": user,
            "session_id": "s1",
            "messages": [{"role": "user", "content": text, "message_id": id}],
            "metadata": {}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["memories"][0]["id"]
        .as_str()
        .expect("memory id")
        .to_string()
}

async fn context(app: &axum::Router, user: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/memories/context",
        Some(json!({"user_id": user, "query": "what does the user keep?", "token_budget": 2000})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["text"].as_str().expect("context text").to_string()
}

async fn export(app: &axum::Router, include_deleted: bool) -> Value {
    export_of(app, SOURCE, include_deleted).await
}

async fn export_of(app: &axum::Router, user: &str, include_deleted: bool) -> Value {
    let (status, body) = call(
        app,
        "GET",
        &format!("/api/v1/users/{user}/export?include_deleted={include_deleted}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["export"].clone()
}

async fn import(app: &axum::Router, user: &str, export: &Value, dry_run: bool) -> Value {
    let (status, body) = call(
        app,
        "POST",
        &format!("/api/v1/users/{user}/import"),
        Some(json!({"export": export, "mode": "append", "dry_run": dry_run})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["summary"].clone()
}

/// The source user keeps one memory and deletes the other.
async fn seed(app: &axum::Router) {
    let wheel = add(app, SOURCE, "m1", "I keep a pottery wheel in the garage.").await;
    add(app, SOURCE, "m2", "My rabbit is called Pebble.").await;
    let (status, body) = call(
        app,
        "DELETE",
        &format!("/api/v1/users/{SOURCE}/memories/{wheel}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_include_deleted_export_imports_the_deleted_memory_as_deleted() {
    let app = app();
    seed(&app).await;
    let exported = export(&app, true).await;

    let facts = exported["facts"].as_array().expect("facts");
    assert_eq!(facts.len(), 2);
    let deleted: Vec<&Value> = facts
        .iter()
        .filter(|f| f["deleted_at"].is_string())
        .collect();
    assert_eq!(deleted.len(), 1, "{exported}");
    assert!(
        deleted[0]["content"]
            .as_str()
            .unwrap()
            .contains("pottery wheel")
    );
    assert_eq!(exported["withheld_message_ids"], json!(["m1"]));

    let target = self::app();
    let planned = import(&target, SOURCE, &exported, true).await;
    let done = import(&target, SOURCE, &exported, false).await;
    for summary in [&planned, &done] {
        assert_eq!(summary["imported_facts"], 2, "{summary}");
        assert_eq!(summary["imported_deleted_facts"], 1, "{summary}");
        assert_eq!(summary["imported_episodes"], 2, "{summary}");
        assert_eq!(summary["skipped_episodes"], 0, "{summary}");
    }

    let text = context(&target, SOURCE).await;
    assert!(!text.contains("pottery wheel"), "{text}");
    assert!(text.contains("Pebble"), "{text}");

    // The target's own export marks it deleted again.
    let again = export_of(&target, SOURCE, true).await;
    assert_eq!(
        again["facts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["deleted_at"].is_string())
            .count(),
        1
    );
}

#[tokio::test]
async fn an_export_without_deleted_memories_skips_their_messages() {
    let app = app();
    seed(&app).await;
    let exported = export(&app, false).await;
    assert_eq!(exported["facts"].as_array().unwrap().len(), 1);
    assert_eq!(exported["withheld_message_ids"], json!(["m1"]));

    let target = self::app();
    let done = import(&target, SOURCE, &exported, false).await;
    assert_eq!(done["imported_facts"], 1, "{done}");
    assert_eq!(done["imported_deleted_facts"], 0, "{done}");
    assert_eq!(done["imported_episodes"], 1, "{done}");
    assert_eq!(done["skipped_episodes"], 1, "{done}");
    let text = context(&target, SOURCE).await;
    assert!(!text.contains("pottery wheel"), "{text}");
    assert!(text.contains("Pebble"), "{text}");
}

#[tokio::test]
async fn an_export_without_the_new_fields_still_imports() {
    let app = app();
    add(&app, SOURCE, "m2", "My rabbit is called Pebble.").await;
    let mut exported = export(&app, true).await;
    let object = exported.as_object_mut().unwrap();
    assert!(
        !object.contains_key("withheld_message_ids"),
        "omitted when empty"
    );
    assert!(
        object["facts"][0].get("deleted_at").is_none(),
        "omitted when unset"
    );
    object.remove("withheld_message_ids");

    let target = self::app();
    let done = import(&target, SOURCE, &exported, false).await;
    assert_eq!(done["imported_facts"], 1, "{done}");
    assert_eq!(done["imported_deleted_facts"], 0, "{done}");
    assert!(context(&target, SOURCE).await.contains("Pebble"));
}
