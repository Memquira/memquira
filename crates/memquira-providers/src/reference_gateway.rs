//! Startup check that the configured OpenAI-compatible endpoint is the Bedrock reference gateway in
//! its validated configuration. One `GET <gateway>/ready`: the gateway answers
//! from its own configuration and never calls Bedrock, so this costs nothing and proves nothing about
//! live AWS reachability.

use std::time::Duration;

/// The proxy source label the reference gateway reports.
pub const REFERENCE_GATEWAY_SOURCE_SHA256: &str =
    "271034621a9a20ef7b2e6a7eec120ef29d12c36f9442fea819b8922f5ece015c";

/// `Ok(())` when `<base>/ready` (a trailing `/v1` is dropped) returns 200 with
/// `reference_configuration: true` and the expected source hash; otherwise the reason.
pub async fn check_reference_gateway(base_url: &str, timeout: Duration) -> Result<(), String> {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    let url = format!("{base}/ready");
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| format!("http client: {error}"))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("gateway /ready unreachable ({})", error.without_url()))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| format!("gateway /ready returned HTTP {status} without the gateway's JSON"))?;
    if !status.is_success() {
        return Err(format!("gateway /ready returned HTTP {status}"));
    }
    if body["source_proxy_sha256"].as_str() != Some(REFERENCE_GATEWAY_SOURCE_SHA256) {
        return Err("the endpoint is not the reference gateway (source hash differs)".to_string());
    }
    if body["reference_configuration"].as_bool() != Some(true) {
        return Err(format!(
            "the gateway is not in its reference configuration (differs in: {})",
            body["reference_mismatches"]
        ));
    }
    Ok(())
}
