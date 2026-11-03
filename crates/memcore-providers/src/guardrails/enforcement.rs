use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use memcore_common::{MemcoreError, MemcoreResult};

use super::budget::ProviderCallBudget;
use super::types::{
    ProviderCallGuardInput, ProviderCallSource, ProviderGuardDecision, ProviderGuardrailConfig,
    ProviderTestMode,
};

/// Returns true when the provider name is treated as a billed/external provider. In-process local
/// models (`local_*`) are neither billed nor external, so the real-provider
/// guardrails (single-real locking, call budgets) do not apply to them.
pub fn is_real_provider_name(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    !(normalized.is_empty()
        || normalized == "mock"
        || normalized == "primary"
        || normalized.starts_with("mock-")
        || normalized.starts_with("local_"))
}

pub struct ProviderGuardrailEnforcer {
    config: ProviderGuardrailConfig,
    budget: Arc<ProviderCallBudget>,
    /// Distinct real provider names allowed/used in this process for single_real locking.
    used_real_providers: Mutex<HashSet<String>>,
}

impl ProviderGuardrailEnforcer {
    pub fn new(config: ProviderGuardrailConfig) -> MemcoreResult<Self> {
        config.validate()?;
        let max_calls = config.max_calls_per_run;
        Ok(Self {
            config,
            budget: Arc::new(ProviderCallBudget::new(max_calls)),
            used_real_providers: Mutex::new(HashSet::new()),
        })
    }

    pub fn config(&self) -> &ProviderGuardrailConfig {
        &self.config
    }

    pub fn budget(&self) -> &ProviderCallBudget {
        &self.budget
    }

    pub fn status_snapshot(&self) -> ProviderGuardrailStatus {
        ProviderGuardrailStatus {
            enabled: self.config.enabled,
            real_provider_calls_enabled: self.config.real_provider_calls_enabled,
            test_mode: self.config.test_mode.as_str().to_string(),
            max_calls_per_run: self.budget.max_calls(),
            used_calls: self.budget.used(),
            remaining_calls: self.budget.remaining(),
            max_input_chars: self.config.max_input_chars,
            max_output_tokens: self.config.max_output_tokens,
            max_retries_per_call: self.config.max_retries_per_call,
            timeout_seconds: self.config.timeout.as_secs(),
            allow_real_providers_during_load_tests: self
                .config
                .allow_real_providers_during_load_tests,
            background_jobs_allow_real_providers: self.config.background_jobs_allow_real_providers,
            multi_provider_validation_enabled: self.config.multi_provider_validation_enabled,
        }
    }

    /// Effective max tokens for a request. Returns Err when rejected in test modes.
    pub fn resolve_output_tokens(&self, requested: Option<usize>) -> MemcoreResult<Option<usize>> {
        if !self.config.enabled {
            return Ok(requested);
        }
        let Some(requested) = requested else {
            return Ok(None);
        };
        if requested <= self.config.max_output_tokens {
            return Ok(Some(requested));
        }
        match self.config.test_mode {
            ProviderTestMode::Production => Ok(Some(self.config.max_output_tokens)),
            _ => Err(MemcoreError::provider_guardrail_violation(
                "output_tokens_exceeded",
                format!(
                    "requested output tokens exceed guardrail max ({})",
                    self.config.max_output_tokens
                ),
            )),
        }
    }

