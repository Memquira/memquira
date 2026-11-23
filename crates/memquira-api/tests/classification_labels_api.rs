//! Classifier operation labels through the whole ingest path, against a fake OpenAI-compatible model.
//!
//! A model may spell an operation `ADD`, `add`, `Add` or `NoOp`. Every recognized spelling must reach the same
//! decision; only an unrecognized label rejects the candidate, and that rejection is reported in the write summary.
//! Repeated facts must be deduplicated as duplicates, never "deduplicated" by a dropped classification.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memquira_api::{AppState, create_app};
use memquira_config::{LlmProviderKind, ProviderTestMode, Settings};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

use common::authorization_header;

const ORG: &str = "org_labels";

/// Extraction returns `fact` (typed `memory_type`); classification answers `label`, targeting the first offered
/// fact when there is one.
struct FakeModel {
    label: Arc<Mutex<String>>,
    fact: Arc<Mutex<String>>,
    memory_type: &'static str,
}

impl Respond for FakeModel {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).expect("json body");
        let texts: Vec<String> = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["content"].as_str().map(str::to_string))
            .collect();
        let payload = texts
            .iter()
            .filter_map(|t| serde_json::from_str::<Value>(t).ok())
            .find(|v| v.get("candidate_fact").is_some());
        let content = match payload {
            Some(p) => {
                let target = p["existing_facts"][0]["id"].clone();
                json!({
                    "operation": self.label.lock().expect("label").clone(),
                    "target_fact_id": target,
                    "reason": "test",
                    "confidence": 0.9
                })
                .to_string()
            }
            None => json!({ "facts": [{ "content": self.fact.lock().expect("fact").clone(), "memory_type": self.memory_type,
                "confidence": 0.9, "importance": 0.8, "valid_at": null, "metadata": {} }] })
            .to_string(),
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30 }
        }))
    }
}

struct Model {
    app: axum::Router,
    server: MockServer,
    label: Arc<Mutex<String>>,
    fact: Arc<Mutex<String>>,
}

impl Model {
    async fn write(&self, user: &str, fact: &str) -> Value {
        *self.fact.lock().expect("fact") = fact.to_string();
        write(&self.app, user, &format!("Please remember: {fact}")).await
    }
}

