//! `MEMCORE_LLM_JSON_MODE`: the default keeps the chat-completions JSON request body
//! exactly as before (`response_format: json_object`, schema in the prompt); `json_schema` sends
//! the same schema as a structured-output `response_format` and changes nothing else.

use memcore_core::{CandidateFact, MemoryType, TenantContext};
use memcore_providers::traits::LlmProvider;
use memcore_providers::{
    ChatJsonMode, FactClassificationInput, OpenAiClient, OpenAiLlmProvider, OpenAiLlmTransport,
};
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn classification_input() -> FactClassificationInput {
    FactClassificationInput {
        tenant: TenantContext::new("org_test", "user_test").expect("tenant"),
        candidate_fact: CandidateFact::new(
            "User likes Rust.",
            MemoryType::Preference,
            0.9,
            0.8,
            None,
            json!({}),
        )
        .expect("candidate"),
        existing_facts: vec![],
    }
}

async fn sent_body(mode: Option<ChatJsonMode>) -> Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": r#"{"operation":"Add","target_fact_id":null,"reason":null,"confidence":0.9}"#
                },
                "finish_reason": "stop"
            }]
        })))
        .mount(&server)
        .await;
    let client = OpenAiClient::new("test-key", server.uri()).expect("client");
    let mut provider = OpenAiLlmProvider::new(client, "compat-model")
        .with_transport(OpenAiLlmTransport::ChatCompletions);
    if let Some(mode) = mode {
        provider = provider.with_json_mode(mode);
    }
    provider
        .classify_fact_operation(classification_input())
        .await
        .expect("classification");
    let requests = server.received_requests().await.expect("requests");
    serde_json::from_slice(&requests[0].body).expect("json body")
}

#[tokio::test]
async fn default_json_mode_sends_the_unchanged_json_object_body() {
    let default = sent_body(None).await;
    let explicit = sent_body(Some(ChatJsonMode::JsonObject)).await;
    assert_eq!(default, explicit);
    assert_eq!(default["response_format"], json!({ "type": "json_object" }));
    let mut keys: Vec<&str> = default
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["max_tokens", "messages", "model", "response_format"],
        "no new fields in the default body"
    );
}

#[tokio::test]
async fn json_schema_mode_sends_the_prompt_schema_as_structured_output() {
    let default = sent_body(None).await;
    let schema = sent_body(Some(ChatJsonMode::JsonSchema)).await;
    let format = &schema["response_format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["strict"], true);
    assert!(format["json_schema"]["name"].as_str().is_some());
    // The structured-output schema is exactly the one the prompt shows.
    let system = default["messages"][0]["content"].as_str().unwrap();
    let prompt_schema: Value = serde_json::from_str(
        system
            .split_once("matches this JSON Schema:\n")
            .expect("schema in prompt")
            .1,
    )
    .unwrap();
    assert_eq!(format["json_schema"]["schema"], prompt_schema);
    // Nothing else changes.
    let strip = |mut body: Value| {
        body.as_object_mut().unwrap().remove("response_format");
        body
    };
    assert_eq!(strip(schema), strip(default));
}

/// `instruction` sends no `response_format`; the system prompt (which already
/// demands one JSON object and shows the schema) and every other field stay exactly the default's.
#[tokio::test]
async fn instruction_mode_omits_response_format_and_changes_nothing_else() {
    let default = sent_body(None).await;
    let instruction = sent_body(Some(ChatJsonMode::Instruction)).await;
    assert!(instruction.get("response_format").is_none());
    let system = instruction["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Return a single JSON object only that matches this JSON Schema:"));
    let mut stripped = default.clone();
    stripped.as_object_mut().unwrap().remove("response_format");
    assert_eq!(instruction, stripped);
}
