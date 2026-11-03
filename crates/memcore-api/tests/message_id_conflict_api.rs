//! A client `message_id` reused for a different message is rejected with 409
//! `MESSAGE_ID_CONFLICT` before extraction, and nothing from the request is stored; the same message
//! under the same ID is a retry (resume); distinct IDs and messages without an ID are unchanged.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_message_id_conflict";
const USER: &str = "user_message_id_conflict";
const ORIGINAL: &str = "I adopted a rescue greyhound named Juniper last spring.";
const CHANGED: &str = "I sold my sailboat and moved to Lisbon in the autumn.";

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
        ..Settings::default()
    }))
}

fn message(id: Option<&str>, content: &str) -> Value {
    let mut message = json!({ "role": "user", "name": "Priya", "content": content });
    if let Some(id) = id {
        message["message_id"] = json!(id);
    }
    message
}

fn request(user: &str, session: Option<&str>, messages: Vec<Value>) -> Value {
    let mut body = json!({
        "user_id": user,
        "session_timestamp": "2023-05-08T13:00:00Z",
        "messages": messages,
    });
    if let Some(session) = session {
        body["session_id"] = json!(session);
    }
    body
}

async fn post(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    call(app.clone(), "POST", "/api/v1/memories", Some(body)).await
}

/// (episodes, facts, memory events) of `user`, from the user export.
async fn counts(app: &axum::Router, user: &str) -> (usize, usize, usize) {
    let (status, export) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/users/{user}/export"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export}");
    let len = |key: &str| {
        export["export"][key]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
    };
    (len("episodes"), len("facts"), len("memory_events"))
}

async fn episode_contents(app: &axum::Router, user: &str) -> Vec<String> {
    let (_, export) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/users/{user}/export"),
        None,
    )
    .await;
    export["export"]["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|episode| episode["content"].as_str().unwrap().to_string())
        .collect()
}

fn assert_conflict(status: StatusCode, body: &Value, position: usize) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "MESSAGE_ID_CONFLICT", "{body}");
    let text = body.to_string();
    assert!(
        text.contains(&format!("messages[{position}]")),
        "position missing: {text}"
    );
    for secret in ["m-1", "greyhound", "sailboat", "Priya", "Rowena"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
}

#[tokio::test]
async fn reuse_for_a_different_message_is_409_and_stores_nothing() {
    let app = app(true);
    let (status, body) = post(
        &app,
        request(USER, Some("S1"), vec![message(Some("m-1"), ORIGINAL)]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = counts(&app, USER).await;
    assert_eq!(before.0, 1);
    // The mock extraction stores facts, so "no new facts" below is a real check.
    assert!(before.1 >= 1, "{before:?}");

    // Identical retry: 200, the stored message is reused.
    let (status, body) = post(
        &app,
        request(USER, Some("S1"), vec![message(Some("m-1"), ORIGINAL)]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(counts(&app, USER).await.0, 1);
    let after_retry = counts(&app, USER).await;

    // Changed content, role, speaker or time under the same ID: 409, nothing stored.
    let mut changed_role = message(Some("m-1"), ORIGINAL);
    changed_role["role"] = json!("assistant");
    let mut changed_name = message(Some("m-1"), ORIGINAL);
    changed_name["name"] = json!("Rowena");
    let mut own_time = message(Some("m-1"), ORIGINAL);
    own_time["timestamp"] = json!("2023-05-09T09:00:00Z");
    let mut other_session_time = request(USER, Some("S1"), vec![message(Some("m-1"), ORIGINAL)]);
    other_session_time["session_timestamp"] = json!("2024-01-01T00:00:00Z");
    for (case, body) in [
        (
            "content",
            request(USER, Some("S1"), vec![message(Some("m-1"), CHANGED)]),
        ),
        ("role", request(USER, Some("S1"), vec![changed_role])),
        ("name", request(USER, Some("S1"), vec![changed_name])),
        ("message time", request(USER, Some("S1"), vec![own_time])),
        ("session time", other_session_time),
    ] {
        let (status, response) = post(&app, body).await;
        assert_conflict(status, &response, 0);
        assert_eq!(counts(&app, USER).await, after_retry, "{case}");
    }
    assert_eq!(
        episode_contents(&app, USER).await,
        vec![ORIGINAL.to_string()]
    );
}

#[tokio::test]
async fn a_mixed_batch_with_one_conflict_stores_nothing() {
    let app = app(true);
    let (status, _) = post(
        &app,
        request(USER, Some("S1"), vec![message(Some("m-1"), ORIGINAL)]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before = counts(&app, USER).await;

    let (status, body) = post(
        &app,
        request(
            USER,
            Some("S1"),
            vec![
                message(
                    Some("m-2"),
                    "My sister Amara is a marine biologist in Cape Town.",
                ),
                message(
                    None,
                    "We are planning a hiking trip to Patagonia next year.",
                ),
                message(Some("m-1"), CHANGED),
            ],
        ),
    )
    .await;
    assert_conflict(status, &body, 2);
    assert_eq!(counts(&app, USER).await, before);
    assert_eq!(
        episode_contents(&app, USER).await,
        vec![ORIGINAL.to_string()]
    );
}

#[tokio::test]
async fn scopes_distinct_ids_and_messages_without_ids_are_unchanged() {
    let app = app(true);
    let first = request(USER, Some("S1"), vec![message(Some("m-1"), ORIGINAL)]);
    assert_eq!(post(&app, first).await.0, StatusCode::OK);

    // The same ID in another session, the session-less scope, another user: separate messages.
    for body in [
        request(USER, Some("S2"), vec![message(Some("m-1"), CHANGED)]),
        request(USER, None, vec![message(Some("m-1"), CHANGED)]),
        request(
            "another_user",
            Some("S1"),
            vec![message(Some("m-1"), CHANGED)],
        ),
    ] {
        let (status, response) = post(&app, body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
    }
    // The session-less scope is itself one scope: reusing the ID there is a conflict.
    let (status, body) = post(
        &app,
        request(USER, None, vec![message(Some("m-1"), ORIGINAL)]),
    )
    .await;
    assert_conflict(status, &body, 0);

    // Distinct IDs with identical text are both kept; messages without an ID are always stored.
    let text = "We met for coffee at the harbour on Sunday morning.";
    let (status, _) = post(
        &app,
        request(
            USER,
            Some("S3"),
            vec![message(Some("c-1"), text), message(Some("c-2"), text)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..2 {
        let (status, _) = post(&app, request(USER, Some("S3"), vec![message(None, text)])).await;
        assert_eq!(status, StatusCode::OK);
    }
    let stored = episode_contents(&app, USER).await;
    assert_eq!(stored.iter().filter(|content| *content == text).count(), 4);
}

#[tokio::test]
async fn without_episode_storage_ids_are_not_checked() {
    // Documented: with MEMCORE_EPISODES_ENABLED off memcore keeps no message IDs.
    let app = app(false);
    for content in [ORIGINAL, CHANGED] {
        let (status, body) = post(
            &app,
            request(USER, Some("S1"), vec![message(Some("m-1"), content)]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}
