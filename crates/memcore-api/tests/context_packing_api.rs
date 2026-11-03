//! The `packing` field of `POST /api/v1/memories/context`. Without it the server default applies:
//! `prefix` (the default), or
//! `MEMCORE_CONTEXT_DEFAULT_PACKING` (`fill` / `evidence` opt in). A prefix response has no new field,
//! `fill` keeps the reference prefix and adds later lines that fit, an unknown value is refused, and
//! the tenant scope is the same. Invented data only.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{ContextPacking, Settings};
use memcore_core::d76_context::count_tokens;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_qv1_packing";

fn app() -> axum::Router {
    app_with_default(Settings::default().context_default_packing)
}

fn app_with_default(context_default_packing: ContextPacking) -> axum::Router {
    create_app(AppState::new(Settings {
        episodes_enabled: true,
        search_max_limit: 100,
        context_default_packing,
        ..Settings::default()
    }))
}

async fn call(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", ORG)
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

async fn add(app: &axum::Router, user: &str, text: &str) {
    let (status, body) = call(
        app,
        "/api/v1/memories",
        json!({"user_id": user, "messages": [{"role": "user", "content": text}], "metadata": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn context(app: &axum::Router, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({"user_id": "user_a", "query": "garden notes", "include_items": true,
                          "token_budget": 120});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    call(app, "/api/v1/memories/context", body).await
}

fn ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

async fn seeded() -> axum::Router {
    seed(app()).await
}

async fn seed(app: axum::Router) -> axum::Router {
    add(
        &app,
        "user_a",
        "Avery waters the tomato beds every morning.",
    )
    .await;
    add(
        &app,
        "user_a",
        &format!(
            "Avery wrote long garden notes: {}",
            "compost layers and mulch ".repeat(120)
        ),
    )
    .await;
    add(&app, "user_a", "Avery planted basil next to the tomatoes.").await;
    add(&app, "user_a", "Avery keeps the seed packets in a tin box.").await;
    add(
        &app,
        "user_b",
        "Jordan's private garden notes about orchids.",
    )
    .await;
    app
}

#[tokio::test]
async fn the_default_is_prefix_and_equals_explicit_prefix() {
    assert_eq!(
        Settings::default().context_default_packing,
        ContextPacking::Prefix
    );
    let app = seeded().await;
    let (status, default) = context(&app, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{default}");
    assert!(default.get("packing").is_none(), "{default}");
    assert!(default.get("lines_skipped").is_none(), "{default}");
    let (_, explicit) = context(&app, json!({"packing": "prefix"})).await;
    assert_eq!(default, explicit);
}

/// Opt-in: `MEMCORE_CONTEXT_DEFAULT_PACKING=evidence` makes a request without `packing` evidence;
/// explicit values still win, and an explicit prefix response stays field for field the default one.
#[tokio::test]
async fn the_server_default_can_opt_into_evidence_and_explicit_values_still_win() {
    let opted = seed(app_with_default(ContextPacking::Evidence)).await;
    let (status, default) = context(&opted, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{default}");
    assert_eq!(default["packing"], "evidence", "{default}");
    let (_, explicit_evidence) = context(&opted, json!({"packing": "evidence"})).await;
    assert_eq!(default, explicit_evidence);
    let (_, prefix) = context(&opted, json!({"packing": "prefix"})).await;
    assert!(prefix.get("packing").is_none(), "{prefix}");
    assert!(prefix.get("lines_skipped").is_none(), "{prefix}");
    for packing in ["fill", "evidence"] {
        let (status, body) = context(&opted, json!({ "packing": packing })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["packing"], packing);
        assert!(body["token_count"].as_u64().unwrap() <= 120);
        assert!(
            !body["text"].as_str().unwrap().contains("Jordan"),
            "cross-user leak"
        );
    }
}

#[tokio::test]
async fn fill_keeps_the_prefix_adds_lines_that_fit_and_stays_in_budget_and_tenant() {
    let app = seeded().await;
    let (_, prefix) = context(&app, json!({"packing": "prefix"})).await;
    let (status, fill) = context(&app, json!({"packing": "fill"})).await;
    assert_eq!(status, StatusCode::OK, "{fill}");
    assert_eq!(fill["packing"], "fill");

    let (prefix_ids, fill_ids) = (ids(&prefix), ids(&fill));
    assert_eq!(&fill_ids[..prefix_ids.len()], prefix_ids.as_slice());
    let text = fill["text"].as_str().unwrap();
    let tokens = fill["token_count"].as_u64().unwrap() as usize;
    assert!(tokens <= 120);
    assert_eq!(tokens, count_tokens(text));
    assert_eq!(
        fill["lines_skipped"].as_u64().unwrap() + fill["selected_count"].as_u64().unwrap(),
        fill["candidates_considered"].as_u64().unwrap()
    );
    assert_eq!(
        fill["candidates_considered"],
        prefix["candidates_considered"]
    );
    assert!(!text.contains("Jordan"), "cross-user leak");
}

#[tokio::test]
async fn an_unknown_packing_is_refused() {
    let app = seeded().await;
    let (status, body) = context(&app, json!({"packing": "greedy"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("packing"), "{body}");
}

/// Retrieval Quality: the opt-in `evidence` packing stays in budget and tenant, keeps exact candidate
/// lines, and reports `packing` / `lines_skipped` like `fill`; the default is unchanged.
#[tokio::test]
async fn evidence_stays_in_budget_and_tenant_with_exact_candidate_lines() {
    let app = seeded().await;
    let (status, evidence) = context(&app, json!({"packing": "evidence"})).await;
    assert_eq!(status, StatusCode::OK, "{evidence}");
    assert_eq!(evidence["packing"], "evidence");
    let text = evidence["text"].as_str().unwrap();
    let tokens = evidence["token_count"].as_u64().unwrap() as usize;
    assert!(tokens <= 120);
    assert_eq!(tokens, count_tokens(text));
    assert_eq!(
        evidence["lines_skipped"].as_u64().unwrap() + evidence["selected_count"].as_u64().unwrap(),
        evidence["candidates_considered"].as_u64().unwrap()
    );
    assert!(!text.contains("Jordan"), "cross-user leak");
    let (_, fill) = context(&app, json!({"packing": "fill", "token_budget": 4000})).await;
    let fill_lines: std::collections::HashSet<String> = fill["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["line"].as_str().unwrap().to_string())
        .collect();
    for item in evidence["items"].as_array().unwrap() {
        assert!(
            fill_lines.contains(item["line"].as_str().unwrap()),
            "evidence line is not a candidate line: {item}"
        );
    }
    let (_, default) = context(&app, json!({})).await;
    assert!(default.get("packing").is_none(), "{default}");
}
