use std::sync::{Arc, Mutex};

use super::pricing::{ProviderCostCalculator, lookup_pricing};
use super::types::{
    ProviderCallStatus, ProviderUsageCapability, ProviderUsageEvent, ProviderUsageRecord,
    ProviderUsageSnapshot, UsageAggregateKey,
};

/// Records aggregate provider usage (no prompts, memory content, or secrets).
pub trait ProviderUsageRecorder: Send + Sync {
    fn record_request(&self, event: ProviderUsageEvent);
    fn snapshot(&self) -> ProviderUsageSnapshot;
}

/// Thread-safe in-process usage recorder keyed by provider/model/capability/operation.
#[derive(Debug, Default)]
pub struct InMemoryProviderUsageRecorder {
    records: Mutex<Vec<ProviderUsageRecord>>,
}

impl InMemoryProviderUsageRecorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn merge_event(records: &mut Vec<ProviderUsageRecord>, event: ProviderUsageEvent) {
        let key = UsageAggregateKey::from_event(&event);
        let total_tokens = match (event.input_tokens, event.output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output)),
            (Some(input), None) => Some(input),
            (None, Some(output)) => Some(output),
            (None, None) => None,
        };

        if let Some(record) = records.iter_mut().find(|record| {
            record.provider_name == key.provider_name
                && record.model_name == key.model_name
                && record.capability == key.capability
                && record.operation_name == key.operation_name
        }) {
            record.request_count = record.request_count.saturating_add(1);
            match event.status {
                ProviderCallStatus::Success => {
                    record.success_count = record.success_count.saturating_add(1);
                }
                ProviderCallStatus::Error => {
                    record.error_count = record.error_count.saturating_add(1);
                }
            }
            record.retry_count = record.retry_count.saturating_add(event.retry_count);
            if event.fallback_used {
                record.fallback_count = record.fallback_count.saturating_add(1);
            }
            if event.circuit_blocked {
                record.circuit_blocked_count = record.circuit_blocked_count.saturating_add(1);
            }
            if event.timed_out {
                record.timeout_count = record.timeout_count.saturating_add(1);
            }
            if let Some(input) = event.input_tokens {
                record.input_tokens = Some(record.input_tokens.unwrap_or(0).saturating_add(input));
            }
            if let Some(output) = event.output_tokens {
                record.output_tokens =
                    Some(record.output_tokens.unwrap_or(0).saturating_add(output));
            }
            if let Some(total) = total_tokens {
                record.total_tokens = Some(record.total_tokens.unwrap_or(0).saturating_add(total));
            }
            if let Some(cost) = event.estimated_cost_usd {
                record.estimated_cost_usd = Some(record.estimated_cost_usd.unwrap_or(0.0) + cost);
            }
            return;
        }

        let (success_count, error_count) = match event.status {
            ProviderCallStatus::Success => (1, 0),
            ProviderCallStatus::Error => (0, 1),
        };

        records.push(ProviderUsageRecord {
            provider_name: event.provider_name,
            model_name: event.model_name,
            capability: event.capability,
            operation_name: event.operation_name,
            input_tokens: event.input_tokens,
            output_tokens: event.output_tokens,
            total_tokens,
            request_count: 1,
            success_count,
            error_count,
            retry_count: event.retry_count,
            fallback_count: if event.fallback_used { 1 } else { 0 },
            circuit_blocked_count: if event.circuit_blocked { 1 } else { 0 },
            timeout_count: if event.timed_out { 1 } else { 0 },
            estimated_cost_usd: event.estimated_cost_usd,
        });
    }

    fn build_snapshot(records: &[ProviderUsageRecord]) -> ProviderUsageSnapshot {
        let mut total_requests = 0_u64;
        let mut total_successes = 0_u64;
        let mut total_errors = 0_u64;
        let mut total_retries = 0_u64;
        let mut total_fallbacks = 0_u64;
        let mut total_circuit_blocks = 0_u64;
        let mut total_timeouts = 0_u64;
        let mut total_cost = 0.0_f64;
        let mut has_cost = false;

        for record in records {
            total_requests = total_requests.saturating_add(record.request_count);
            total_successes = total_successes.saturating_add(record.success_count);
            total_errors = total_errors.saturating_add(record.error_count);
            total_retries = total_retries.saturating_add(record.retry_count);
            total_fallbacks = total_fallbacks.saturating_add(record.fallback_count);
            total_circuit_blocks =
                total_circuit_blocks.saturating_add(record.circuit_blocked_count);
            total_timeouts = total_timeouts.saturating_add(record.timeout_count);
            if let Some(cost) = record.estimated_cost_usd {
                total_cost += cost;
                has_cost = true;
            }
        }

        ProviderUsageSnapshot {
            records: records.to_vec(),
            total_requests,
            total_successes,
            total_errors,
            total_retries,
            total_fallbacks,
            total_circuit_blocks,
            total_timeouts,
            total_estimated_cost_usd: if has_cost { Some(total_cost) } else { None },
        }
    }
}

impl ProviderUsageRecorder for InMemoryProviderUsageRecorder {
    fn record_request(&self, event: ProviderUsageEvent) {
        tracing::debug!(
            provider_name = %event.provider_name,
            model_name = ?event.model_name,
            capability = %event.capability,
            operation_name = %event.operation_name,
            status = ?event.status,
            retry_count = event.retry_count,
            fallback_used = event.fallback_used,
            circuit_blocked = event.circuit_blocked,
            timed_out = event.timed_out,
            input_tokens = ?event.input_tokens,
            output_tokens = ?event.output_tokens,
            estimated_cost_usd = ?event.estimated_cost_usd,
            "provider usage recorded"
        );

        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::merge_event(&mut records, event);
    }

