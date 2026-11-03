//! `/health/live` and `/health/ready`: liveness, readiness, provider mode, draining.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{Settings, VectorBackend};
use tower::ServiceExt;

async fn get(state: AppState, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = create_app(state)
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&body).expect("response should be valid json"),
    )
}

fn mock_allowed() -> Settings {
    Settings {
        allow_mock: true,
        ..Settings::default()
    }
}

#[tokio::test]
async fn live_is_200_without_touching_dependencies() {
    let (status, json) = get(AppState::new(Settings::default()), "/health/live").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
    assert_eq!(json["service"], "memcore");
}

#[tokio::test]
async fn ready_is_200_with_explicitly_allowed_mock_providers() {
    let (status, json) = get(AppState::new(mock_allowed()), "/health/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ready");
    assert_eq!(json["draining"], false);
    assert_eq!(json["checks"]["database"]["connected"], true);
    assert_eq!(json["checks"]["vector_store"]["backend"], "mock");
    assert_eq!(
        json["checks"]["vector_store"]["reachable"],
        serde_json::Value::Null
    );
    assert_eq!(json["checks"]["providers"]["mode"], "mock_allowed");
    assert_eq!(json["checks"]["providers"]["llm_provider"], "mock");
}

#[tokio::test]
async fn ready_is_503_when_mock_providers_are_not_allowed() {
    let (status, json) = get(AppState::new(Settings::default()), "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["status"], "not_ready");
    assert_eq!(json["checks"]["providers"]["ready"], false);
    assert_eq!(json["checks"]["providers"]["mode"], "refused");
}

#[tokio::test]
async fn ready_is_503_and_draining_once_shutdown_starts() {
    let state = AppState::new(mock_allowed());
    state.shutdown_token.cancel();
    let (status, json) = get(state, "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["status"], "draining");
    assert_eq!(json["draining"], true);
}

#[tokio::test]
async fn ready_is_503_when_qdrant_is_unreachable_and_hides_the_url() {
    let mut state = AppState::new(mock_allowed());
    // Readiness only: the stores stay mock; the probe targets a closed loopback port.
    state.settings.vector_backend = VectorBackend::Qdrant;
    state.settings.qdrant_url = "http://127.0.0.1:1".to_string();
    let (status, json) = get(state, "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["checks"]["vector_store"]["backend"], "qdrant");
    assert_eq!(json["checks"]["vector_store"]["reachable"], false);
    assert!(!json.to_string().contains("127.0.0.1"));
}

#[tokio::test]
async fn legacy_ready_route_is_unchanged() {
    // `/ready` keeps its previous contract (always 200; body from the startup storage checks).
    let (status, json) = get(AppState::new(Settings::default()), "/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ready");
    assert_eq!(json["checks"]["providers"]["configured"], true);
}

/// Readiness reports the runtime quality tier; mock providers are never
/// reported as a validated tier, and nothing claims a live provider verification by default.
#[tokio::test]
async fn ready_reports_the_quality_tier() {
    let (_, json) = get(AppState::new(mock_allowed()), "/health/ready").await;
    assert_eq!(json["quality"]["tier"], "development-mock");
    assert_eq!(json["quality"]["reference_gateway"], "not_applicable");
    assert_eq!(json["quality"]["llm_startup_probe"], "skipped");
    assert_eq!(json["quality"]["live_provider_verified"], false);
    assert!(
        !json["quality"]["reference_mismatches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Readiness records the classification-skip flag; with it on, the
/// variant is listed with its validation record (output-equivalent, not operationally equivalent)
/// and the tier is not changed silently.
#[tokio::test]
async fn ready_reports_the_classification_skip_variant() {
    let (_, off) = get(AppState::new(mock_allowed()), "/health/ready").await;
    assert_eq!(off["quality"]["classification_skip_when_no_related"], false);
    assert_eq!(off["quality"]["validated_variants"], serde_json::json!([]));

    let mut settings = mock_allowed();
    settings.classification_skip_when_no_related = true;
    let (_, on) = get(AppState::new(settings), "/health/ready").await;
    assert_eq!(on["quality"]["tier"], off["quality"]["tier"]);
    assert_eq!(
        on["quality"]["reference_mismatches"],
        off["quality"]["reference_mismatches"]
    );
    assert_eq!(on["quality"]["classification_skip_when_no_related"], true);
    let variant = &on["quality"]["validated_variants"][0];
    assert_eq!(variant["name"], "classification-skip");
    assert_eq!(variant["equivalence"], "output-equivalent");
    assert_eq!(variant["operationally_equivalent"], false);
    assert_eq!(
        variant["intentional_operational_differences"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

fn reference_settings(base_url: String) -> Settings {
    use memcore_config::{EmbeddingProviderKind, LlmProviderKind};
    let mut s = Settings::default();
    s.llm_provider = LlmProviderKind::OpenAi;
    s.embedding_provider = EmbeddingProviderKind::OpenAi;
    s.llm_model = memcore_config::REFERENCE_LLM_MODEL.into();
    s.embedding_model = memcore_config::REFERENCE_EMBEDDING_MODEL.into();
    s.episodes_enabled = true;
    s.search_max_limit = 100;
    s.qdrant_search_hnsw_ef = Some(memcore_config::REFERENCE_QDRANT_SEARCH_HNSW_EF);
    s.openai_base_url = base_url;
    s.openai_api_key = Some("sk-INVENTED-gateway-key".into());
    s
}

async fn fake_gateway(reference: bool) -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ready",
            "source_proxy_sha256": memcore_providers::reference_gateway::REFERENCE_GATEWAY_SOURCE_SHA256,
            "reference_configuration": reference, "reference_mismatches": [],
        })))
        .mount(&server)
        .await;
    server
}

/// The startup quality checks.
#[tokio::test]
async fn startup_quality_confirms_downgrades_and_refuses() {
    use memcore_api::quality::startup_quality;
    use memcore_config::QualityTier;

    // Reference configuration + the reference gateway: reference-validated, no chat call made.
    let gateway = fake_gateway(true).await;
    let q = startup_quality(&reference_settings(format!("{}/v1", gateway.uri())))
        .await
        .unwrap();
    assert_eq!(
        (q.tier.as_str(), q.reference_gateway.as_str()),
        ("reference-validated", "confirmed")
    );
    assert!(!q.live_provider_verified);
    assert!(q.evidence.starts_with("historical"));
    assert!(
        gateway
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.url.path() == "/ready")
    );

    // The gateway is not in its reference configuration: downgraded, not refused...
    let other = fake_gateway(false).await;
    let q = startup_quality(&reference_settings(other.uri()))
        .await
        .unwrap();
    assert_eq!(
        (q.tier.as_str(), q.reference_gateway.as_str()),
        ("compatible-unvalidated", "failed")
    );
    assert_eq!(q.configured_tier, "reference-validated");
    // ...unless the reference tier is required.
    let mut required = reference_settings(other.uri());
    required.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
    assert!(startup_quality(&required).await.is_err());
    // A changed reference parameter is compatible-unvalidated without asking any gateway.
    let mut changed = reference_settings("http://127.0.0.1:9".into());
    changed.providers.llm_json_mode = memcore_config::LlmJsonMode::JsonSchema;
    let q = startup_quality(&changed).await.unwrap();
    assert_eq!(
        (q.tier.as_str(), q.reference_gateway.as_str()),
        ("compatible-unvalidated", "not_applicable")
    );
    assert_eq!(q.reference_mismatches, vec!["llm_json_mode".to_string()]);
    // The rollback beam (unset) is compatible-unvalidated without
    // asking any gateway, and the required reference tier refuses it.
    let mut rollback = reference_settings("http://127.0.0.1:9".into());
    rollback.qdrant_search_hnsw_ef = None;
    let q = startup_quality(&rollback).await.unwrap();
    assert_eq!(
        (q.tier.as_str(), q.reference_gateway.as_str()),
        ("compatible-unvalidated", "not_applicable")
    );
    assert_eq!(
        q.reference_mismatches,
        vec!["qdrant_search_hnsw_ef".to_string()]
    );
    rollback.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
    assert!(startup_quality(&rollback).await.is_err());
    // The default packing (prefix) is the measured reference packing, so the reference configuration
    // above is reference-validated with no explicit packing; opting the server default into evidence
    // is compatible-unvalidated without asking any gateway, and the required reference tier refuses it.
    let mut evidence = reference_settings("http://127.0.0.1:9".into());
    evidence.context_default_packing = memcore_config::ContextPacking::Evidence;
    let q = startup_quality(&evidence).await.unwrap();
    assert_eq!(
        (q.tier.as_str(), q.reference_gateway.as_str()),
        ("compatible-unvalidated", "not_applicable")
    );
    assert_eq!(
        q.reference_mismatches,
        vec!["context_default_packing".to_string()]
    );
    evidence.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
    assert!(startup_quality(&evidence).await.is_err());
}

/// The opt-in chat probe (fake LLM endpoint; one call; off by default).
#[tokio::test]
async fn llm_startup_probe_passes_on_valid_json_and_refuses_otherwise() {
    use memcore_api::quality::startup_quality;
    use wiremock::matchers::{method, path};
    let reply = |content: &str| serde_json::json!({"choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]});
    for (content, ok) in [
        (
            r#"{"operation":"Add","target_fact_id":null,"reason":null,"confidence":0.9}"#,
            true,
        ),
        ("I think you should add it.", false),
    ] {
        let llm = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(reply(content)))
            .mount(&llm)
            .await;
        let mut s = reference_settings(format!("{}/v1", llm.uri()));
        s.llm_model = "byom-chat".into(); // not reference: no gateway check
        s.providers.llm_startup_probe = true;
        s.real_provider_calls_enabled = true;
        s.provider_test_mode = memcore_config::ProviderTestMode::Production;
        let result = startup_quality(&s).await;
        assert_eq!(result.is_ok(), ok, "{result:?}");
        if let Ok(q) = result {
            assert_eq!(q.llm_startup_probe, "passed");
            assert!(q.live_provider_verified);
        }
        assert_eq!(
            llm.received_requests().await.unwrap().len(),
            1,
            "exactly one probe call"
        );
    }
    // Default: no probe, nothing called.
    let s = reference_settings("http://127.0.0.1:9".into());
    assert!(!s.providers.llm_startup_probe);
}

/// Readiness checks the PostgreSQL database LIVE (it used to report the startup
/// check, so a database lost after startup stayed "ready"). A probe pointed at a port where nothing
/// listens stands in for a database that stopped answering after startup.
#[cfg(feature = "postgres")]
mod live_postgres_readiness {
    use super::*;
    use std::time::{Duration, Instant};

    const DEAD: &str = "postgres://ar076_user:ar076_secret@127.0.0.1:1/ar076_db";

    fn with_probe(url: &str) -> AppState {
        let mut state = AppState::new(mock_allowed());
        state.database_readiness =
            memcore_storage::DatabaseReadinessProbe::postgres(url, Duration::from_secs(2))
                .expect("probe builds lazily");
        state
    }

    #[tokio::test]
    async fn ready_is_503_when_postgres_stops_answering_and_hides_the_url() {
        let state = with_probe(DEAD);
        let started = Instant::now();
        let (status, json) = get(state.clone(), "/health/ready").await;
        let elapsed = started.elapsed();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{json}");
        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["checks"]["database"]["connected"], false);
        assert!(elapsed < Duration::from_secs(4), "bounded: {elapsed:?}");
        let text = json.to_string();
        for secret in ["ar076_secret", "ar076_user", "postgres://", "127.0.0.1:1"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        // The legacy route reports the same live state; liveness is untouched.
        let (_, legacy) = get(state.clone(), "/ready").await;
        assert_eq!(legacy["status"], "not_ready");
        assert_eq!(legacy["checks"]["database"]["connected"], false);
        let started = Instant::now();
        let (live, _) = get(state, "/health/live").await;
        assert_eq!(live, StatusCode::OK);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn concurrent_readiness_polls_during_an_outage_stay_bounded() {
        let state = with_probe(DEAD);
        let started = Instant::now();
        let mut polls = tokio::task::JoinSet::new();
        for _ in 0..50 {
            let state = state.clone();
            polls.spawn(async move { get(state, "/health/ready").await });
        }
        let mut results = Vec::new();
        while let Some(result) = polls.join_next().await {
            results.push(result.expect("poll task"));
        }
        let elapsed = started.elapsed();
        assert_eq!(results.len(), 50);
        assert!(
            results
                .iter()
                .all(|(status, _)| *status == StatusCode::SERVICE_UNAVAILABLE)
        );
        // One connection at most, every check bounded by the 2 s probe timeout.
        assert!(
            elapsed < Duration::from_secs(6),
            "50 polls took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn ready_is_200_while_postgres_answers() {
        let Ok(url) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: MEMCORE_TEST_POSTGRES_URL not set");
            return;
        };
        let state = with_probe(&url);
        for _ in 0..3 {
            let (status, json) = get(state.clone(), "/health/ready").await;
            assert_eq!(status, StatusCode::OK, "{json}");
            assert_eq!(json["checks"]["database"]["connected"], true);
        }
    }
}
