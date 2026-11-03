use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};

use super::state::{CircuitBreakerSnapshot, CircuitState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub failure_threshold: usize,
    pub reset_timeout: Duration,
    pub half_open_max_calls: usize,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            failure_threshold: 5,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }
    }
}

impl CircuitBreakerConfig {
    pub fn for_tests() -> Self {
        Self {
            enabled: true,
            failure_threshold: 3,
            reset_timeout: Duration::from_millis(50),
            half_open_max_calls: 1,
        }
    }

    pub fn from_config(
        enabled: bool,
        failure_threshold: usize,
        reset_timeout_seconds: u64,
        half_open_max_calls: usize,
    ) -> MemcoreResult<Self> {
        validate_circuit_breaker_config(
            failure_threshold,
            reset_timeout_seconds,
            half_open_max_calls,
        )?;
        Ok(Self {
            enabled,
            failure_threshold,
            reset_timeout: Duration::from_secs(reset_timeout_seconds),
            half_open_max_calls,
        })
    }
}

pub fn validate_circuit_breaker_config(
    failure_threshold: usize,
    reset_timeout_seconds: u64,
    half_open_max_calls: usize,
) -> MemcoreResult<()> {
    if failure_threshold == 0 {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD must be greater than 0".to_string(),
        ));
    }
    if reset_timeout_seconds == 0 {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS must be greater than 0"
                .to_string(),
        ));
    }
    if half_open_max_calls == 0 {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS must be greater than 0"
                .to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct CircuitEntry {
    state: CircuitState,
    failure_count: usize,
    opened_at: Option<chrono::DateTime<Utc>>,
    half_open_calls: usize,
}

impl CircuitEntry {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            failure_count: 0,
            opened_at: None,
            half_open_calls: 0,
        }
    }
}

/// Process-local circuit breaker keyed by provider capability, name, and operation.
#[derive(Debug)]
pub struct ProviderCircuitBreaker {
    config: CircuitBreakerConfig,
    circuits: Mutex<HashMap<String, CircuitEntry>>,
}

impl ProviderCircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            circuits: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    pub fn snapshot(&self, key: &str) -> CircuitBreakerSnapshot {
        if !self.config.enabled {
            return CircuitBreakerSnapshot {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
            };
        }

