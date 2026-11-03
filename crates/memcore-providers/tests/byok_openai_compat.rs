//! Deployment-level BYOK against fake OpenAI-compatible servers (wiremock).
//! Invented keys only; no real provider is contacted.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use memcore_common::MemcoreError;
use memcore_core::ports::{EmbeddingProvider, FactExtractionInput, LlmProvider};
use memcore_core::{MemoryMessage, MessageRole, TenantContext};
use memcore_providers::usage::{TokenSource, UsageReportStatus};
use memcore_providers::{
    CircuitBreakerConfig, EmbeddingOptions, OpenAiClient, OpenAiEmbeddingProvider,
    OpenAiLlmProvider, ProviderCandidate, ProviderCapability, ProviderCircuitBreaker,
    ProviderExecutionPolicy, ProviderId, ProviderUsageEvent, ProviderUsageRecorder,
    ProviderUsageSnapshot, build_resilient_embedding_from_candidates, new_token_usage_slot,
};
use serde_json::json;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KEY_LLM: &str = "sk-test-llm-INVENTED-0001";
const KEY_EMB: &str = "sk-test-emb-INVENTED-0002";

fn embedding_body(dims: usize, usage: serde_json::Value) -> serde_json::Value {
    let mut body = json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": vec![0.5f32; dims]}],
        "model": "fake-embed",
    });
    if !usage.is_null() {
        body["usage"] = usage;
    }
    body
}

/// Captures every usage event (the router records one per logical call).
#[derive(Default)]
struct Events(Mutex<Vec<ProviderUsageEvent>>);
impl ProviderUsageRecorder for Events {
    fn record_request(&self, event: ProviderUsageEvent) {
        self.0.lock().unwrap().push(event);
    }
    fn snapshot(&self) -> ProviderUsageSnapshot {
        memcore_providers::InMemoryProviderUsageRecorder::default().snapshot()
    }
}

fn routed_embedding(
    provider: OpenAiEmbeddingProvider,
    policy: ProviderExecutionPolicy,
) -> (Arc<dyn EmbeddingProvider>, Arc<Events>) {
    let events = Arc::new(Events::default());
    let routed = build_resilient_embedding_from_candidates(
        vec![ProviderCandidate::new(
            ProviderId::new("openai", ProviderCapability::Embedding),
            Arc::new(provider) as Arc<dyn EmbeddingProvider>,
            Some("fake-embed".to_string()),
            Some(new_token_usage_slot()),
        )],
        Arc::new(ProviderCircuitBreaker::new(CircuitBreakerConfig::default())),
        policy,
        false,
        None,
        Some(events.clone() as Arc<dyn ProviderUsageRecorder>),
        None,
        true,
    )
    .expect("routed provider");
    (routed, events)
}

fn quick_policy() -> ProviderExecutionPolicy {
    ProviderExecutionPolicy {
        timeout: Duration::from_secs(5),
        max_retries: 2,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
        jitter_enabled: false,
        ..ProviderExecutionPolicy::default()
    }
}

// ---- C. independent endpoints, keys and key-free servers ---------------------------------------

#[tokio::test]
async fn llm_and_embedding_use_their_own_endpoint_and_key() {
    let llm_server = MockServer::start().await;
    let emb_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", format!("Bearer {KEY_LLM}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "{\"facts\":[]}"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14}
        })))
        .expect(1)
        .mount(&llm_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .and(header(
            "authorization",
            format!("Bearer {KEY_EMB}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(
            4,
            json!({"prompt_tokens": 2, "total_tokens": 2}),
        )))
        .expect(1)
        .mount(&emb_server)
        .await;
    // Nothing may reach the wrong server.
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&llm_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&emb_server)
        .await;

    let llm = OpenAiLlmProvider::for_base_url(
        OpenAiClient::with_optional_key(Some(KEY_LLM.into()), llm_server.uri()).unwrap(),
        "fake-chat-model".to_string(),
    );
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(Some(KEY_EMB.into()), emb_server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    llm.extract_facts(FactExtractionInput {
        tenant: TenantContext::new("org_byok", "user_byok").unwrap(),
        messages: vec![MemoryMessage {
            role: MessageRole::User,
            content: "An invented sentence.".to_string(),
            ..Default::default()
        }],
        metadata: json!({}),
        mode: Default::default(),
    })
    .await
    .expect("extraction via the LLM endpoint");
    emb.embed_text("an invented text")
        .await
        .expect("embedding via its own endpoint");
    // `expect(..)` counts are verified when the servers drop.
}

