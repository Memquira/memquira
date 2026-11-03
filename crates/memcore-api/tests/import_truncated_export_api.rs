//! over the wire: `POST /api/v1/users/{user_id}/import` refuses `replace` with an export marked
//! `truncated`, rejects malformed truncation metadata, keeps `append` working, and a rejected
//! request changes nothing. Invented data only.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_t1c_api";
const USER: &str = "user_t1c_api";

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (auth_name, auth_value) = authorization_header();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", ORG)
        .header(auth_name, auth_value)
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
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

async fn seeded_app() -> axum::Router {
    let app = create_app(AppState::new(Settings::default()));
    for text in [
        "Avery keeps a pottery wheel in the garage.",
        "Avery bakes sourdough every Sunday.",
    ] {
        let (status, _) = call(
            &app,
            "POST",
            "/api/v1/memories",
            Some(json!({"user_id": USER, "messages": [{"role": "user", "content": text}], "metadata": {}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    app
}

async fn export(app: &axum::Router) -> Value {
    let (status, json) = call(app, "GET", &format!("/api/v1/users/{USER}/export"), None).await;
    assert_eq!(status, StatusCode::OK);
    json["export"].clone()
}

fn fact_ids(export: &Value) -> Vec<String> {
    let mut ids: Vec<String> = export["facts"]
        .as_array()
        .expect("facts")
        .iter()
        .map(|f| f["id"].as_str().expect("id").to_string())
        .collect();
    ids.sort();
    ids
}

async fn import(app: &axum::Router, export: &Value, mode: &str) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        &format!("/api/v1/users/{USER}/import"),
        Some(json!({"export": export, "mode": mode, "restore_events": false, "dry_run": false})),
    )
    .await
}

fn marked(export: &Value, truncated: Value) -> Value {
    let mut marked = export.clone();
    marked["truncated"] = truncated;
    marked
}

#[tokio::test]
async fn replace_with_a_marked_export_is_rejected_and_changes_nothing() {
    let app = seeded_app().await;
    let exported = export(&app).await;
    let before = fact_ids(&exported);
    assert_eq!(before.len(), 2);

    for truncated in [
        json!([{"section": "facts", "limit": 100}]),
        json!([{"section": "facts", "limit": 100}, {"section": "memory_events", "limit": 100}, {"section": "episodes", "limit": 10000}]),
    ] {
        let (status, body) = import(&app, &marked(&exported, truncated), "replace").await;
        assert!(status.is_client_error(), "{status}: {body}");
        assert!(body.to_string().contains("marked truncated"), "{body}");
        assert_eq!(
            fact_ids(&export(&app).await),
            before,
            "rejected replace must not change data"
        );
    }
}

#[tokio::test]
async fn malformed_truncation_metadata_is_rejected_in_both_modes() {
    let app = seeded_app().await;
    let exported = export(&app).await;
    let before = fact_ids(&exported);
    for truncated in [
        json!("yes"),
        json!([]),
        json!([{"section": "everything", "limit": 100}]),
        json!([{"section": "facts", "limit": 0}]),
        json!([{"section": "facts"}]),
    ] {
        for mode in ["append", "replace"] {
            let (status, body) = import(&app, &marked(&exported, truncated.clone()), mode).await;
            assert!(
                status.is_client_error(),
                "{truncated} / {mode}: {status} {body}"
            );
            assert_eq!(
                fact_ids(&export(&app).await),
                before,
                "{truncated} / {mode}: nothing written"
            );
        }
    }
}

#[tokio::test]
async fn append_with_a_marked_export_keeps_merge_semantics() {
    let app = seeded_app().await;
    let exported = export(&app).await;
    let before = fact_ids(&exported);
    // An unchanged export is not re-applied to its own user (409
    // IMPORT_ALREADY_APPLIED). Edited fact text keeps the append (merge) path under test: the live
    // ids are re-keyed and the existing facts stay.
    let mut payload = marked(&exported, json!([{"section": "facts", "limit": 100}]));
    for fact in payload["facts"].as_array_mut().expect("facts") {
        let edited = format!(
            "{} (edited after export)",
            fact["content"].as_str().expect("text")
        );
        fact["content"] = json!(edited);
    }
    let (status, body) = import(&app, &payload, "append").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.to_string().contains("TRUNCATED_EXPORT_PARTIAL"),
        "{body}"
    );
    let after = fact_ids(&export(&app).await);
    assert!(
        before.iter().all(|id| after.contains(id)),
        "append keeps existing facts"
    );
}

#[tokio::test]
async fn replace_with_a_complete_export_still_succeeds() {
    let app = seeded_app().await;
    let exported = export(&app).await;
    assert!(exported.get("truncated").is_none());
    let (status, body) = import(&app, &exported, "replace").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fact_ids(&export(&app).await).len(), 2);
}
