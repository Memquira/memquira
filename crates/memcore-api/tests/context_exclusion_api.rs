//! `exclude_message_ids` on `POST /api/v1/memories/context` and item
//! provenance. Invented data only.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_v2_ctx";

fn app() -> axum::Router {
    create_app(AppState::new(Settings {
        episodes_enabled: true,
        search_max_limit: 100,
        ..Settings::default()
    }))
}

async fn call(app: &axum::Router, org: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org)
        .header(name, value)
        .body(Body::from(body.to_string()))
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

/// The same two-message conversation stored for both participants (as a client storing a
/// conversation per participant does), with stable client message ids.
async fn store_for_both(app: &axum::Router, org: &str) {
    for user in ["user_avery", "user_jordan"] {
        let (status, body) = call(
            app,
            org,
            "/api/v1/memories",
            json!({"user_id": user, "session_id": "s1", "messages": [
                {"role": "user", "content": "Avery keeps a pottery wheel in the garage.", "message_id": "m1", "name": "Avery"},
                {"role": "assistant", "content": "Jordan wants to borrow the pottery wheel.", "message_id": "m2", "name": "Jordan"}
            ], "metadata": {}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

fn episode_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "episode")
        .filter_map(|i| i["message_id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn excluding_the_other_users_episodes_removes_exact_repeats_only() {
    let app = app();
    store_for_both(&app, ORG).await;
    let ctx = |user: &str, extra: Value| {
        let mut b = json!({"user_id": user, "query": "pottery wheel", "include_items": true, "packing": "prefix"});
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        b
    };
    let (s, first) = call(
        &app,
        ORG,
        "/api/v1/memories/context",
        ctx("user_avery", json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let first_eps = episode_ids(&first);
    assert!(
        !first_eps.is_empty(),
        "episodes carry their client message id: {first}"
    );
    assert!(
        first.get("excluded_candidates").is_none(),
        "absent without exclusions"
    );

    let (_, plain) = call(
        &app,
        ORG,
        "/api/v1/memories/context",
        ctx("user_jordan", json!({})),
    )
    .await;
    let (s, deduped) = call(
        &app,
        ORG,
        "/api/v1/memories/context",
        ctx("user_jordan", json!({"exclude_message_ids": first_eps})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{deduped}");
    // the default response repeats the first user's episodes; the deduplicated one does not
    assert!(episode_ids(&plain).iter().any(|m| first_eps.contains(m)));
    assert!(episode_ids(&deduped).iter().all(|m| !first_eps.contains(m)));
    assert_eq!(
        deduped["excluded_candidates"].as_u64().unwrap() as usize,
        episode_ids(&plain)
            .iter()
            .filter(|m| first_eps.contains(m))
            .count()
    );
    // facts are never removed
    let facts = |b: &Value| {
        b["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "fact")
            .count()
    };
    assert_eq!(facts(&deduped), facts(&plain));
}

#[tokio::test]
async fn exclusion_never_reaches_another_tenant_or_user() {
    let app = app();
    store_for_both(&app, ORG).await;
    store_for_both(&app, "org_v2_other").await;
    let body = json!({"user_id": "user_jordan", "query": "pottery wheel", "include_items": true,
                      "exclude_message_ids": ["m1", "m2"]});
    let (_, other_plain) = call(
        &app,
        "org_v2_other",
        "/api/v1/memories/context",
        json!({"user_id": "user_jordan", "query": "pottery wheel", "include_items": true}),
    )
    .await;
    let (_, own) = call(&app, ORG, "/api/v1/memories/context", body).await;
    // the excluding request changes only its own context; the other organisation's is untouched
    assert!(!episode_ids(&other_plain).is_empty());
    assert!(episode_ids(&own).is_empty());
    let (_, other_again) = call(
        &app,
        "org_v2_other",
        "/api/v1/memories/context",
        json!({"user_id": "user_jordan", "query": "pottery wheel", "include_items": true}),
    )
    .await;
    assert_eq!(other_plain, other_again);
}

#[tokio::test]
async fn invalid_exclusion_lists_are_rejected() {
    let app = app();
    for bad in [
        json!([""]),
        json!(["x".repeat(201)]),
        json!((0..2001).map(|i| format!("m{i}")).collect::<Vec<_>>()),
    ] {
        let (s, body) = call(
            &app,
            ORG,
            "/api/v1/memories/context",
            json!({"user_id": "u", "query": "q", "exclude_message_ids": bad}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn items_carry_fact_provenance() {
    let app = app();
    store_for_both(&app, ORG).await;
    let (_, body) = call(
        &app,
        ORG,
        "/api/v1/memories/context",
        json!({"user_id": "user_avery", "query": "pottery wheel", "include_items": true}),
    )
    .await;
    for item in body["items"].as_array().unwrap() {
        if item["kind"] == "episode" {
            assert!(item["message_id"].is_string(), "{item}");
            assert!(item.get("source_message_ids").is_none());
        }
    }
}