    pub fn check_provider_call(
        &self,
        input: ProviderCallGuardInput,
    ) -> MemcoreResult<ProviderGuardDecision> {
        if !self.config.enabled {
            return Ok(ProviderGuardDecision::Allowed);
        }

        if !input.is_real_provider {
            return Ok(ProviderGuardDecision::Allowed);
        }

        if !self.config.real_provider_calls_enabled {
            return Ok(blocked(
                "real_provider_calls_disabled",
                "real provider calls are disabled (set MEMCORE_REAL_PROVIDER_CALLS_ENABLED=true to allow)",
            ));
        }

        if self.config.test_mode == ProviderTestMode::MockOnly {
            return Ok(blocked(
                "mock_only_mode",
                "provider test mode is mock_only; real providers are not allowed",
            ));
        }

        if input.source == ProviderCallSource::LoadTest
            && !self.config.allow_real_providers_during_load_tests
        {
            return Ok(blocked(
                "load_test_real_provider_blocked",
                "real providers are blocked for load-test source; use mock providers",
            ));
        }

        if input.source == ProviderCallSource::BackgroundJob
            && !self.config.background_jobs_allow_real_providers
        {
            return Ok(blocked(
                "background_job_real_provider_blocked",
                "real providers are blocked for background jobs unless explicitly allowed",
            ));
        }

        if input.input_char_count > self.config.max_input_chars {
            return Ok(blocked(
                "input_chars_exceeded",
                format!(
                    "provider input exceeds max characters ({})",
                    self.config.max_input_chars
                ),
            ));
        }

        if let Some(tokens) = input.requested_output_tokens
            && tokens > self.config.max_output_tokens
        {
            match self.config.test_mode {
                ProviderTestMode::Production => {
                    // Clamping is applied by callers via resolve_output_tokens.
                }
                _ => {
                    return Ok(blocked(
                        "output_tokens_exceeded",
                        format!(
                            "requested output tokens exceed guardrail max ({})",
                            self.config.max_output_tokens
                        ),
                    ));
                }
            }
        }

        {
            let mut used = self
                .used_real_providers
                .lock()
                .map_err(|_| MemcoreError::Internal("provider guardrail lock poisoned".into()))?;
            let provider_key = input.provider_name.to_ascii_lowercase();

            match self.config.test_mode {
                ProviderTestMode::SingleReal => {
                    if used.is_empty() {
                        used.insert(provider_key);
                    } else if !used.contains(&provider_key) {
                        return Ok(blocked(
                            "single_real_provider_locked",
                            "single_real mode allows only one real provider per process; enable multi_real with confirmation to test more",
                        ));
                    }
                }
                ProviderTestMode::MultiReal => {
                    if !self.config.multi_provider_confirmed() {
                        return Ok(blocked(
                            "multi_provider_confirmation_required",
                            "multi_real mode requires multi-provider validation confirmation",
                        ));
                    }
                    used.insert(provider_key);
                }
                ProviderTestMode::Production => {
                    if used.len() > 1 && !self.config.multi_provider_validation_enabled {
                        // Production may use fallbacks; do not block distinct names here.
                    }
                    used.insert(provider_key);
                }
                ProviderTestMode::MockOnly => unreachable!("handled above"),
            }

            // Defense: if multi validation is off and more than one distinct real provider
            // appears outside production, block (single_real already handled).
            if self.config.test_mode != ProviderTestMode::Production
                && !self.config.multi_provider_validation_enabled
                && used.len() > 1
            {
                return Ok(blocked(
                    "multi_provider_validation_disabled",
                    "multiple real providers require MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED",
                ));
            }
        }

        if !self.budget.try_consume() {
            return Ok(blocked(
                "call_budget_exhausted",
                format!(
                    "provider call budget exhausted ({}/{})",
                    self.budget.used(),
                    self.budget.max_calls()
                ),
            ));
        }

        tracing::info!(
            provider_name = %input.provider_name,
            model_name = %input.model_name,
            operation = %input.operation,
            source = input.source.as_str(),
            input_char_count = input.input_char_count,
            used_calls = self.budget.used(),
            remaining_calls = self.budget.remaining(),
            "provider guardrail allowed real provider call"
        );

        Ok(ProviderGuardDecision::Allowed)
    }

    pub fn decide_or_error(&self, input: ProviderCallGuardInput) -> MemcoreResult<()> {
        match self.check_provider_call(input)? {
            ProviderGuardDecision::Allowed => Ok(()),
            ProviderGuardDecision::Blocked { code, message } => {
                tracing::warn!(
                    guardrail_code = %code,
                    "provider call blocked by guardrail"
                );
                Err(MemcoreError::provider_guardrail_violation(code, message))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderGuardrailStatus {
    pub enabled: bool,
    pub real_provider_calls_enabled: bool,
    pub test_mode: String,
    pub max_calls_per_run: usize,
    pub used_calls: usize,
    pub remaining_calls: usize,
    pub max_input_chars: usize,
    pub max_output_tokens: usize,
    pub max_retries_per_call: usize,
    pub timeout_seconds: u64,
    pub allow_real_providers_during_load_tests: bool,
    pub background_jobs_allow_real_providers: bool,
    pub multi_provider_validation_enabled: bool,
}

fn blocked(code: impl Into<String>, message: impl Into<String>) -> ProviderGuardDecision {
    ProviderGuardDecision::Blocked {
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guardrails::types::MULTI_PROVIDER_VALIDATION_CONFIRMATION;

    fn base_config() -> ProviderGuardrailConfig {
        ProviderGuardrailConfig::default()
    }

    fn real_input(source: ProviderCallSource) -> ProviderCallGuardInput {
        ProviderCallGuardInput {
            provider_name: "openai".into(),
            model_name: "gpt-test".into(),
            operation: "llm_extract_facts".into(),
            source,
            input_char_count: 40,
            requested_output_tokens: Some(64),
            is_real_provider: true,
        }
    }

    #[test]
    fn local_in_process_providers_are_not_real_providers() {
        assert!(!is_real_provider_name("local_bge"));
        assert!(!is_real_provider_name("Local_Example"));
        assert!(is_real_provider_name("openai"));
        assert!(
            is_real_provider_name("localhost-proxy"),
            "only the local_ prefix is exempt"
        );
    }

    #[test]
    fn mock_allowed_in_mock_only() {
        let enforcer = ProviderGuardrailEnforcer::new(base_config()).unwrap();
        let decision = enforcer
            .check_provider_call(ProviderCallGuardInput {
                provider_name: "mock".into(),
                model_name: "mock".into(),
                operation: "embed".into(),
                source: ProviderCallSource::ApiRequest,
                input_char_count: 10,
                requested_output_tokens: None,
                is_real_provider: false,
            })
            .unwrap();
        assert_eq!(decision, ProviderGuardDecision::Allowed);
        assert_eq!(enforcer.budget().used(), 0);
    }

    #[test]
    fn real_blocked_in_mock_only() {
        let enforcer = ProviderGuardrailEnforcer::new(base_config()).unwrap();
        let decision = enforcer
            .check_provider_call(real_input(ProviderCallSource::ApiRequest))
            .unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "real_provider_calls_disabled"
        ));
    }

    #[test]
    fn real_blocked_when_calls_disabled() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = false;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let decision = enforcer
            .check_provider_call(real_input(ProviderCallSource::SmokeTest))
            .unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "real_provider_calls_disabled"
        ));
    }