async fn app_with_model(label: &str, memory_type: &'static str) -> Model {
    let server = MockServer::start().await;
    let shared = Arc::new(Mutex::new(label.to_string()));
    let fact = Arc::new(Mutex::new(String::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(FakeModel {
            label: shared.clone(),
            fact: fact.clone(),
            memory_type,
        })
        .mount(&server)
        .await;
    let mut s = Settings {
        allow_mock: true, // the embedding provider stays the deterministic mock
        ..Settings::default()
    };
    s.llm_provider = LlmProviderKind::OpenAi;
    s.llm_model = "byom-chat".into();
    s.openai_base_url = format!("{}/v1", server.uri());
    s.openai_api_key = Some("sk-INVENTED-test-key".into());
    s.real_provider_calls_enabled = true;
    s.provider_test_mode = ProviderTestMode::Production;
    Model {
        app: create_app(AppState::new(s)),
        server,
        label: shared,
        fact,
    }
}

async fn write(app: &axum::Router, user: &str, text: &str) -> Value {
    let (name, value) = authorization_header();
    let body = json!({ "user_id": user, "messages": [{ "role": "user", "content": text }], "metadata": {} });
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/memories")
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
    let json: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(status, StatusCode::OK, "{json}");
    json
}

async fn current(app: &axum::Router, user: &str) -> Vec<String> {
    let (name, value) = authorization_header();
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/users/{user}/memories"))
        .header("X-Organization-ID", ORG)
        .header(name, value)
        .body(Body::empty())
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: Value = serde_json::from_slice(&bytes).expect("json");
    json["memories"]
        .as_array()
        .expect("memories")
        .iter()
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn every_spelling_of_add_stores_the_fact() {
    for (i, label) in ["ADD", "add", "Add", "aDd", "  Add  ", "ADD\n"]
        .into_iter()
        .enumerate()
    {
        let model = app_with_model(label, "Preference").await;
        let user = format!("user_add_{i}");
        let summary = &model
            .write(&user, "The user prefers green tea in the morning.")
            .await["summary"];
        assert_eq!(summary["added"], 1, "label {label:?}: {summary}");
        assert_eq!(
            summary["discarded"]["invalid_classification"], 0,
            "label {label:?}"
        );
        assert_eq!(current(&model.app, &user).await.len(), 1, "label {label:?}");
    }
}

#[tokio::test]
async fn uppercase_memory_types_and_operations_are_both_accepted() {
    let model = app_with_model("ADD", "PREFERENCE").await;
    let summary = &model
        .write("user_types", "The user prefers oolong tea.")
        .await["summary"];
    assert_eq!(summary["added"], 1, "{summary}");
    assert_eq!(summary["skipped"]["unknown_memory_type"], 0);
}

#[tokio::test]
async fn update_delete_and_noop_in_any_case_reach_their_decision() {
    // The legacy lookup relates a candidate to a stored fact that contains its text; the stored fact here is not a
    // near-duplicate (cosine 0.85 on the mock's 8-dimension vectors, under the 0.92 threshold), so the classifier decides.
    for (label, field) in [
        ("UPDATE", "updated"),
        ("update", "updated"),
        ("Update", "updated"),
        ("DELETE", "deleted"),
        ("delete", "deleted"),
        ("Delete", "deleted"),
        ("NOOP", "noop"),
        ("NO_OP", "noop"),
        ("noop", "noop"),
        ("NoOp", "noop"),
        ("no op", "noop"),
        ("No-Op", "noop"),
    ] {
        let model = app_with_model("Add", "Preference").await;
        let user = format!("user_{}_{}", field, label.replace([' ', '-', '_'], "x"));
        assert_eq!(
            model
                .write(&user, "On cold days the user prefers green tea")
                .await["summary"]["added"],
            1
        );
        *model.label.lock().expect("label") = label.to_string();
        let summary = &model.write(&user, "the user prefers green tea").await["summary"];
        assert_eq!(summary[field], 1, "label {label:?}: {summary}");
        assert_eq!(
            summary["discarded"]["invalid_classification"], 0,
            "label {label:?}"
        );
    }
}

#[tokio::test]
async fn repeated_facts_are_deduplicated_as_duplicates() {
    // Uppercase labels must not hide a missing deduplication: the repeat is a duplicate, not a dropped candidate.
    let model = app_with_model("ADD", "Preference").await;
    let first = model
        .write("user_repeat", "The user prefers green tea in the morning.")
        .await;
    assert_eq!(first["summary"]["added"], 1);
    let calls_after_first = model
        .server
        .received_requests()
        .await
        .expect("recorded")
        .len();
    let second = &model
        .write("user_repeat", "The user prefers green tea in the morning.")
        .await["summary"];
    assert_eq!(second["added"], 0, "{second}");
    assert_eq!(second["noop"], 1, "{second}");
    assert_eq!(second["discarded"]["invalid_classification"], 0, "{second}");
    let dup = second["discarded"]["duplicate_content"]
        .as_u64()
        .unwrap_or(0)
        + second["discarded"]["duplicate_embedding"]
            .as_u64()
            .unwrap_or(0);
    assert_eq!(dup, 1, "deduplicated as a duplicate: {second}");
    // Only the extraction call: a duplicate never reaches the classifier.
    assert_eq!(
        model
            .server
            .received_requests()
            .await
            .expect("recorded")
            .len(),
        calls_after_first + 1
    );
    assert_eq!(
        current(&model.app, "user_repeat").await,
        vec!["The user prefers green tea in the morning.".to_string()]
    );
}

#[tokio::test]
async fn an_unrecognized_operation_is_rejected_and_reported() {
    for label in ["MERGE", "Append", "", "ADD_OR_UPDATE"] {
        let model = app_with_model(label, "Preference").await;
        let user = format!("user_unknown_{}", label.len());
        let summary = &model
            .write(&user, "The user prefers green tea in the morning.")
            .await["summary"];
        assert_eq!(summary["added"], 0, "label {label:?}");
        assert_eq!(
            summary["discarded"]["invalid_classification"], 1,
            "label {label:?}: {summary}"
        );
        assert!(current(&model.app, &user).await.is_empty());
    }
}
