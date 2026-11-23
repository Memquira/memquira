//! Correction cases replayed through the whole ingest path with the opt-in update configuration (semantic related-fact
//! matcher, operations prompt, value guard, ...), against a fake OpenAI-compatible model that returns extraction
//! outputs recorded from Gemini 3.1 Flash-Lite. No timestamps are sent, as by the hosted preview at the time, so
//! yearless dates stay unanchored (no event time; only the expression as written).
//!
//! - A rescheduled appointment whose date exists only in the expression ("March 3" -> "March 10"): the classifier
//!   answered NoOp. With the value guard the new date is kept; without it, it was lost.
//! - Three known gaps (ignored): the old fact is never offered to the classifier because the extraction gave the new
//!   fact a different subject. They pin the failing stage (related-fact lookup) for a later fix.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memquira_api::{AppState, create_app};
use memquira_config::{
    ClassificationPromptSetting, DedupValueGuardSetting, EventUpdatePolicySetting,
    ImportancePolicySetting, LlmProviderKind, ProviderTestMode, RelatedFactMatcher, Settings,
    StructuredExtractionPolicy, TemporalRewriteSetting, UnknownMemoryTypeSetting,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

use common::authorization_header;

const ORG: &str = "org_fixtures";

#[derive(Default)]
struct Script {
    /// Extraction replies, one per write, in order.
    extractions: VecDeque<Value>,
    /// The classifier's operation for every classification call.
    operation: String,
    /// How many existing facts each classification call was offered.
    offered: Vec<usize>,
}

struct FakeModel(Arc<Mutex<Script>>);

impl Respond for FakeModel {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).expect("json body");
        let payload = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["content"].as_str())
            .filter_map(|t| serde_json::from_str::<Value>(t).ok())
            .find(|v| v.get("candidate_fact").is_some());
        let mut script = self.0.lock().expect("script");
        let content = match payload {
            Some(p) => {
                script
                    .offered
                    .push(p["existing_facts"].as_array().map_or(0, Vec::len));
                json!({ "operation": script.operation, "target_fact_id": p["existing_facts"][0]["id"],
                        "reason": "recorded", "confidence": 1.0 })
                .to_string()
            }
            None => script
                .extractions
                .pop_front()
                .unwrap_or_else(|| json!({ "facts": [] }))
                .to_string(),
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30 }
        }))
    }
}

async fn app(value_guard: bool, script: Arc<Mutex<Script>>) -> (axum::Router, MockServer) {
    app_with(value_guard, RelatedFactMatcher::SemanticV23, script).await
}

async fn app_with(
    value_guard: bool,
    matcher: RelatedFactMatcher,
    script: Arc<Mutex<Script>>,
) -> (axum::Router, MockServer) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(FakeModel(script))
        .mount(&server)
        .await;
    let mut s = Settings {
        allow_mock: true,
        ..Settings::default()
    };
    s.llm_provider = LlmProviderKind::OpenAi;
    s.llm_model = "byom-chat".into();
    s.openai_base_url = format!("{}/v1", server.uri());
    s.openai_api_key = Some("sk-INVENTED-test-key".into());
    s.real_provider_calls_enabled = true;
    s.provider_test_mode = ProviderTestMode::Production;
    s.structured_extraction_enabled = true;
    s.structured_extraction_policy = StructuredExtractionPolicy::Retentive;
    s.related_fact_matcher = matcher;
    s.classification_prompt = ClassificationPromptSetting::OperationsV1;
    s.importance_policy = ImportancePolicySetting::Durable;
    s.temporal_rewrite = TemporalRewriteSetting::Preserving;
    s.unknown_memory_type = UnknownMemoryTypeSetting::Other;
    s.event_update_policy = EventUpdatePolicySetting::ChangeMarker;
    s.classification_skip_when_no_related = true;
    s.dedup_value_guard = if value_guard {
        DedupValueGuardSetting::On
    } else {
        DedupValueGuardSetting::Off
    };
    (create_app(AppState::new(s)), server)
}