    #[test]
    fn real_allowed_in_single_real_when_enabled() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = true;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let decision = enforcer
            .check_provider_call(real_input(ProviderCallSource::SmokeTest))
            .unwrap();
        assert_eq!(decision, ProviderGuardDecision::Allowed);
        assert_eq!(enforcer.budget().used(), 1);
    }

    #[test]
    fn load_test_blocks_real_by_default() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = true;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let decision = enforcer
            .check_provider_call(real_input(ProviderCallSource::LoadTest))
            .unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "load_test_real_provider_blocked"
        ));
    }

    #[test]
    fn background_job_blocks_real_by_default() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::Production;
        config.real_provider_calls_enabled = true;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let decision = enforcer
            .check_provider_call(real_input(ProviderCallSource::BackgroundJob))
            .unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "background_job_real_provider_blocked"
        ));
    }

    #[test]
    fn input_too_large_blocked() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = true;
        config.max_input_chars = 10;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let mut input = real_input(ProviderCallSource::SmokeTest);
        input.input_char_count = 11;
        let decision = enforcer.check_provider_call(input).unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "input_chars_exceeded"
        ));
    }

    #[test]
    fn output_tokens_too_high_blocked_in_test_mode() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = true;
        config.max_output_tokens = 50;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let mut input = real_input(ProviderCallSource::SmokeTest);
        input.requested_output_tokens = Some(100);
        let decision = enforcer.check_provider_call(input).unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "output_tokens_exceeded"
        ));
    }

    #[test]
    fn call_budget_exhausted_blocks() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = true;
        config.max_calls_per_run = 1;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        assert!(matches!(
            enforcer
                .check_provider_call(real_input(ProviderCallSource::SmokeTest))
                .unwrap(),
            ProviderGuardDecision::Allowed
        ));
        let decision = enforcer
            .check_provider_call(real_input(ProviderCallSource::SmokeTest))
            .unwrap();
        assert!(matches!(
            decision,
            ProviderGuardDecision::Blocked { code, .. } if code == "call_budget_exhausted"
        ));
    }

    #[test]
    fn multi_real_without_confirmation_fails_at_config() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::MultiReal;
        config.real_provider_calls_enabled = true;
        config.multi_provider_validation_enabled = true;
        assert!(ProviderGuardrailEnforcer::new(config).is_err());
    }

    #[test]
    fn multi_real_with_confirmation_allows() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::MultiReal;
        config.real_provider_calls_enabled = true;
        config.multi_provider_validation_enabled = true;
        config.multi_provider_validation_confirmation =
            Some(MULTI_PROVIDER_VALIDATION_CONFIRMATION.to_string());
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        assert_eq!(
            enforcer
                .check_provider_call(real_input(ProviderCallSource::AdminValidation))
                .unwrap(),
            ProviderGuardDecision::Allowed
        );
    }

    #[test]
    fn blocked_messages_do_not_include_input_text() {
        let mut config = base_config();
        config.test_mode = ProviderTestMode::SingleReal;
        config.real_provider_calls_enabled = true;
        config.max_input_chars = 5;
        let enforcer = ProviderGuardrailEnforcer::new(config).unwrap();
        let secret_prompt = "SECRET_PROMPT_sk-abc123_should_not_leak";
        let mut input = real_input(ProviderCallSource::SmokeTest);
        input.input_char_count = secret_prompt.len();
        let decision = enforcer.check_provider_call(input).unwrap();
        match decision {
            ProviderGuardDecision::Blocked { message, .. } => {
                assert!(!message.contains("SECRET_PROMPT"));
                assert!(!message.contains("sk-abc"));
            }
            _ => panic!("expected blocked"),
        }
    }
}