#[tokio::test]
async fn key_free_endpoint_sends_no_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(401))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(4, json!(null))))
        .expect(1)
        .mount(&server)
        .await;
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(None, server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    emb.embed_text("no key needed")
        .await
        .expect("key-free server");
}

// ---- D. redaction ------------------------------------------------------------------------------

#[tokio::test]
async fn provider_errors_never_carry_the_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": format!("bad request for key {KEY_EMB} and model fake-embed")}
        })))
        .mount(&server)
        .await;
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(Some(KEY_EMB.into()), server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    let error = emb.embed_text("x").await.expect_err("400");
    let text = format!("{error} {error:?}");
    assert!(!text.contains(KEY_EMB), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
    let client = OpenAiClient::with_optional_key(Some(KEY_EMB.into()), server.uri()).unwrap();
    assert!(!format!("{client:?}").contains(KEY_EMB));
}

// ---- F. reported usage -------------------------------------------------------------------------

async fn usage_case(usage: serde_json::Value) -> ProviderUsageEvent {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_body(4, usage)))
        .mount(&server)
        .await;
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(Some(KEY_EMB.into()), server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    let (routed, events) = routed_embedding(emb, quick_policy());
    routed
        .embed_text("an invented sentence of some length")
        .await
        .unwrap();
    let events = events.0.lock().unwrap();
    assert_eq!(
        events.len(),
        1,
        "one usage event per logical call (no double counting)"
    );
    events[0].clone()
}

#[tokio::test]
async fn reported_missing_partial_and_malformed_usage_are_labelled() {
    let reported = usage_case(json!({"prompt_tokens": 9, "total_tokens": 9})).await;
    assert_eq!(
        (
            reported.input_tokens,
            reported.output_tokens,
            reported.token_source,
            reported.usage_report
        ),
        (
            Some(9),
            None,
            TokenSource::Reported,
            UsageReportStatus::Complete
        )
    );
    let missing = usage_case(json!(null)).await;
    assert_eq!(
        missing.token_source,
        TokenSource::Estimated,
        "estimate, labelled as such"
    );
    assert_eq!(missing.usage_report, UsageReportStatus::NotReported);
    assert!(missing.input_tokens.is_some());
    let partial = usage_case(json!({"total_tokens": 9})).await;
    assert_eq!(
        (
            partial.input_tokens,
            partial.token_source,
            partial.usage_report
        ),
        (
            None,
            TokenSource::PartiallyReported,
            UsageReportStatus::Partial
        ),
        "a partial report is not filled in from the estimate"
    );
    let malformed = usage_case(json!({"prompt_tokens": "nine"})).await;
    assert_eq!(malformed.usage_report, UsageReportStatus::Malformed);
    assert_eq!(malformed.token_source, TokenSource::Estimated);
}

/// Answers 429 (no usage) first, then 200 with usage.
struct ThrottleThenOk {
    calls: Arc<Mutex<Vec<Instant>>>,
    first: ResponseTemplate,
    then: ResponseTemplate,
}
impl Respond for ThrottleThenOk {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut calls = self.calls.lock().unwrap();
        calls.push(Instant::now());
        if calls.len() == 1 {
            self.first.clone()
        } else {
            self.then.clone()
        }
    }
}

