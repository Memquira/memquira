//! Provider calls made while serving a request carry that request's operation token (`X-Memquira-Operation`), for a
//! provider-side proxy that charges each call to the caller's operation: only on clients whose host is allowed,
//! including every retry and every embedding call; never outside a scoped request; never to another provider URL.

use std::sync::Arc;
use std::time::Duration;

use memquira_core::{CandidateFact, MemoryType, TenantContext};
use memquira_providers::operation::{OPERATION_HEADER, with_operation_token};
use memquira_providers::traits::{EmbeddingProvider, LlmProvider};
use memquira_providers::{
    FactClassificationInput, OpenAiClient, OpenAiEmbeddingProvider, OpenAiLlmProvider,
    OpenAiLlmTransport, ProviderExecutionPolicy, wrap_llm_provider,
};
use serde_json::json;
use wiremock::matchers::{method, path};
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

fn chat_ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{ "message": { "role": "assistant", "content": r#"{"operation":"Add","target_fact_id":null,"reason":"new","confidence":0.9}"# }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    }))
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(chat_ok())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": [0.6, 0.8], "index": 0 }],
            "usage": { "prompt_tokens": 3, "total_tokens": 3 }
        })))
        .mount(&server)
        .await;
    server
}

/// The mock server's `host:port`, the allow-list entry for it.
fn host_of(server: &MockServer) -> Vec<String> {
    vec![server.uri().trim_start_matches("http://").to_string()]
}

fn client(server: &MockServer, allowed: &[String]) -> OpenAiClient {
    OpenAiClient::new("k", server.uri())
        .expect("client")
        .with_operation_forwarding(true, allowed)
}

fn chat(client: OpenAiClient) -> OpenAiLlmProvider {
    OpenAiLlmProvider::new(client, "m").with_transport(OpenAiLlmTransport::ChatCompletions)
}

async fn headers(server: &MockServer) -> Vec<Option<String>> {
    server
        .received_requests()
        .await
        .expect("recording")
        .iter()
        .map(|r| {
            r.headers
                .get(OPERATION_HEADER)
                .map(|v| v.to_str().expect("ascii").to_string())
        })
        .collect()
}

#[tokio::test]
async fn chat_and_embedding_calls_carry_the_scoped_token_and_only_inside_the_scope() {
    let server = server().await;
    let allowed = host_of(&server);
    let llm = chat(client(&server, &allowed));
    let embed = OpenAiEmbeddingProvider::new(client(&server, &allowed), "e", 2).expect("provider");

    llm.classify_fact_operation(classification_input())
        .await
        .expect("unscoped call");
    with_operation_token(Some("v1.op-a.1763800000.sigA".to_string()), async {
        llm.classify_fact_operation(classification_input())
            .await
            .expect("scoped chat");
        embed.embed_text("hello").await.expect("scoped embedding");
    })
    .await;
    with_operation_token(Some("v1.op-b.1763800000.sigB".to_string()), async {
        llm.classify_fact_operation(classification_input())
            .await
            .expect("other operation");
    })
    .await;

    assert_eq!(
        headers(&server).await,
        vec![
            None,
            Some("v1.op-a.1763800000.sigA".to_string()),
            Some("v1.op-a.1763800000.sigA".to_string()),
            Some("v1.op-b.1763800000.sigB".to_string()),
        ]
    );
}

#[tokio::test]
async fn a_provider_whose_host_is_not_allowed_never_receives_the_token() {
    let server = server().await;
    for allowed in [
        vec![],
        vec!["vertex-adapter:8790".to_string()],
        vec!["127.0.0.1:1".to_string()],
    ] {
        let llm = chat(client(&server, &allowed));
        with_operation_token(Some("v1.op.1.sig".to_string()), async {
            llm.classify_fact_operation(classification_input())
                .await
                .expect("call");
        })
        .await;
    }
    // Forwarding disabled for an allowed host: nothing either.
    let off = chat(
        OpenAiClient::new("k", server.uri())
            .expect("client")
            .with_operation_forwarding(false, &host_of(&server)),
    );
    with_operation_token(Some("v1.op.1.sig".to_string()), async {
        off.classify_fact_operation(classification_input())
            .await
            .expect("call");
    })
    .await;
    assert_eq!(headers(&server).await, vec![None, None, None, None]);
}

#[tokio::test]
async fn retried_calls_keep_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(chat_ok())
        .mount(&server)
        .await;
    let inner: Arc<dyn LlmProvider> = Arc::new(chat(client(&server, &host_of(&server))));
    let wrapped = wrap_llm_provider(
        inner,
        ProviderExecutionPolicy {
            timeout: Duration::from_secs(5),
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
            backoff_multiplier: 1.0,
            jitter_enabled: false,
        },
    );
    with_operation_token(Some("v1.op-r.1.sig".to_string()), async {
        wrapped
            .classify_fact_operation(classification_input())
            .await
            .expect("succeeds on the third attempt");
    })
    .await;
    let seen = headers(&server).await;
    assert_eq!(seen.len(), 3, "two retries");
    assert!(
        seen.iter().all(|h| h.as_deref() == Some("v1.op-r.1.sig")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn concurrent_operations_never_see_each_others_token() {
    let server = server().await;
    let llm = Arc::new(chat(client(&server, &host_of(&server))));
    let mut tasks = Vec::new();
    for i in 0..16 {
        let llm = llm.clone();
        tasks.push(tokio::spawn(with_operation_token(
            Some(format!("v1.op-{i}.1.sig")),
            async move {
                for _ in 0..3 {
                    llm.classify_fact_operation(classification_input())
                        .await
                        .expect("call");
                    tokio::task::yield_now().await;
                }
            },
        )));
    }
    for t in tasks {
        t.await.expect("join");
    }
    let mut counts = std::collections::HashMap::new();
    for h in headers(&server).await {
        *counts
            .entry(h.expect("every call carries a token"))
            .or_insert(0) += 1;
    }
    assert_eq!(counts.len(), 16);
    assert!(counts.values().all(|n| *n == 3), "{counts:?}");
}

#[tokio::test]
async fn a_forwarding_client_never_follows_a_redirect_with_the_token() {
    // reqwest keeps custom headers across redirects, so a redirect from the allowed host could carry the token to
    // another one. A forwarding client follows no redirect; a client that does not forward is unchanged.
    let elsewhere = server().await;
    let redirecting = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/chat/completions", elsewhere.uri())),
        )
        .mount(&redirecting)
        .await;

    let forwarding = chat(client(&redirecting, &host_of(&redirecting)));
    let result = with_operation_token(Some("v1.op-x.1.sig".to_string()), async {
        forwarding
            .classify_fact_operation(classification_input())
            .await
    })
    .await;
    assert!(result.is_err(), "a redirect is not followed");
    assert!(
        elsewhere
            .received_requests()
            .await
            .expect("recording")
            .is_empty()
    );

    let plain = chat(OpenAiClient::new("k", redirecting.uri()).expect("client"));
    plain
        .classify_fact_operation(classification_input())
        .await
        .expect("the default client still follows the redirect");
    assert_eq!(headers(&elsewhere).await, vec![None]);
}