async fn write(app: &axum::Router, user: &str, id: &str, text: &str) -> Value {
    let (name, value) = authorization_header();
    let body = json!({ "user_id": user, "session_id": "s1",
        "messages": [{ "role": "user", "content": text, "message_id": id }], "metadata": {} });
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
    json["summary"].clone()
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

fn fact(
    content: &str,
    memory_type: &str,
    subject: &str,
    message: &str,
    time: Option<(&str, &str)>,
) -> Value {
    json!({
        "content": content, "memory_type": memory_type, "subject": subject,
        "event_time": time.map(|(day, text)| json!({ "start": day, "end": day, "granularity": "day", "text": text })),
        "source_message_ids": [message], "confidence": 1.0, "importance": 0.8
    })
}

/// The recorded rescheduling case: both extractions put the date only in the event time; the classifier said NoOp.
fn rescheduled_appointment() -> Arc<Mutex<Script>> {
    Arc::new(Mutex::new(Script {
        extractions: VecDeque::from([
            json!({ "facts": [fact("The user has a dentist appointment.", "event", "user", "m1", Some(("2025-03-03", "March 3")))] }),
            json!({ "facts": [fact("The user has a dentist appointment scheduled.", "event", "user", "m2", Some(("2024-03-10", "March 10")))] }),
        ]),
        operation: "NoOp".into(),
        offered: vec![],
    }))
}

#[tokio::test]
async fn a_rescheduled_date_is_not_lost_to_a_noop_with_the_value_guard() {
    let script = rescheduled_appointment();
    let (app, _server) = app(true, script.clone()).await;
    assert_eq!(
        write(&app, "u_n06", "m1", "My dentist appointment is on March 3.").await["added"],
        1
    );
    let second = write(
        &app,
        "u_n06",
        "m2",
        "The dentist moved my appointment to March 10.",
    )
    .await;
    assert_eq!(second["added"], 1, "the new date is kept: {second}");
    assert_eq!(second["discarded"]["no_op_decision"], 0, "{second}");
    assert_eq!(second["discarded"]["duplicate_embedding"], 0, "{second}");
    assert_eq!(current(&app, "u_n06").await.len(), 2);
    // The classifier saw the old appointment and said NoOp; the guard, not dedup, kept the new date.
    assert_eq!(script.lock().expect("script").offered, vec![1]);
}

#[tokio::test]
async fn without_the_value_guard_the_rescheduled_date_is_dropped() {
    // The behaviour behind the recorded miss, kept as a pin: the reschedule is treated as nothing new.
    let script = rescheduled_appointment();
    let (app, _server) = app(false, script.clone()).await;
    write(
        &app,
        "u_n06_off",
        "m1",
        "My dentist appointment is on March 3.",
    )
    .await;
    let second = write(
        &app,
        "u_n06_off",
        "m2",
        "The dentist moved my appointment to March 10.",
    )
    .await;
    assert_eq!(second["added"], 0, "{second}");
    assert_eq!(second["noop"], 1, "{second}");
}

#[tokio::test]
async fn the_noop_guard_applies_only_with_the_semantic_v23_matcher() {
    // The older `semantic` matcher keeps the classifier's NoOp: the fix is scoped to the A3+R1 configuration.
    let script = rescheduled_appointment();
    let (app, _server) = app_with(true, RelatedFactMatcher::Semantic, script.clone()).await;
    write(
        &app,
        "u_n06_semantic",
        "m1",
        "My dentist appointment is on March 3.",
    )
    .await;
    let second = write(
        &app,
        "u_n06_semantic",
        "m2",
        "The dentist moved my appointment to March 10.",
    )
    .await;
    assert_eq!(second["discarded"]["no_op_decision"], 1, "{second}");
    assert_eq!(second["added"], 0, "{second}");
}

#[tokio::test]
async fn a_real_repeat_is_still_a_noop_with_the_value_guard() {
    // The guard keeps only candidates with a value the offered facts lack: the same date restated is still dropped.
    let script = Arc::new(Mutex::new(Script {
        extractions: VecDeque::from([
            json!({ "facts": [fact("The user has a dentist appointment.", "event", "user", "m1", Some(("2025-03-03", "March 3")))] }),
            json!({ "facts": [fact("The user has a dentist appointment scheduled.", "event", "user", "m2", Some(("2025-03-03", "March 3")))] }),
        ]),
        operation: "NoOp".into(),
        offered: vec![],
    }));
    let (app, _server) = app(true, script.clone()).await;
    write(
        &app,
        "u_repeat",
        "m1",
        "My dentist appointment is on March 3.",
    )
    .await;
    let second = write(
        &app,
        "u_repeat",
        "m2",
        "Reminder: my dentist appointment is on March 3.",
    )
    .await;
    assert_eq!(second["added"], 0, "{second}");
    assert_eq!(current(&app, "u_repeat").await.len(), 1);
}

/// One recorded two-write case; returns how many facts each classification call was offered.
async fn offered_for(first: (&str, Value), second: (&str, Value)) -> Vec<usize> {
    let script = Arc::new(Mutex::new(Script {
        extractions: VecDeque::from([first.1, second.1]),
        operation: "Update".into(),
        offered: vec![],
    }));
    let (app, _server) = app(true, script.clone()).await;
    write(&app, "u_gap", "m1", first.0).await;
    write(&app, "u_gap", "m2", second.0).await;
    script.lock().expect("script").offered.clone()
}

#[tokio::test]
#[ignore = "known gap: the extraction gave the new manager fact the subject 'Tom Okafor' ('The speaker's new manager'), so the user's manager fact is never offered"]
async fn manager_change_is_offered_the_old_manager() {
    let offered = offered_for(
        ("My manager is Priya Raman.", json!({ "facts": [fact("The user's manager is Priya Raman.", "relationship", "user", "m1", None)] })),
        ("Priya left the company; my new manager is Tom Okafor.", json!({ "facts": [
            fact("Priya left the company.", "event", "Priya", "m2", None),
            fact("The speaker's new manager is Tom Okafor.", "relationship", "Tom Okafor", "m2", None)] })),
    )
    .await;
    assert!(offered.iter().any(|n| *n > 0), "{offered:?}");
}

#[tokio::test]
#[ignore = "known gap: subjects 'user's team' and 'The user' are different canonical subjects, so the 6-engineer fact is never offered"]
async fn team_size_change_is_offered_the_old_size() {
    let offered = offered_for(
        ("Our team has 6 engineers.", json!({ "facts": [fact("The user's team consists of 6 engineers.", "entity", "user's team", "m1", None)] })),
        ("We hired two more, so the team has 8 engineers now.", json!({ "facts": [fact("The user's team currently consists of 8 engineers.", "profile", "The user", "m2", None)] })),
    )
    .await;
    assert!(offered.iter().any(|n| *n > 0), "{offered:?}");
}

#[tokio::test]
#[ignore = "known gap: 'Rocket is a basset hound' has the subject 'Rocket'; the beagle fact's subject is 'user', so it is never offered"]
async fn breed_correction_is_offered_the_old_breed() {
    let offered = offered_for(
        ("My dog is a beagle named Rocket.", json!({ "facts": [fact("The user owns a beagle named Rocket.", "entity", "user", "m1", None)] })),
        ("Correction: Rocket is a basset hound, not a beagle.", json!({ "facts": [fact("Rocket is a Basset Hound.", "entity", "Rocket", "m2", None)] })),
    )
    .await;
    assert!(offered.iter().any(|n| *n > 0), "{offered:?}");
}