        let circuits = self
            .circuits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        circuits
            .get(key)
            .map(|entry| CircuitBreakerSnapshot {
                state: entry.state,
                failure_count: entry.failure_count,
                opened_at: entry.opened_at,
            })
            .unwrap_or(CircuitBreakerSnapshot {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
            })
    }

    pub fn check_allow(&self, key: &str) -> MemcoreResult<CircuitState> {
        if !self.config.enabled {
            return Ok(CircuitState::Closed);
        }

        let mut circuits = self
            .circuits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = circuits
            .entry(key.to_string())
            .or_insert_with(CircuitEntry::new);
        Self::maybe_transition_open_to_half_open(entry, &self.config);

        match entry.state {
            CircuitState::Closed | CircuitState::HalfOpen => {
                if entry.state == CircuitState::HalfOpen
                    && entry.half_open_calls >= self.config.half_open_max_calls
                {
                    return Err(MemcoreError::provider_circuit_open());
                }
                if entry.state == CircuitState::HalfOpen {
                    entry.half_open_calls += 1;
                }
                Ok(entry.state)
            }
            CircuitState::Open => Err(MemcoreError::provider_circuit_open()),
        }
    }

    pub fn record_success(&self, key: &str) {
        if !self.config.enabled {
            return;
        }

        let mut circuits = self
            .circuits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = circuits
            .entry(key.to_string())
            .or_insert_with(CircuitEntry::new);
        let previous = entry.state;
        entry.state = CircuitState::Closed;
        entry.failure_count = 0;
        entry.opened_at = None;
        entry.half_open_calls = 0;

        if previous == CircuitState::HalfOpen {
            tracing::debug!(
                circuit_key = key,
                circuit_state = "closed",
                "circuit closed after half-open success"
            );
        }
    }

    pub fn record_failure(&self, key: &str) {
        if !self.config.enabled {
            return;
        }

        let mut circuits = self
            .circuits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = circuits
            .entry(key.to_string())
            .or_insert_with(CircuitEntry::new);

        match entry.state {
            CircuitState::Closed => {
                entry.failure_count = entry.failure_count.saturating_add(1);
                if entry.failure_count >= self.config.failure_threshold {
                    entry.state = CircuitState::Open;
                    entry.opened_at = Some(Utc::now());
                    entry.half_open_calls = 0;
                    tracing::warn!(
                        circuit_key = key,
                        failure_count = entry.failure_count,
                        circuit_state = "open",
                        "provider circuit opened"
                    );
                }
            }
            CircuitState::HalfOpen => {
                entry.state = CircuitState::Open;
                entry.opened_at = Some(Utc::now());
                entry.half_open_calls = 0;
                tracing::warn!(
                    circuit_key = key,
                    circuit_state = "open",
                    "provider circuit reopened from half-open failure"
                );
            }
            CircuitState::Open => {}
        }
    }

    fn maybe_transition_open_to_half_open(entry: &mut CircuitEntry, config: &CircuitBreakerConfig) {
        if entry.state != CircuitState::Open {
            return;
        }
        let Some(opened_at) = entry.opened_at else {
            return;
        };
        let elapsed = Utc::now().signed_duration_since(opened_at);
        if elapsed.to_std().unwrap_or(config.reset_timeout) >= config.reset_timeout {
            entry.state = CircuitState::HalfOpen;
            entry.half_open_calls = 0;
            tracing::debug!(
                circuit_state = "half_open",
                "circuit transitioned to half-open"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{ProviderCapability, ProviderId, circuit_key};

    fn breaker() -> ProviderCircuitBreaker {
        ProviderCircuitBreaker::new(CircuitBreakerConfig::for_tests())
    }

    fn key(name: &str, op: &str) -> String {
        circuit_key(&ProviderId::new(name, ProviderCapability::Llm), op)
    }

    #[test]
    fn starts_closed() {
        let breaker = breaker();
        let snapshot = breaker.snapshot(&key("mock", "op"));
        assert_eq!(snapshot.state, CircuitState::Closed);
        assert_eq!(snapshot.failure_count, 0);
    }

    #[test]
    fn retryable_failures_open_circuit_at_threshold() {
        let breaker = breaker();
        let circuit_key = key("mock", "op");
        for _ in 0..2 {
            breaker.record_failure(&circuit_key);
        }
        assert_eq!(breaker.snapshot(&circuit_key).state, CircuitState::Closed);
        breaker.record_failure(&circuit_key);
        assert_eq!(breaker.snapshot(&circuit_key).state, CircuitState::Open);
        assert!(breaker.check_allow(&circuit_key).is_err());
    }

    #[test]
    fn success_resets_failure_count() {
        let breaker = breaker();
        let circuit_key = key("mock", "op");
        breaker.record_failure(&circuit_key);
        breaker.record_failure(&circuit_key);
        breaker.record_success(&circuit_key);
        let snapshot = breaker.snapshot(&circuit_key);
        assert_eq!(snapshot.state, CircuitState::Closed);
        assert_eq!(snapshot.failure_count, 0);
    }

    #[test]
    fn circuit_key_separates_providers_capabilities_and_operations() {
        let a = key("mock", "op_a");
        let b = key("openai", "op_a");
        let c = circuit_key(
            &ProviderId::new("mock", ProviderCapability::Embedding),
            "op_a",
        );
        let d = key("mock", "op_b");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[tokio::test]
    async fn half_open_success_closes_circuit() {
        let breaker = ProviderCircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            reset_timeout: Duration::from_millis(20),
            half_open_max_calls: 1,
            enabled: true,
        });
        let circuit_key = key("mock", "op");
        breaker.record_failure(&circuit_key);
        assert_eq!(breaker.snapshot(&circuit_key).state, CircuitState::Open);
        tokio::time::sleep(Duration::from_millis(30)).await;
        breaker.check_allow(&circuit_key).expect("half-open");
        breaker.record_success(&circuit_key);
        assert_eq!(breaker.snapshot(&circuit_key).state, CircuitState::Closed);
    }

    #[test]
    fn disabled_breaker_always_allows() {
        let breaker = ProviderCircuitBreaker::new(CircuitBreakerConfig {
            enabled: false,
            ..CircuitBreakerConfig::for_tests()
        });
        let circuit_key = key("mock", "op");
        for _ in 0..10 {
            breaker.record_failure(&circuit_key);
        }
        assert!(breaker.check_allow(&circuit_key).is_ok());
    }
}
