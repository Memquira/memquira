//! The startup check that the endpoint is the reference gateway in its reference
//! configuration. Fake gateways only; the real check never calls Bedrock either.

use std::time::Duration;

use memcore_providers::reference_gateway::{
    REFERENCE_GATEWAY_SOURCE_SHA256, check_reference_gateway,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn gateway(status: u16, body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

fn ready(reference: bool, sha: &str) -> serde_json::Value {
    json!({"status": "ready", "source_proxy_sha256": sha, "reference_configuration": reference,
           "reference_mismatches": if reference { json!([]) } else { json!(["chat_model_id"]) }})
}

#[tokio::test]
async fn confirms_the_reference_gateway_with_or_without_v1() {
    let server = gateway(200, ready(true, REFERENCE_GATEWAY_SOURCE_SHA256)).await;
    let t = Duration::from_secs(2);
    assert_eq!(check_reference_gateway(&server.uri(), t).await, Ok(()));
    assert_eq!(
        check_reference_gateway(&format!("{}/v1", server.uri()), t).await,
        Ok(())
    );
    // Only GET /ready was called (no chat or embedding request).
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| r.url.path() == "/ready" && r.method.as_str() == "GET")
    );
}

#[tokio::test]
async fn refuses_anything_else() {
    let t = Duration::from_secs(2);
    let not_reference = gateway(200, ready(false, REFERENCE_GATEWAY_SOURCE_SHA256)).await;
    let error = check_reference_gateway(&not_reference.uri(), t)
        .await
        .unwrap_err();
    assert!(error.contains("chat_model_id"), "{error}");
    let other_source = gateway(200, ready(true, "0000")).await;
    assert!(
        check_reference_gateway(&other_source.uri(), t)
            .await
            .unwrap_err()
            .contains("source hash")
    );
    let capped = gateway(503, ready(true, REFERENCE_GATEWAY_SOURCE_SHA256)).await;
    assert!(
        check_reference_gateway(&capped.uri(), t)
            .await
            .unwrap_err()
            .contains("503")
    );
    let not_gateway = MockServer::start().await; // 404 with no JSON
    assert!(
        check_reference_gateway(&not_gateway.uri(), t)
            .await
            .is_err()
    );
    assert!(
        check_reference_gateway("http://127.0.0.1:9", t)
            .await
            .unwrap_err()
            .contains("unreachable")
    );
}
