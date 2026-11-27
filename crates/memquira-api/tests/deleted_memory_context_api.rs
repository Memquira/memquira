//! `DELETE /api/v1/users/{user}/memories/{id}` then `POST /api/v1/memories/context` and
//! `POST /api/v1/episodes/search`: with episodes on, the message a deleted memory was extracted
//! from no longer returns its text, while other memories and messages are unchanged. Invented data
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

const ORG: &str = "org_delete_context";
const USER: &str = "user_delete_context";

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

async fn add(app: &axum::Router, id: &str, text: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/memories",
        Some(json!({
            "user_id": USER,
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

async fn context(app: &axum::Router) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/memories/context",
        Some(json!({"user_id": USER, "query": "what does the user keep?", "token_budget": 2000})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["text"].as_str().expect("context text").to_string()
}

async fn episodes(app: &axum::Router) -> Vec<String> {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/episodes/search",
        Some(json!({"user_id": USER, "query": "what does the user keep?", "limit": 20})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| result["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_deleted_memory_does_not_come_back_through_its_message() {
    let app = app();
    let wheel = add(&app, "m1", "I keep a pottery wheel in the garage.").await;
    add(&app, "m2", "My rabbit is called Pebble.").await;
    assert!(context(&app).await.contains("pottery wheel"));
    assert_eq!(episodes(&app).await.len(), 2);

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v1/users/{USER}/memories/{wheel}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let text = context(&app).await;
    assert!(!text.contains("pottery wheel"), "{text}");
    assert!(text.contains("Pebble"), "{text}");
    let found = episodes(&app).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("Pebble"));

    // A repeated delete does not bring anything back.
    call(
        &app,
        "DELETE",
        &format!("/api/v1/users/{USER}/memories/{wheel}"),
        None,
    )
    .await;
    assert!(!context(&app).await.contains("pottery wheel"));
}
