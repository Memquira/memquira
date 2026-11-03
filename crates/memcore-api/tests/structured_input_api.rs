//! Speaker names, message timestamps, message IDs, sessions.
//!
//! All of them are optional. A request without them must behave exactly as before Part B (P1-02);
//! a request with an unusable value must be rejected with a clear 400 rather than silently fall back.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

/// Structured extraction explicitly off: the legacy path must behave exactly as before Part B.
fn app() -> axum::Router {
    create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        ..Settings::default()
    }))
}

async fn post_memory(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
        .header("content-type", "application/json")
        .header("X-Organization-ID", "org_partb")
        .header(name, value)
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!({})))
}

fn message(extra: Value) -> Value {
    let mut message = json!({ "role": "user", "content": "I am learning Rust." });
    if let (Some(target), Some(source)) = (message.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    message
}

#[tokio::test]
async fn a_pre_part_b_payload_is_accepted_unchanged() {
    let (status, body) = post_memory(
        app(),
        json!({ "user_id": "u1", "messages": [message(json!({}))], "metadata": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "success");
    assert_eq!(body["summary"]["added"], 1);
}

#[tokio::test]
async fn all_new_fields_are_accepted_in_every_supported_timestamp_form() {
    for timestamp in [
        "2024-03-12T16:15:00Z",
        "2023-05-08T19:26:00+05:30",
        "2023-05-08",
        "8 May 2023",
        "4:15 pm on 12 March, 2024",
    ] {
        let (status, body) = post_memory(
            app(),
            json!({
                "user_id": "u1",
                "session_id": "session_7",
                "session_timestamp": "2023-05-08",
                "messages": [message(json!({
                    "name": "Priya",
                    "timestamp": timestamp,
                    "message_id": "D7:1"
                }))],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "timestamp {timestamp:?}: {body}");
    }
}

#[tokio::test]
async fn an_unreadable_message_timestamp_is_a_400_naming_the_field() {
    let (status, body) = post_memory(
        app(),
        json!({ "user_id": "u1", "messages": [message(json!({ "timestamp": "last Tuesday-ish" }))] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("messages[0].timestamp"), "{message}");
    assert!(message.contains("RFC 3339"), "{message}");
}

#[tokio::test]
async fn an_unreadable_session_timestamp_is_a_400() {
    let (status, body) = post_memory(
        app(),
        json!({ "user_id": "u1", "session_timestamp": "sometime", "messages": [message(json!({}))] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("session_timestamp")
    );
}

#[tokio::test]
async fn duplicate_message_ids_are_rejected() {
    let (status, body) = post_memory(
        app(),
        json!({
            "user_id": "u1",
            "messages": [
                message(json!({ "message_id": "D1:1" })),
                message(json!({ "message_id": "D1:1" })),
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("messages[1].message_id")
    );
}

#[tokio::test]
async fn empty_or_oversized_labels_are_rejected() {
    for (field, value) in [
        ("name", json!("   ")),
        ("message_id", json!("x".repeat(201))),
    ] {
        let (status, _) = post_memory(
            app(),
            json!({ "user_id": "u1", "messages": [message(json!({ field: value }))] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
    }
    let (status, _) = post_memory(
        app(),
        json!({ "user_id": "u1", "session_id": "", "messages": [message(json!({}))] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "session_id");
}

// ---------------------------------------------------------------------------------------------
// End to end through the router: the same request with MEMCORE_STRUCTURED_EXTRACTION_ENABLED off
// and on, read back through export (P1-02 flag-off compatibility; new fields persisted and returned).

async fn export_facts(app: axum::Router, user_id: &str) -> Vec<Value> {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/users/{user_id}/export"))
        .header("X-Organization-ID", "org_partb")
        .header(name, value)
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: Value = serde_json::from_slice(&bytes).expect("json");
    json["export"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn structured_request(user_id: &str) -> Value {
    json!({
        "user_id": user_id,
        "session_id": "session_3",
        "session_timestamp": "2023-05-08T09:00:00Z",
        "messages": [message(json!({
            "name": "Priya",
            "timestamp": "4:15 pm on 12 March, 2024",
            "message_id": "D3:1",
            "content": "I adopted a rescue dog named Biscuit."
        }))],
    })
}

#[tokio::test]
async fn flag_off_accepts_the_new_fields_but_stores_and_returns_exactly_what_it_did_before() {
    let app = app();
    let (status, body) = post_memory(app.clone(), structured_request("flag_off_user")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["summary"]["temporal_resolution"]["none"], 0);

    let facts = export_facts(app, "flag_off_user").await;
    assert_eq!(facts.len(), 1);
    for key in [
        "subject",
        "event_time",
        "observed_at",
        "session_id",
        "source_message_ids",
    ] {
        assert!(
            facts[0].get(key).is_none(),
            "flag off must not add `{key}` to the export"
        );
    }
}

#[tokio::test]
async fn flag_on_persists_and_returns_speaker_session_time_and_provenance() {
    let settings = Settings {
        structured_extraction_enabled: true,
        ..Settings::default()
    };
    let app = create_app(AppState::new(settings));
    let (status, body) = post_memory(app.clone(), structured_request("flag_on_user")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["summary"]["temporal_resolution"]["none"], 1,
        "one structured fact, with no time expression"
    );

    let facts = export_facts(app, "flag_on_user").await;
    assert_eq!(facts.len(), 1);
    let fact = &facts[0];
    assert_eq!(fact["subject"], "Priya");
    assert_eq!(fact["session_id"], "session_3");
    assert_eq!(fact["source_message_ids"], json!(["D3:1"]));
    assert_eq!(
        fact["observed_at"], "2024-03-12T16:15:00Z",
        "observed at the message's own (conversational-format) time, not the session's"
    );
}
