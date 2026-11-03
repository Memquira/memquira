//! HTTP middleware and helpers for provider test-source tagging.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use memcore_common::MemcoreResult;
use memcore_config::{ProviderTestMode as ConfigTestMode, Settings};
use memcore_providers::{
    ProviderCallSource, ProviderCallSourceSlot, ProviderGuardrailConfig, ProviderGuardrailEnforcer,
    ProviderTestMode,
};

pub const TEST_SOURCE_HEADER: &str = "X-Memcore-Test-Source";

pub fn guardrail_config_from_settings(settings: &Settings) -> ProviderGuardrailConfig {
    let test_mode = match settings.provider_test_mode {
        ConfigTestMode::MockOnly => ProviderTestMode::MockOnly,
        ConfigTestMode::SingleReal => ProviderTestMode::SingleReal,
        ConfigTestMode::MultiReal => ProviderTestMode::MultiReal,
        ConfigTestMode::Production => ProviderTestMode::Production,
    };

    ProviderGuardrailConfig {
        enabled: settings.provider_guardrails_enabled,
        real_provider_calls_enabled: settings.real_provider_calls_enabled,
        test_mode,
        max_calls_per_run: settings.provider_max_calls_per_run,
        max_input_chars: settings.provider_max_input_chars,
        max_output_tokens: settings.provider_max_output_tokens,
        max_retries_per_call: settings.provider_max_retries_per_call,
        timeout: Duration::from_secs(settings.provider_timeout_seconds),
        allow_real_providers_during_load_tests: settings.allow_real_providers_during_load_tests,
        background_jobs_allow_real_providers: settings.background_jobs_allow_real_providers,
        multi_provider_validation_enabled: settings.multi_provider_validation_enabled,
        multi_provider_validation_confirmation: settings
            .multi_provider_validation_confirmation
            .clone(),
    }
}

pub fn create_provider_guardrails(
    settings: &Settings,
) -> MemcoreResult<(Arc<ProviderGuardrailEnforcer>, Arc<ProviderCallSourceSlot>)> {
    let config = guardrail_config_from_settings(settings);
    let enforcer = ProviderGuardrailEnforcer::new(config)?;
    Ok((Arc::new(enforcer), Arc::new(ProviderCallSourceSlot::new())))
}

/// Hint-only middleware: maps `X-Memcore-Test-Source` into the process call-source slot.
/// Not a security boundary in production — treat as a test/ops hint.
pub async fn apply_provider_test_source(
    State(slot): State<Arc<ProviderCallSourceSlot>>,
    request: Request,
    next: Next,
) -> Response {
    let source = request
        .headers()
        .get(TEST_SOURCE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ProviderCallSource::from_test_header)
        .unwrap_or(ProviderCallSource::ApiRequest);
    slot.set(source);
    let response = next.run(request).await;
    slot.clear();
    response
}
