//! The EXPERIMENTAL `POST /api/v1/memories/candidates`.
//!
//! Off by default (400). On, it returns the user's semantic fact results and episode results as one
//! list in the E-merge order, the order a client would build from the two existing
//! endpoints, and nothing from any other user.

mod common;

use std::cmp::Ordering;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_candidates_api";
const USER: &str = "user_candidates_api";
const OTHER: &str = "other_candidates_api";

async fn call(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", ORG)
        .header(name, value)
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn app(episodes_enabled: bool, candidates_enabled: bool) -> axum::Router {
    create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        episodes_enabled,
        experimental_candidates_enabled: candidates_enabled,
        ..Settings::default()
    }))
}

async fn add(app: &axum::Router, user: &str, session: &str, lines: &[(&str, &str)]) {
    let messages: Vec<Value> = lines
        .iter()
        .map(|(id, content)| {
            json!({ "role": "user", "name": "Rowena", "message_id": id, "content": content })
        })
        .collect();
    let (status, body) = call(
        app.clone(),
        "/api/v1/memories",
        json!({
            "user_id": user,
            "session_id": session,
            "session_timestamp": "2023-05-08T13:00:00Z",
            "messages": messages,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn seed(app: &axum::Router) {
    add(
        app,
        USER,
        "S1",
        &[
            (
                "D1:1",
                "I adopted a rescue greyhound named Juniper last spring.",
            ),
            (
                "D1:2",
                "Juniper loves long walks along the canal every morning.",
            ),
            (
                "D1:3",
                "My sister Ixora gave me a copper kettle for my birthday.",
            ),
        ],
    )
    .await;
    add(
        app,
        USER,
        "S2",
        &[
            (
                "D2:1",
                "I adopted a rescue greyhound named Juniper last spring.",
            ),
            (
                "D2:2",
                "We are repainting the kitchen a pale sage green this weekend.",
            ),
        ],
    )
    .await;
    add(
        app,
        OTHER,
        "S9",
        &[(
            "D9:1",
            "Pellam keeps a secret greyhound sanctuary in the hills.",
        )],
    )
    .await;
}

/// The harness's E-merge key, rebuilt from the two existing endpoints' responses.
fn reference_merge(facts: &Value, episodes: &Value) -> Vec<String> {
    struct Item {
        id: String,
        score: f64,
        content: String,
        provenance: String,
        kind: &'static str,
        rank: usize,
    }
    let mut items: Vec<Item> = Vec::new();
    for (rank, fact) in facts["results"].as_array().unwrap().iter().enumerate() {
        let sources: Vec<String> = fact["source_message_ids"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|id| id.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default();
        items.push(Item {
            id: fact["fact_id"].as_str().unwrap().to_string(),
            score: fact["vector_score"]
                .as_f64()
                .expect("semantic vector score"),
            content: fact["content"].as_str().unwrap().to_string(),
            provenance: sources.join(","),
            kind: "fact",
            rank,
        });
    }
    for (rank, episode) in episodes["results"].as_array().unwrap().iter().enumerate() {
        items.push(Item {
            id: episode["episode_id"].as_str().unwrap().to_string(),
            score: episode["vector_score"].as_f64().unwrap(),
            content: episode["content"].as_str().unwrap().to_string(),
            provenance: episode["message_id"].as_str().unwrap_or("").to_string(),
            kind: "episode",
            rank,
        });
    }
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.content.cmp(&b.content))
            .then_with(|| a.provenance.cmp(&b.provenance))
            .then_with(|| a.kind.cmp(b.kind))
            .then_with(|| a.rank.cmp(&b.rank))
    });
    items.into_iter().map(|item| item.id).collect()
}

#[tokio::test]
async fn candidates_are_off_by_default_and_need_episodes() {
    let body = json!({ "user_id": USER, "query": "greyhound" });
    for (episodes, candidates) in [(false, false), (true, false), (false, true)] {
        let (status, response) = call(
            app(episodes, candidates),
            "/api/v1/memories/candidates",
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    }
    assert!(!Settings::default().experimental_candidates_enabled);
}

#[tokio::test]
async fn candidates_reproduce_the_e_merge_order_of_the_two_searches() {
    let app = app(true, true);
    seed(&app).await;

    for query in [
        "greyhound Juniper",
        "birthday present kettle",
        "kitchen paint",
    ] {
        let (status, facts) = call(
            app.clone(),
            "/api/v1/memories/search",
            json!({ "user_id": USER, "query": query, "limit": 50, "retrieval_mode": "semantic" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{facts}");
        let (status, episodes) = call(
            app.clone(),
            "/api/v1/episodes/search",
            json!({ "user_id": USER, "query": query, "limit": 50 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{episodes}");
        let (status, candidates) = call(
            app.clone(),
            "/api/v1/memories/candidates",
            json!({ "user_id": USER, "query": query, "fact_limit": 50, "episode_limit": 50 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{candidates}");

        let results = candidates["results"].as_array().unwrap();
        let ids: Vec<String> = results
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, reference_merge(&facts, &episodes), "query {query}");
        assert!(results.iter().any(|item| item["kind"] == "episode"));
        assert!(
            results.iter().any(|item| item["kind"] == "fact"),
            "{candidates}"
        );
        for item in results {
            assert!(item["vector_score"].as_f64().is_some());
            assert!(item["rank"].as_u64().unwrap() >= 1);
            if item["kind"] == "episode" {
                assert_eq!(item["speaker"], "Rowena");
                assert!(
                    item["event_time"].is_null(),
                    "episodes never carry event time"
                );
                assert!(item["message_id"].as_str().is_some());
            }
            assert!(
                !item["content"]
                    .as_str()
                    .unwrap()
                    .contains("secret greyhound sanctuary"),
                "another user's data leaked"
            );
        }
    }
}

#[tokio::test]
async fn candidates_default_to_the_capped_d76_depths_and_validate_input() {
    let app = app(true, true);
    seed(&app).await;
    // Defaults: 100 each, capped at MEMCORE_SEARCH_MAX_LIMIT (50 by default), so no 400.
    let (status, body) = call(
        app.clone(),
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": "greyhound" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for bad in [
        json!({ "user_id": USER, "query": "greyhound", "fact_limit": 0 }),
        json!({ "user_id": USER, "query": "greyhound", "episode_limit": 51 }),
        json!({ "user_id": USER, "query": "   " }),
        json!({ "user_id": "  ", "query": "greyhound" }),
    ] {
        let (status, body) = call(app.clone(), "/api/v1/memories/candidates", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn openapi_documents_the_experimental_candidates_endpoint() {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("GET")
        .uri("/openapi.json")
        .header(name, value)
        .body(Body::empty())
        .expect("request");
    let response = app(false, false).oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    let path = &doc["paths"]["/api/v1/memories/candidates"]["post"];
    assert!(path.is_object(), "path missing");
    assert!(
        path["description"]
            .as_str()
            .unwrap_or_default()
            .contains("EXPERIMENTAL")
            || path["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("EXPERIMENTAL")
    );
    assert!(doc["components"]["schemas"]["CandidateResponse"].is_object());
}

fn pipeline_app(
    analysis: bool,
    temporal: bool,
    multi_query: memcore_config::MultiQuerySetting,
) -> axum::Router {
    create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        episodes_enabled: true,
        experimental_candidates_enabled: true,
        query_analysis_enabled: analysis,
        temporal_ranking_enabled: temporal,
        multi_query,
        ..Settings::default()
    }))
}

fn ids(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn flags_off_ignore_reference_time_and_analysis_requests() {
    let app = app(true, true);
    seed(&app).await;
    let plain = json!({ "user_id": USER, "query": "When did Rowena adopt Juniper in 2022?" });
    let (_, baseline) = call(app.clone(), "/api/v1/memories/candidates", plain).await;
    let (status, with_extras) = call(
        app.clone(),
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": "When did Rowena adopt Juniper in 2022?",
                "reference_time": "2023-03-15T12:00:00Z", "include_analysis": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{with_extras}");
    assert_eq!(ids(&with_extras), ids(&baseline));
    assert_eq!(with_extras["stages"], json!([]));
    assert!(with_extras.get("analysis").is_none());
    assert!(with_extras.get("rerank_fallback").is_none());
}

#[tokio::test]
async fn analysis_and_temporal_ranking_run_only_when_enabled() {
    let app = pipeline_app(true, true, memcore_config::MultiQuerySetting::None);
    seed(&app).await;
    let (status, body) = call(
        app.clone(),
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": "What did Rowena buy yesterday?",
                "reference_time": "2023-03-15T02:00:00+05:30", "include_analysis": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stages"], json!(["temporal_ranking"]));
    assert_eq!(body["analysis"]["question_type"], "factoid");
    assert_eq!(body["analysis"]["relative_expression"], "yesterday");
    assert_eq!(
        body["analysis"]["temporal_constraint"]["start"],
        "2023-03-13T00:00:00Z"
    );
    // No stored fact has an event time here (structured extraction is off), and episodes are never
    // event time, so the order is the baseline order.
    let baseline = baseline_contents("What did Rowena buy yesterday?").await;
    assert_eq!(contents(&body), baseline);

    let (status, _) = call(
        app.clone(),
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": "greyhound", "reference_time": "yesterday" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn contents(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| format!("{}|{}", item["kind"], item["content"]))
        .collect()
}

/// The baseline list's items (kind and content) from a fresh app with every flag off; ids
/// are random per app, so they are not compared.
async fn baseline_contents(query: &str) -> Vec<String> {
    let app = app(true, true);
    seed(&app).await;
    let (_, body) = call(
        app,
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": query }),
    )
    .await;
    contents(&body)
}

#[tokio::test]
async fn deterministic_multi_query_unions_clause_retrievals_without_duplicates() {
    let app = pipeline_app(
        true,
        false,
        memcore_config::MultiQuerySetting::Deterministic,
    );
    seed(&app).await;
    let query = "Where does Rowena walk Juniper and what did Ixora give her?";
    let (status, body) = call(
        app.clone(),
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": query, "include_analysis": true,
                "fact_limit": 3, "episode_limit": 3 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stages"], json!(["multi_query"]));
    assert_eq!(
        body["analysis"]["sub_queries"],
        json!([
            "Where does Rowena walk Juniper?",
            "what did Ixora give her?"
        ])
    );
    let merged = ids(&body);
    let unique: std::collections::HashSet<&String> = merged.iter().collect();
    assert_eq!(unique.len(), merged.len(), "no duplicates");
    // A single-clause question never splits.
    let (_, single) = call(
        app,
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": "What did Ixora and Rowena share?", "include_analysis": true }),
    )
    .await;
    assert_eq!(single["stages"], json!([]));
    assert_eq!(single["analysis"]["sub_queries"], json!([]));
}

#[tokio::test]
async fn noop_decomposition_is_audited_and_changes_nothing() {
    let app = create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        episodes_enabled: true,
        experimental_candidates_enabled: true,
        query_decomposition: memcore_config::QueryDecompositionSetting::Noop,
        ..Settings::default()
    }));
    seed(&app).await;
    let (status, body) = call(
        app,
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": "greyhound Juniper" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stages"], json!(["decomposition:noop"]));
    assert_eq!(body["decomposition"]["state"], "called");
    assert_eq!(body["decomposition"]["operations"], 1);
    assert_eq!(body["decomposition"]["accepted"], json!([]));
    assert_eq!(
        contents(&body),
        baseline_contents("greyhound Juniper").await
    );
}

#[tokio::test]
async fn llm_decomposition_runs_once_per_question_across_speakers() {
    // through the HTTP endpoint with the mock LLM (which never decomposes): the second
    // speaker's request for the same question reuses the first request's operation.
    let app = create_app(AppState::new(Settings {
        structured_extraction_enabled: false,
        episodes_enabled: true,
        experimental_candidates_enabled: true,
        query_decomposition: memcore_config::QueryDecompositionSetting::Llm,
        ..Settings::default()
    }));
    seed(&app).await;
    let question = "Did Rowena or Dunstan bake more for the harvest festival?";
    let (status, first) = call(
        app.clone(),
        "/api/v1/memories/candidates",
        json!({ "user_id": USER, "query": question }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["stages"], json!(["decomposition:llm"]));
    assert_eq!(first["decomposition"]["state"], "called");
    assert_eq!(first["decomposition"]["operations"], 1);
    let (status, second) = call(
        app,
        "/api/v1/memories/candidates",
        json!({ "user_id": OTHER, "query": question }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["decomposition"]["state"], "cache_hit");
    assert_eq!(second["decomposition"]["operations"], 0);
    assert_eq!(
        second["decomposition"]["fingerprint"],
        first["decomposition"]["fingerprint"]
    );
    // no query text in the audit beyond the accepted sub-queries
    assert!(!first["decomposition"].to_string().contains("harvest"));
}
