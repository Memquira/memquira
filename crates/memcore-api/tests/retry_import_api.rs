//! The re-drivable delete
//! (R3, protocol D2-D4) and the fail-closed append import (R7) as clients see them.
//!
//! Every request carries the same `X-Request-ID`, so response bodies can be compared byte for byte.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const REQUEST_ID: &str = "ar023-s2-fixed-request-id";
const CONTENT: &str = "Memory CANARY-org_a-user_a-1 about restoring vintage radios";

fn request(method: &str, uri: &str, org: &str, body: Option<String>) -> Request<Body> {
    let (name, value) = authorization_header();
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Organization-ID", org)
        .header("X-Request-ID", REQUEST_ID)
        .header(name, value);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body)),
        None => builder.body(Body::empty()),
    }
    .expect("request")
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, bytes)
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("json body")
}

async fn seed(app: &axum::Router, org: &str, user: &str, content: &str) -> Uuid {
    let body = serde_json::json!({
        "user_id": user,
        "messages": [{ "role": "user", "content": content }],
        "metadata": {}
    })
    .to_string();
    let (status, bytes) = send(app, request("POST", "/api/v1/memories", org, Some(body))).await;
    assert_eq!(status, StatusCode::OK);
    Uuid::parse_str(json(&bytes)["memories"][0]["id"].as_str().expect("id")).expect("uuid")
}

async fn delete(app: &axum::Router, org: &str, user: &str, id: Uuid) -> (StatusCode, Vec<u8>) {
    send(
        app,
        request(
            "DELETE",
            &format!("/api/v1/users/{user}/memories/{id}"),
            org,
            None,
        ),
    )
    .await
}

#[tokio::test]
async fn d2_d3_repeated_delete_of_own_memory_is_200_with_the_same_body() {
    let app = create_app(AppState::new(Settings::default()));
    let id = seed(&app, "org_a", "user_a", CONTENT).await;
    let first = delete(&app, "org_a", "user_a", id).await;
    let second = delete(&app, "org_a", "user_a", id).await;
    let third = delete(&app, "org_a", "user_a", id).await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(json(&first.1)["deleted"], true);
    assert_eq!(second, first, "a retry of a deleted memory: 200, same body");
    assert_eq!(third, first);
}

#[tokio::test]
async fn d4_unknown_other_user_and_other_org_ids_get_byte_identical_404s() {
    let app = create_app(AppState::new(Settings::default()));
    let live = seed(&app, "org_a", "user_a", CONTENT).await;
    let deleted = seed(
        &app,
        "org_a",
        "user_a",
        "A second memory about sailing small boats",
    )
    .await;
    assert_eq!(
        delete(&app, "org_a", "user_a", deleted).await.0,
        StatusCode::OK
    );

    let responses = vec![
        delete(&app, "org_a", "user_a", Uuid::new_v4()).await, // unknown
        delete(&app, "org_a", "user_b", live).await,           // same org, other user, live
        delete(&app, "org_a", "user_b", deleted).await,        // same org, other user, deleted
        delete(&app, "org_b", "user_a", live).await,           // other org, same user id, live
        delete(&app, "org_b", "user_a", deleted).await,        // other org, same user id, deleted
    ];
    for (status, _) in &responses {
        assert_eq!(*status, StatusCode::NOT_FOUND);
    }
    assert!(
        responses.windows(2).all(|pair| pair[0].1 == pair[1].1),
        "byte-identical 404 bodies"
    );
    assert_eq!(json(&responses[0].1)["error"]["code"], "NOT_FOUND");
    // The live memory was not touched by the other tenants' attempts.
    assert_eq!(
        delete(&app, "org_a", "user_a", live).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn r7_append_import_conflicts_map_to_the_approved_409_codes() {
    let app = create_app(AppState::new(Settings::default()));
    let id = seed(&app, "org_a", "user_a", CONTENT).await;
    let (status, bytes) = send(
        &app,
        request("GET", "/api/v1/users/user_a/export", "org_a", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let export = json(&bytes)["export"].clone();
    let import_body =
        serde_json::json!({ "export": export, "mode": "append", "dry_run": false }).to_string();

    let (status, bytes) = send(
        &app,
        request(
            "POST",
            "/api/v1/users/user_a/import",
            "org_a",
            Some(import_body.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let body = json(&bytes);
    assert_eq!(body["error"]["code"], "IMPORT_ALREADY_APPLIED");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(
        !text.contains(&id.to_string()) && !text.contains("CANARY"),
        "no id or content"
    );

    assert_eq!(delete(&app, "org_a", "user_a", id).await.0, StatusCode::OK);
    let (status, bytes) = send(
        &app,
        request(
            "POST",
            "/api/v1/users/user_a/import",
            "org_a",
            Some(import_body),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json(&bytes)["error"]["code"], "IMPORT_ID_CONFLICT");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(!text.contains(&id.to_string()) && !text.contains("CANARY"));

    // Nothing was imported by either rejected request.
    let (status, bytes) = send(
        &app,
        request("GET", "/api/v1/users/user_a/memories", "org_a", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json(&bytes)["memories"]
            .as_array()
            .expect("list")
            .is_empty()
    );
}
