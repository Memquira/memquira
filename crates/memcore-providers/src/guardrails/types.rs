use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use memcore_common::{MemcoreError, MemcoreResult};

/// Exact confirmation string required for multi-provider credit-using validation.
pub const MULTI_PROVIDER_VALIDATION_CONFIRMATION: &str =
    "I_UNDERSTAND_THIS_WILL_USE_PROVIDER_CREDITS";

/// Safe upper bound for per-call provider retries under guardrails.
pub const MAX_SAFE_PROVIDER_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTestMode {
    MockOnly,
    SingleReal,
    MultiReal,
    Production,
}

impl ProviderTestMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MockOnly => "mock_only",
            Self::SingleReal => "single_real",
            Self::MultiReal => "multi_real",
            Self::Production => "production",
        }
    }
}

impl FromStr for ProviderTestMode {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_provider_test_mode(value)
    }
}

pub fn parse_provider_test_mode(value: &str) -> MemcoreResult<ProviderTestMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "mock_only" => Ok(ProviderTestMode::MockOnly),
        "single_real" => Ok(ProviderTestMode::SingleReal),
        "multi_real" => Ok(ProviderTestMode::MultiReal),
        "production" => Ok(ProviderTestMode::Production),
        other => Err(MemcoreError::ValidationError(format!(
            "invalid MEMCORE_PROVIDER_TEST_MODE '{other}' (expected mock_only|single_real|multi_real|production)"
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderCallSource {
    #[default]
    ApiRequest,
    SmokeTest,
    LoadTest,
    BackgroundJob,
    AdminValidation,
}

impl ProviderCallSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiRequest => "api-request",
            Self::SmokeTest => "smoke-test",
            Self::LoadTest => "load-test",
            Self::BackgroundJob => "background-job",
            Self::AdminValidation => "admin-validation",
        }
    }

    /// Parse `X-Memcore-Test-Source` header values. Unknown values fall back to ApiRequest.
    pub fn from_test_header(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "smoke-test" | "smoke_test" | "smoke" => Self::SmokeTest,
            "load-test" | "load_test" | "load" => Self::LoadTest,
            "admin-validation" | "admin_validation" => Self::AdminValidation,
            "background-job" | "background_job" => Self::BackgroundJob,
            _ => Self::ApiRequest,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderGuardrailConfig {
    pub enabled: bool,
    pub real_provider_calls_enabled: bool,
    pub test_mode: ProviderTestMode,
    pub max_calls_per_run: usize,
    pub max_input_chars: usize,
    pub max_output_tokens: usize,
    pub max_retries_per_call: usize,
    pub timeout: Duration,
    pub allow_real_providers_during_load_tests: bool,
    pub background_jobs_allow_real_providers: bool,
    pub multi_provider_validation_enabled: bool,
    pub multi_provider_validation_confirmation: Option<String>,
}

impl Default for ProviderGuardrailConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            real_provider_calls_enabled: false,
            test_mode: ProviderTestMode::MockOnly,
            max_calls_per_run: 10,
            max_input_chars: 4000,
            // Mirrors memcore-config DEFAULT_PROVIDER_MAX_OUTPUT_TOKENS.
            max_output_tokens: 2_048,
            max_retries_per_call: 1,
            timeout: Duration::from_secs(20),
            allow_real_providers_during_load_tests: false,
            background_jobs_allow_real_providers: false,
            multi_provider_validation_enabled: false,
            multi_provider_validation_confirmation: None,
        }
    }
}

impl ProviderGuardrailConfig {
    pub fn validate(&self) -> MemcoreResult<()> {
        if self.max_calls_per_run == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_CALLS_PER_RUN must be greater than 0".to_string(),
            ));
        }
        if self.max_input_chars == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_INPUT_CHARS must be greater than 0".to_string(),
            ));
        }
        if self.max_output_tokens == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS must be greater than 0".to_string(),
            ));
        }
        if self.max_retries_per_call > MAX_SAFE_PROVIDER_RETRIES {
            return Err(MemcoreError::ValidationError(format!(
                "MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL cannot exceed {MAX_SAFE_PROVIDER_RETRIES}"
            )));
        }
        if self.timeout.is_zero() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_TIMEOUT_SECONDS (guardrail) must be greater than 0".to_string(),
            ));
        }

        if self.test_mode == ProviderTestMode::MultiReal {
            if !self.multi_provider_validation_enabled {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED must be true when MEMCORE_PROVIDER_TEST_MODE=multi_real"
                        .to_string(),
                ));
            }
            let confirmed = self
                .multi_provider_validation_confirmation
                .as_deref()
                .map(str::trim)
                == Some(MULTI_PROVIDER_VALIDATION_CONFIRMATION);
            if !confirmed {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_MULTI_PROVIDER_VALIDATION_CONFIRMATION must be exactly I_UNDERSTAND_THIS_WILL_USE_PROVIDER_CREDITS for multi_real mode"
                        .to_string(),
                ));
            }
        }

        Ok(())
    }

    pub fn multi_provider_confirmed(&self) -> bool {
        self.multi_provider_validation_enabled
            && self
                .multi_provider_validation_confirmation
                .as_deref()
                .map(str::trim)
                == Some(MULTI_PROVIDER_VALIDATION_CONFIRMATION)
    }
}

#[derive(Debug, Clone)]
pub struct ProviderCallGuardInput {
    pub provider_name: String,
    pub model_name: String,
    pub operation: String,
    pub source: ProviderCallSource,
    pub input_char_count: usize,
    pub requested_output_tokens: Option<usize>,
    pub is_real_provider: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderGuardDecision {
    Allowed,
    Blocked { code: String, message: String },
}

/// Process-local request/job call-source hint (same concurrency caveats as usage attribution).
#[derive(Debug, Default)]
pub struct ProviderCallSourceSlot {
    value: Mutex<ProviderCallSource>,
}

impl ProviderCallSourceSlot {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(ProviderCallSource::ApiRequest),
        }
    }

    pub fn set(&self, source: ProviderCallSource) {
        if let Ok(mut guard) = self.value.lock() {
            *guard = source;
        }
    }

    pub fn clear(&self) {
        self.set(ProviderCallSource::ApiRequest);
    }

    pub fn snapshot(&self) -> ProviderCallSource {
        self.value
            .lock()
            .map(|guard| *guard)
            .unwrap_or(ProviderCallSource::ApiRequest)
    }
}