    fn snapshot(&self) -> ProviderUsageSnapshot {
        let records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::build_snapshot(&records)
    }
}

/// No-op recorder when usage metrics are disabled.
#[derive(Debug, Default)]
pub struct NoopProviderUsageRecorder;

impl ProviderUsageRecorder for NoopProviderUsageRecorder {
    fn record_request(&self, _event: ProviderUsageEvent) {}

    fn snapshot(&self) -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            records: Vec::new(),
            total_requests: 0,
            total_successes: 0,
            total_errors: 0,
            total_retries: 0,
            total_fallbacks: 0,
            total_circuit_blocks: 0,
            total_timeouts: 0,
            total_estimated_cost_usd: None,
        }
    }
}

pub fn provider_usage_recorder(enabled: bool) -> Arc<dyn ProviderUsageRecorder> {
    if enabled {
        InMemoryProviderUsageRecorder::new()
    } else {
        Arc::new(NoopProviderUsageRecorder)
    }
}

pub fn estimate_event_cost(
    cost_tracking_enabled: bool,
    provider_name: &str,
    model_name: Option<&str>,
    capability: ProviderUsageCapability,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<f64> {
    if !cost_tracking_enabled {
        return None;
    }
    let model = model_name?;
    let pricing = lookup_pricing(provider_name, model)?;
    match capability {
        ProviderUsageCapability::Embedding => {
            ProviderCostCalculator::estimate_embedding_cost_usd(&pricing, input_tokens)
        }
        ProviderUsageCapability::Llm | ProviderUsageCapability::Summarization => {
            ProviderCostCalculator::estimate_cost_usd(&pricing, input_tokens, output_tokens)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::usage::ProviderUsageCapability;

    fn sample_event(status: ProviderCallStatus) -> ProviderUsageEvent {
        ProviderUsageEvent {
            org_id: Some("org_test".to_string()),
            user_id: Some("user_test".to_string()),
            provider_name: "mock".to_string(),
            model_name: Some("mock-llm".to_string()),
            capability: ProviderUsageCapability::Llm,
            operation_name: "llm_extract_facts".to_string(),
            status,
            input_tokens: Some(100),
            output_tokens: Some(20),
            retry_count: 0,
            fallback_used: false,
            circuit_blocked: false,
            timed_out: false,
            estimated_cost_usd: None,
            token_source: crate::usage::TokenSource::Estimated,
            usage_report: crate::usage::UsageReportStatus::NotReported,
        }
    }

    #[test]
    fn starts_empty() {
        let recorder = InMemoryProviderUsageRecorder::new();
        let snapshot = recorder.snapshot();
        assert!(snapshot.records.is_empty());
        assert_eq!(snapshot.total_requests, 0);
    }

    #[test]
    fn records_success() {
        let recorder = InMemoryProviderUsageRecorder::new();
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.total_requests, 1);
        assert_eq!(snapshot.total_successes, 1);
        assert_eq!(snapshot.total_errors, 0);
    }

    #[test]
    fn records_error() {
        let recorder = InMemoryProviderUsageRecorder::new();
        recorder.record_request(sample_event(ProviderCallStatus::Error));
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.total_errors, 1);
    }

    #[test]
    fn aggregates_by_provider_model_capability_operation() {
        let recorder = InMemoryProviderUsageRecorder::new();
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        recorder.record_request(ProviderUsageEvent {
            operation_name: "llm_classify_fact_operation".to_string(),
            ..sample_event(ProviderCallStatus::Success)
        });
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.total_requests, 3);
    }

    #[test]
    fn records_token_counts() {
        let recorder = InMemoryProviderUsageRecorder::new();
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        let record = &recorder.snapshot().records[0];
        assert_eq!(record.input_tokens, Some(200));
        assert_eq!(record.output_tokens, Some(40));
        assert_eq!(record.total_tokens, Some(240));
    }

    #[test]
    fn records_retry_fallback_circuit_timeout_counts() {
        let recorder = InMemoryProviderUsageRecorder::new();
        recorder.record_request(ProviderUsageEvent {
            retry_count: 2,
            fallback_used: true,
            circuit_blocked: false,
            timed_out: true,
            status: ProviderCallStatus::Error,
            ..sample_event(ProviderCallStatus::Error)
        });
        recorder.record_request(ProviderUsageEvent {
            circuit_blocked: true,
            status: ProviderCallStatus::Error,
            ..sample_event(ProviderCallStatus::Error)
        });
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.total_retries, 2);
        assert_eq!(snapshot.total_fallbacks, 1);
        assert_eq!(snapshot.total_circuit_blocks, 1);
        assert_eq!(snapshot.total_timeouts, 1);
    }

    #[test]
    fn snapshot_totals_are_correct() {
        let recorder = InMemoryProviderUsageRecorder::new();
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        recorder.record_request(sample_event(ProviderCallStatus::Error));
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.total_requests, 2);
        assert_eq!(snapshot.total_successes, 1);
        assert_eq!(snapshot.total_errors, 1);
    }

    #[test]
    fn concurrent_recording_is_safe() {
        let recorder = InMemoryProviderUsageRecorder::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let recorder: Arc<InMemoryProviderUsageRecorder> = recorder.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..50 {
                    recorder.record_request(sample_event(ProviderCallStatus::Success));
                }
            }));
        }
        for handle in handles {
            handle.join().expect("thread");
        }
        assert_eq!(recorder.snapshot().total_requests, 400);
    }

    #[test]
    fn noop_recorder_discards_events() {
        let recorder = Arc::new(NoopProviderUsageRecorder);
        recorder.record_request(sample_event(ProviderCallStatus::Success));
        assert_eq!(recorder.snapshot().total_requests, 0);
    }
}
