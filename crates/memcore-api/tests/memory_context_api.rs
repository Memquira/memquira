//! `POST /api/v1/memories/context` (auth, validation, rendering contract, tenant scope).
//! Invented data only.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use memcore_core::d76_context::count_tokens;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_t5_ctx";

fn app(episodes: bool, max_limit: usize) -> axum::Router {
    create_app(AppState::new(Settings {
        episodes_enabled: episodes,
        search_max_limit: max_limit,
        ..Settings::default()
    }))
}

async fn call(
    app: &axum::Router,
    org: &str,
    uri: &str,
    body: Value,
    auth: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org);
    if auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!({})))
}

async fn add(app: &axum::Router, org: &str, user: &str, text: &str) {
    let (status, _) = call(
        app,
        org,
        "/api/v1/memories",
        json!({"user_id": user, "messages": [{"role": "user", "content": text}], "metadata": {}}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

fn context_body(user: &str, extra: Value) -> Value {
    let mut body = json!({"user_id": user, "query": "pottery wheel"});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

#[tokio::test]
async fn requires_authentication() {
    let (status, _) = call(
        &app(false, 100),
        ORG,
        "/api/v1/memories/context",
        context_body("u1", json!({})),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn validates_budget_render_mode_and_search_depth() {
    let app100 = app(false, 100);
    for extra in [
        json!({"token_budget": 0}),
        json!({"token_budget": 4001}),
        json!({"render_mode": "compact"}),
    ] {
        let (status, body) = call(
            &app100,
            ORG,
            "/api/v1/memories/context",
            context_body("u1", extra.clone()),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{extra}: {body}");
    }
    let (status, body) = call(
        &app(false, 50),
        ORG,
        "/api/v1/memories/context",
        context_body("u1", json!({})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.to_string().contains("MEMCORE_SEARCH_MAX_LIMIT"),
        "{body}"
    );
}

#[tokio::test]
async fn renders_lines_with_exact_token_accounting_and_stays_in_tenant() {
    for episodes in [false, true] {
        let app = app(episodes, 100);
        add(
            &app,
            ORG,
            "user_a",
            "Avery keeps a pottery wheel in the garage.",
        )
        .await;
        add(
            &app,
            ORG,
            "user_a",
            "Avery adopted a grey rabbit named Pebble.",
        )
        .await;
        add(
            &app,
            "org_other",
            "user_a",
            "Secret note from another organization about pottery.",
        )
        .await;
        add(
            &app,
            ORG,
            "user_b",
            "Jordan's private pottery class schedule.",
        )
        .await;

        let (status, body) = call(
            &app,
            ORG,
            "/api/v1/memories/context",
            // The reference rendering contract (prefix); the default packing is covered by
            // context_packing_api.
            context_body(
                "user_a",
                json!({"include_items": true, "packing": "prefix"}),
            ),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["render_mode"], "d76");
        assert_eq!(
            body["source"],
            if episodes { "e_merge" } else { "facts_only" }
        );
        let text = body["text"].as_str().unwrap();
        assert_eq!(
            body["token_count"].as_u64().unwrap() as usize,
            count_tokens(text)
        );
        assert_eq!(body["token_budget"], 1500);
        let items = body["items"].as_array().unwrap();
        assert_eq!(
            body["selected_count"].as_u64().unwrap() as usize,
            items.len()
        );
        assert!(!items.is_empty());
        let lines: Vec<&str> = items.iter().map(|i| i["line"].as_str().unwrap()).collect();
        assert_eq!(lines.join("\n"), text, "text is the joined lines");
        for item in items {
            assert_eq!(
                item["line"].as_str().unwrap(),
                format!(
                    "{}: {}",
                    item["timestamp"].as_str().unwrap(),
                    item["text"].as_str().unwrap()
                )
            );
            assert_eq!(
                item["tokens"].as_u64().unwrap() as usize,
                count_tokens(item["line"].as_str().unwrap())
            );
        }
        assert!(!text.contains("another organization"), "cross-org leak");
        assert!(!text.contains("Jordan"), "cross-user leak");
        if episodes {
            assert!(
                items.iter().any(|i| i["kind"] == "episode"),
                "episodes are merged in"
            );
        }

        // A tiny budget is binding and keeps a strict prefix.
        let (_, small) = call(
            &app,
            ORG,
            "/api/v1/memories/context",
            context_body(
                "user_a",
                json!({"token_budget": 12, "include_items": true, "packing": "prefix"}),
            ),
            true,
        )
        .await;
        assert_eq!(small["budget_bound"], true);
        assert!(small["token_count"].as_u64().unwrap() <= 12);
        let small_lines: Vec<&str> = small["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["line"].as_str().unwrap())
            .collect();
        assert_eq!(
            small_lines.as_slice(),
            &lines[..small_lines.len()],
            "prefix of the full context"
        );

        // Items are omitted unless requested.
        let (_, no_items) = call(
            &app,
            ORG,
            "/api/v1/memories/context",
            context_body("user_a", json!({})),
            true,
        )
        .await;
        assert!(no_items.get("items").is_none());
    }
}