#[tokio::test]
async fn retried_call_records_one_event_with_the_successful_attempts_usage() {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ThrottleThenOk {
            calls: calls.clone(),
            first: ResponseTemplate::new(429),
            then: ResponseTemplate::new(200).set_body_json(embedding_body(
                4,
                json!({"prompt_tokens": 5, "total_tokens": 5}),
            )),
        })
        .mount(&server)
        .await;
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(Some(KEY_EMB.into()), server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    let (routed, events) = routed_embedding(emb, quick_policy());
    routed
        .embed_text("retry me")
        .await
        .expect("succeeds on the retry");
    assert_eq!(calls.lock().unwrap().len(), 2, "two HTTP attempts");
    let events = events.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].retry_count, 1);
    assert_eq!(
        events[0].input_tokens,
        Some(5),
        "the 429 reported nothing; not double counted"
    );
    assert_eq!(events[0].token_source, TokenSource::Reported);
}

// ---- G. Retry-After ----------------------------------------------------------------------------

async fn throttled_server(first: ResponseTemplate) -> (MockServer, Arc<Mutex<Vec<Instant>>>) {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ThrottleThenOk {
            calls: calls.clone(),
            first,
            then: ResponseTemplate::new(200).set_body_json(embedding_body(4, json!(null))),
        })
        .mount(&server)
        .await;
    (server, calls)
}

