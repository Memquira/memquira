//! Output-token limits and truncation handling for the OpenAI-compatible LLM provider.
//!
//! Extraction used to send a hardcoded `max_tokens: 300`, ignoring configuration. That cut off
//! a share of extraction responses mid-JSON; each surfaced as a generic JSON
//! parse failure, was retried three times against a cache that replayed the same truncated reply,
//! and lost every fact in its batch. These tests pin the replacement behaviour: the configured
//! limit is sent, extraction and classification are limited independently, and a truncated
//! response is a distinct, non-retryable error.

use std::sync::Arc;
use std::time::Duration;

use memcore_common::MemcoreError;
use memcore_core::{CandidateFact, MemoryType, TenantContext};
use memcore_providers::traits::LlmProvider;
use memcore_providers::{
    FactClassificationInput, FactExtractionInput, MemoryMessage, MessageRole, OpenAiClient,
    OpenAiLlmProvider, OpenAiLlmTransport, OutputTokenLimits, ProviderExecutionPolicy,
    is_provider_health_failure, is_retryable_provider_error, wrap_llm_provider,
};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ONE_FACT: &str = r#"{"facts":[{"content":"User likes Rust.","memory_type":"Preference","confidence":0.9,"importance":0.8,"valid_at":null,"metadata":{}}]}"#;

fn tenant() -> TenantContext {
    TenantContext::new("org_test", "user_test").expect("tenant")
}

fn extraction_input() -> FactExtractionInput {
    FactExtractionInput {
        tenant: tenant(),
        messages: vec![MemoryMessage {
            role: MessageRole::User,
            content: "I like Rust.".to_string(),
            ..Default::default()
        }],
        metadata: json!({}),
        mode: Default::default(),
    }
}

fn classification_input() -> FactClassificationInput {
    FactClassificationInput {
        tenant: tenant(),
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

fn chat_reply(content: &str, finish_reason: &str) -> serde_json::Value {
    json!({
        "choices": [{
            "message": { "role": "assistant", "content": content },
            "finish_reason": finish_reason
        }]
    })
}

fn chat_provider(server: &MockServer, limits: OutputTokenLimits) -> OpenAiLlmProvider {
    let client = OpenAiClient::new("test-key", server.uri()).expect("client");
    OpenAiLlmProvider::new(client, "compat-model")
        .with_transport(OpenAiLlmTransport::ChatCompletions)
        .with_output_token_limits(limits)
}

#[tokio::test]
async fn extraction_sends_the_configured_limit_not_a_hardcoded_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({ "max_tokens": 1234 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_reply(ONE_FACT, "stop")))
        .expect(1)
        .mount(&server)
        .await;

    let provider = chat_provider(
        &server,
        OutputTokenLimits {
            extraction: 1234,
            classification: 77,
        },
    );
    let output = provider
        .extract_facts(extraction_input())
        .await
        .expect("a request carrying the configured limit should be matched");
    assert_eq!(output.candidates.len(), 1);
}

#[tokio::test]
async fn classification_uses_its_own_limit_independently_of_extraction() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({ "max_tokens": 77 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_reply(
            r#"{"operation":"Add","target_fact_id":null,"reason":"new","confidence":0.9}"#,
            "stop",
        )))
        .expect(1)
        .mount(&server)
        .await;

    let provider = chat_provider(
        &server,
        OutputTokenLimits {
            extraction: 1234,
            classification: 77,
        },
    );
    provider
        .classify_fact_operation(classification_input())
        .await
        .expect("classification should send its own, smaller limit");
}

#[test]
fn default_limits_give_extraction_room_for_a_full_fact_list() {
    let limits = OutputTokenLimits::default();
    assert_eq!(limits.extraction, 2_048);
    assert_eq!(limits.classification, 300);
}

#[tokio::test]
async fn a_length_finish_reason_is_reported_as_truncation_not_as_bad_json() {
    let server = MockServer::start().await;
    // What a cut-off extraction actually looks like: valid JSON up to the point the budget ran out.
    let cut_off = r#"{"facts":[{"content":"User likes Rust.","memory_type":"Preference","confidence":0.9,"imp"#;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_reply(cut_off, "length")))
        .mount(&server)
        .await;

    let provider = chat_provider(&server, OutputTokenLimits::default());
    let error = provider
        .extract_facts(extraction_input())
        .await
        .expect_err("a truncated response must fail");

    assert_eq!(
        error,
        MemcoreError::ProviderOutputTruncated {
            operation: "extract_facts",
            max_output_tokens: Some(2_048),
        }
    );
    assert_eq!(error.code(), "provider_output_truncated");
}

#[tokio::test]
async fn a_truncated_classification_names_its_own_operation_and_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_reply(r#"{"operation":"Ad"#, "length")),
        )
        .mount(&server)
        .await;

    let provider = chat_provider(&server, OutputTokenLimits::default());
    let error = provider
        .classify_fact_operation(classification_input())
        .await
        .expect_err("a truncated response must fail");

    assert_eq!(
        error,
        MemcoreError::ProviderOutputTruncated {
            operation: "classify_fact_operation",
            max_output_tokens: Some(300),
        }
    );
}

#[tokio::test]
async fn an_incomplete_responses_api_reply_is_reported_as_truncation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [{
                "type": "message",
                "content": [{ "type": "output_text", "text": "{\"facts\":[{\"content\":\"User" }]
            }]
        })))
        .mount(&server)
        .await;

    let client = OpenAiClient::new("test-key", server.uri()).expect("client");
    let provider = OpenAiLlmProvider::new(client, "gpt-test");
    let error = provider
        .extract_facts(extraction_input())
        .await
        .expect_err("an incomplete response must fail");

    assert_eq!(
        error,
        MemcoreError::ProviderOutputTruncated {
            operation: "extract_facts",
            // The Responses transport has never sent an output limit, so none is reported.
            max_output_tokens: None,
        }
    );
}

#[test]
fn truncation_is_not_retryable_and_does_not_trip_the_circuit_breaker() {
    let error = MemcoreError::provider_output_truncated("extract_facts", Some(2_048));
    assert!(
        !is_retryable_provider_error(&error),
        "the same request with the same limit reproduces the same truncated output"
    );
    assert!(
        !is_provider_health_failure(&error),
        "a truncation is a configuration problem, not a provider outage"
    );
}

/// End to end through the real retry wrapper: the policy allows two retries, and a truncated
/// response must still be requested exactly once.
#[tokio::test]
async fn the_retry_policy_sends_a_truncated_request_exactly_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_reply(r#"{"facts":[{"con"#, "length")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let inner: Arc<dyn LlmProvider> =
        Arc::new(chat_provider(&server, OutputTokenLimits::default()));
    let policy = ProviderExecutionPolicy {
        timeout: Duration::from_secs(5),
        max_retries: 2,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(2),
        backoff_multiplier: 1.0,
        jitter_enabled: false,
    };
    let wrapped = wrap_llm_provider(inner, policy);

    let error = wrapped
        .extract_facts(extraction_input())
        .await
        .expect_err("truncation must surface");
    assert!(error.is_provider_output_truncated(), "got {error:?}");
    // `.expect(1)` on the mock is verified when `server` drops.
}

#[tokio::test]
async fn a_normal_stop_is_unaffected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_reply(ONE_FACT, "stop")))
        .mount(&server)
        .await;

    let output = chat_provider(&server, OutputTokenLimits::default())
        .extract_facts(extraction_input())
        .await
        .expect("a complete response must parse as before");
    assert_eq!(output.candidates.len(), 1);
    assert_eq!(output.skipped.total(), 0);
}
