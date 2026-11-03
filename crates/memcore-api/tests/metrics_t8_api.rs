//! Minimal observability. Stage timings, bounded labels (matched route templates, never
//! raw ids), in-flight / RSS / shed / provider-token metrics, and nothing tenant- or secret-bearing
//! in the scrape output. All tests share the process-wide recorder, so assertions use labels that
//! only one test touches.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::metrics::PrometheusProviderUsageRecorder;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use memcore_core::ProviderUsageCapability;
use memcore_providers::{InMemoryProviderUsageRecorder, ProviderCallStatus, ProviderUsageEvent};
use serde_json::json;
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_t8_private_tenant";
const USER: &str = "conv0_PrivateSpeakerName";
const SECRET_TEXT: &str = "Avery's private pottery wheel memo";

fn metrics_app() -> axum::Router {
    create_app(AppState::new(Settings {
        metrics_enabled: true,
        metrics_path: "/metrics".to_string(),
        metrics_require_auth: false,
        metrics_include_process: true,
        ..Settings::default()
    }))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, String) {
    let (auth_name, auth_value) = authorization_header();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", ORG)
        .header(auth_name, auth_value)
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn scrape(app: &axum::Router) -> String {
    let (status, body) = call(app, "GET", "/metrics", None).await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn stage_timings_and_operational_metrics_are_exposed_with_safe_labels() {
    let app = metrics_app();
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/memories",
        Some(json!({"user_id": USER, "messages": [{"role": "user", "content": SECRET_TEXT}], "metadata": {}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/memories/search",
        Some(json!({"user_id": USER, "query": SECRET_TEXT, "limit": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GET", &format!("/api/v1/users/{USER}/memories"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GET", &format!("/no/such/route/{USER}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let body = scrape(&app).await;

    // Search and ingest stage histograms (fixed stage names).
    for stage in [
        "fact_query_embed",
        "fact_vector_search",
        "fact_sql_fetch",
        "fact_merge",
    ] {
        assert!(
            body.contains(&format!(
                "memcore_stage_duration_seconds_bucket{{path=\"search\",stage=\"{stage}\""
            )),
            "missing search stage {stage}"
        );
    }
    for stage in ["extract", "fact_embed", "dedup_classify", "fact_write"] {
        assert!(
            body.contains(&format!(
                "memcore_stage_duration_seconds_bucket{{path=\"ingest\",stage=\"{stage}\""
            )),
            "missing ingest stage {stage}"
        );
    }

    // Route labels are matched templates; unknown routes are `unmatched`.
    assert!(
        body.contains("route=\"/api/v1/users/{user_id}/memories\""),
        "matched template label"
    );
    assert!(body.contains("route=\"unmatched\""), "unmatched label");

    // Operational metrics.
    assert!(body.contains("memcore_http_requests_in_flight{class=\"read\"}"));
    assert!(body.contains("memcore_process_resident_memory_bytes "));
    assert!(body.contains("memcore_requests_shed_total{class=\"read\",reason=\"capacity\"} 0"));

    // Nothing tenant-, content- or secret-bearing anywhere in the scrape.
    let (_, auth_value) = authorization_header();
    for forbidden in [
        USER,
        ORG,
        SECRET_TEXT,
        "pottery",
        auth_value.as_str(),
        "http://",
        "https://",
    ] {
        assert!(
            !body.contains(forbidden),
            "scrape output leaks `{forbidden}`"
        );
    }
}

fn usage_event(provider: &str, input: Option<u64>, output: Option<u64>) -> ProviderUsageEvent {
    ProviderUsageEvent {
        org_id: Some(ORG.to_string()),
        user_id: Some(USER.to_string()),
        provider_name: provider.to_string(),
        model_name: Some("vendor/secret-model-name-v9".to_string()),
        capability: ProviderUsageCapability::Llm,
        operation_name: "llm_extract_facts".to_string(),
        status: ProviderCallStatus::Success,
        input_tokens: input,
        output_tokens: output,
        retry_count: 2,
        fallback_used: false,
        circuit_blocked: false,
        timed_out: false,
        estimated_cost_usd: None,
        token_source: memcore_providers::usage::TokenSource::Estimated,
        usage_report: memcore_providers::usage::UsageReportStatus::NotReported,
    }
}

fn metric_value(body: &str, prefix: &str) -> Option<f64> {
    body.lines()
        .find(|line| line.starts_with(prefix))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse().ok())
}

#[tokio::test]
async fn provider_calls_are_counted_once_with_bounded_labels_and_no_token_metric() {
    let app = metrics_app();
    let recorder = PrometheusProviderUsageRecorder::wrap(InMemoryProviderUsageRecorder::new());
    // Provider kinds unique to this test keep the shared recorder's counters isolated. The events
    // carry token numbers, as memcore's character-count estimates do; none may be exported.
    recorder.record_request(usage_event("anthropic", Some(120), Some(30)));
    recorder.record_request(usage_event("anthropic", Some(80), Some(10)));
    recorder.record_request(usage_event("groq", None, None));

    let body = scrape(&app).await;
    assert_eq!(
        metric_value(
            &body,
            "memcore_provider_requests_total{provider=\"anthropic\",role=\"llm\",operation=\"llm_extract_facts\",status=\"success\"}"
        ),
        Some(2.0),
        "one count per recorded call"
    );
    assert_eq!(
        metric_value(&body, "memcore_provider_retries_total{provider=\"groq\"}"),
        Some(2.0)
    );
    assert!(
        !body.contains("token"),
        "no token metric: usage tokens are estimates, not billed usage"
    );
    assert!(
        !body.contains("secret-model-name"),
        "model ids are not labels"
    );
    assert!(!body.contains("model=\""), "no model label at all");
}