fn routed_for(server: &MockServer) -> Arc<dyn EmbeddingProvider> {
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(Some(KEY_EMB.into()), server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    routed_embedding(emb, quick_policy()).0
}

#[tokio::test]
async fn retry_after_seconds_is_honoured() {
    let (server, calls) =
        throttled_server(ResponseTemplate::new(429).insert_header("retry-after", "1")).await;
    routed_for(&server).embed_text("x").await.expect("retried");
    let calls = calls.lock().unwrap();
    let waited = calls[1] - calls[0];
    assert!(
        waited >= Duration::from_millis(950),
        "waited {waited:?}, Retry-After was 1 s"
    );
}

#[tokio::test]
async fn retry_after_on_503_and_as_http_date_is_honoured() {
    let when = std::time::SystemTime::now() + Duration::from_secs(2);
    let (server, calls) = throttled_server(
        ResponseTemplate::new(503)
            .insert_header("retry-after", httpdate::fmt_http_date(when).as_str()),
    )
    .await;
    routed_for(&server).embed_text("x").await.expect("retried");
    let calls = calls.lock().unwrap();
    assert!(
        calls[1] - calls[0] >= Duration::from_millis(900),
        "{:?}",
        calls[1] - calls[0]
    );
}

#[tokio::test]
async fn invalid_retry_after_falls_back_to_backoff() {
    let (server, calls) =
        throttled_server(ResponseTemplate::new(429).insert_header("retry-after", "soon-ish")).await;
    routed_for(&server).embed_text("x").await.expect("retried");
    let calls = calls.lock().unwrap();
    assert!(
        calls[1] - calls[0] < Duration::from_millis(500),
        "backoff (10 ms), not a long wait"
    );
}

#[tokio::test]
async fn retry_after_beyond_the_cap_stops_and_keeps_the_throttled_error() {
    let (server, calls) =
        throttled_server(ResponseTemplate::new(429).insert_header("retry-after", "120")).await;
    let started = Instant::now();
    let error = routed_for(&server)
        .embed_text("x")
        .await
        .expect_err("not retried");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "did not wait 120 s"
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        matches!(
            error,
            MemcoreError::ProviderThrottled {
                status: 429,
                retry_after_ms: Some(120_000),
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn retry_after_past_the_request_deadline_stops_immediately() {
    let (server, calls) =
        throttled_server(ResponseTemplate::new(429).insert_header("retry-after", "2")).await;
    let routed = routed_for(&server);
    let started = Instant::now();
    let error = memcore_core::deadline::with_request_deadline(
        Instant::now() + Duration::from_millis(500),
        routed.embed_text("x"),
    )
    .await
    .expect_err("would pass the deadline");
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        matches!(error, MemcoreError::ProviderThrottled { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn throttling_without_retry_after_uses_bounded_backoff_and_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(429))
        .expect(3) // 1 + max_retries 2
        .mount(&server)
        .await;
    let error = routed_for(&server)
        .embed_text("x")
        .await
        .expect_err("exhausted");
    assert!(
        matches!(
            error,
            MemcoreError::ProviderThrottled {
                status: 429,
                retry_after_ms: None,
                ..
            }
        ),
        "{error:?}"
    );
}

// ---- E. embedding options & identity -----------------------------------------------------------

#[tokio::test]
async fn prefixes_normalisation_and_dimensions_field_follow_the_options() {
    let server = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    struct Capture(Arc<Mutex<Vec<serde_json::Value>>>);
    impl Respond for Capture {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&request.body).unwrap());
            ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"index": 0, "embedding": [3.0, 4.0, 0.0, 0.0]}]
            }))
        }
    }
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(Capture(bodies.clone()))
        .mount(&server)
        .await;
    let options = EmbeddingOptions {
        send_dimensions: false,
        normalize: true,
        query_prefix: Some("query: ".into()),
        document_prefix: Some("passage: ".into()),
        revision: Some("rev-1".into()),
        serving_variant: Some("int8".into()),
        dedup_policy: Some("exact_text_only_uncalibrated".into()),
    };
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(None, server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap()
    .with_options(options);
    let doc = emb.embed_text("stored text").await.unwrap();
    let query = emb.embed_query("a question").await.unwrap();
    assert!(
        (doc[0] - 0.6).abs() < 1e-6 && (doc[1] - 0.8).abs() < 1e-6,
        "L2-normalised: {doc:?}"
    );
    assert_eq!(query.len(), 4);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["input"][0], "passage: stored text");
    assert_eq!(bodies[1]["input"][0], "query: a question");
    assert!(
        bodies[0].get("dimensions").is_none(),
        "dimensions not sent when disabled"
    );

    let identity = emb.embedding_identity().unwrap();
    assert_eq!(identity.normalisation.as_deref(), Some("l2_by_memcore"));
    assert_eq!(identity.document_prefix.as_deref(), Some("passage: "));
    assert!(identity.query_instruction_sha256.is_some());
    assert_eq!(identity.serving_variant.as_deref(), Some("int8"));
    assert_eq!(
        identity.dedup_policy.as_deref(),
        Some("exact_text_only_uncalibrated")
    );

    // Every option that changes vectors (or the dedup policy) changes the identity hash.
    let base = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(None, server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    let base_hash = base.embedding_identity().unwrap().semantic_hash();
    assert_ne!(base_hash, identity.semantic_hash());
    // The reference (no options) identity hash is unchanged by the new optional fields.
    assert_eq!(
        base_hash,
        memcore_core::EmbeddingIdentity::remote("openai_compatible", "fake-embed", 4)
            .semantic_hash()
    );
}

#[tokio::test]
async fn non_finite_vectors_are_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"data":[{"index":0,"embedding":[1.0, 1e39, 0.0, 0.0]}]}"#),
        )
        .mount(&server)
        .await;
    let emb = OpenAiEmbeddingProvider::new(
        OpenAiClient::with_optional_key(None, server.uri()).unwrap(),
        "fake-embed",
        4,
    )
    .unwrap();
    let error = emb.embed_text("x").await.expect_err("inf refused");
    assert!(format!("{error}").contains("non-finite"), "{error}");
}

#[test]
fn retry_after_parser_bounds() {
    use memcore_providers::openai::parse_retry_after_ms;
    assert_eq!(parse_retry_after_ms("3"), Some(3_000));
    assert_eq!(parse_retry_after_ms(" 0 "), Some(0));
    assert_eq!(parse_retry_after_ms("-1"), None);
    assert_eq!(parse_retry_after_ms("abc"), None);
    assert_eq!(
        parse_retry_after_ms("999999999"),
        None,
        "absurd waits ignored"
    );
}
