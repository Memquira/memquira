//! The OpenAI-compatible provider sends exactly the
//! frozen `decompose-v1` request, other operations still send no sampling settings, and a truncated
//! reply is an error rather than partial output.

use memcore_common::MemcoreError;
use memcore_core::candidates::decomposition::{
    MAX_OUTPUT_TOKENS, SYSTEM_PROMPT, TEMPERATURE, TOP_P, USER_TEMPLATE,
};
use memcore_core::ports::QueryDecompositionInput;
use memcore_providers::traits::LlmProvider;
use memcore_providers::{OpenAiClient, OpenAiLlmProvider, OpenAiLlmTransport};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const QUESTION: &str = "Did Rowena or Dunstan bake more for the harvest festival?";

fn reply(content: &str, finish_reason: &str) -> Value {
    json!({
        "choices": [{
            "message": { "role": "assistant", "content": content },
            "finish_reason": finish_reason
        }]
    })
}

fn input() -> QueryDecompositionInput {
    QueryDecompositionInput {
        system: SYSTEM_PROMPT.to_string(),
        user: USER_TEMPLATE.replace("{question}", QUESTION),
        max_output_tokens: MAX_OUTPUT_TOKENS,
        temperature: TEMPERATURE,
        top_p: TOP_P,
    }
}

fn provider(server: &MockServer, transport: OpenAiLlmTransport) -> OpenAiLlmProvider {
    let client = OpenAiClient::new("test-key", server.uri()).expect("client");
    OpenAiLlmProvider::new(client, "bedrock-nova-lite").with_transport(transport)
}

#[tokio::test]
async fn the_exact_frozen_request_is_sent_once_and_the_raw_text_returned() {
    let server = MockServer::start().await;
    let text = r#"{"sub_queries": ["What did Rowena bake?"]}"#;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply(text, "stop")))
        .expect(1)
        .mount(&server)
        .await;
    let got = provider(&server, OpenAiLlmTransport::ChatCompletions)
        .decompose_query(input())
        .await
        .unwrap();
    assert_eq!(got, text);
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body,
        json!({
            "model": "bedrock-nova-lite",
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": format!("Question: {QUESTION}") },
            ],
            "response_format": { "type": "json_object" },
            "max_tokens": 150,
            "temperature": 0.0,
            "top_p": 1.0,
        })
    );
}

#[tokio::test]
async fn a_truncated_reply_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(reply(r#"{"sub_queries": ["Wha"#, "length")),
        )
        .mount(&server)
        .await;
    let error = provider(&server, OpenAiLlmTransport::ChatCompletions)
        .decompose_query(input())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        MemcoreError::ProviderOutputTruncated {
            operation: "decompose_query",
            ..
        }
    ));
}

#[tokio::test]
async fn the_responses_transport_is_refused_without_a_call() {
    let server = MockServer::start().await;
    let error = provider(&server, OpenAiLlmTransport::Responses)
        .decompose_query(input())
        .await
        .unwrap_err();
    assert!(matches!(error, MemcoreError::ProviderError(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn other_chat_operations_still_send_no_sampling_settings() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply("A short summary.", "stop")))
        .mount(&server)
        .await;
    provider(&server, OpenAiLlmTransport::ChatCompletions)
        .summarize_memory(memcore_core::ports::SummarizationInput {
            tenant: memcore_core::TenantContext::new("org_test", "user_test").unwrap(),
            facts: vec![],
            max_tokens: None,
        })
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body.get("temperature").is_none() && body.get("top_p").is_none());
}
