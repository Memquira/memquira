//! Provider usage recorder that also emits Prometheus counters.
//!
//! The provider router records one usage event per attempted provider call (one per logical call
//! when fallback is off), after taking that call's token usage exactly once, so these counters do
//! not double count. Labels are bounded: provider kind, role (capability) and a fixed operation
//! name; never the model id, tenant, user, text or URL.
//!
//! Token metrics: `memcore_provider_reported_tokens_total` counts ONLY the token counts
//! the provider reported in its responses (`usage`); memcore's character-count estimates are never
//! exported as tokens. `memcore_provider_usage_reports_total{outcome}` counts whether each call's
//! responses reported usage (`reported`, `partial`, `missing`, `malformed`). Reported counts are
//! what the endpoint says it processed; they are not an invoice.

use std::sync::Arc;

use memcore_core::ProviderUsageCapability;
use memcore_providers::{
    ProviderCallStatus, ProviderUsageEvent, ProviderUsageRecorder, ProviderUsageSnapshot,
};

use super::ops::{
    record_provider_circuit_open, record_provider_failure, record_provider_request,
    record_provider_retries,
};

pub struct PrometheusProviderUsageRecorder {
    inner: Arc<dyn ProviderUsageRecorder>,
}

impl PrometheusProviderUsageRecorder {
    pub fn wrap(inner: Arc<dyn ProviderUsageRecorder>) -> Arc<dyn ProviderUsageRecorder> {
        Arc::new(Self { inner })
    }
}

impl ProviderUsageRecorder for PrometheusProviderUsageRecorder {
    fn record_request(&self, event: ProviderUsageEvent) {
        let provider = provider_label(&event.provider_name);
        let role = role_label(event.capability);
        let operation = sanitize_operation(&event.operation_name);
        let status = match event.status {
            ProviderCallStatus::Success => "success",
            ProviderCallStatus::Error => "error",
        };
        record_provider_request(provider, role, operation, status);
        if matches!(event.status, ProviderCallStatus::Error) {
            let class = if event.timed_out {
                "timeout"
            } else if event.circuit_blocked {
                "circuit_blocked"
            } else {
                "error"
            };
            record_provider_failure(provider, class);
        }
        record_provider_retries(provider, event.retry_count);
        metrics::counter!(
            "memcore_provider_usage_reports_total",
            "provider" => provider,
            "role" => role,
            "outcome" => event.usage_report.as_str(),
        )
        .increment(1);
        if matches!(
            event.token_source,
            memcore_providers::usage::TokenSource::Reported
                | memcore_providers::usage::TokenSource::PartiallyReported
        ) {
            for (direction, tokens) in [
                ("input", event.input_tokens),
                ("output", event.output_tokens),
            ] {
                if let Some(tokens) = tokens {
                    metrics::counter!(
                        "memcore_provider_reported_tokens_total",
                        "provider" => provider,
                        "role" => role,
                        "direction" => direction,
                    )
                    .increment(tokens);
                }
            }
        }
        if event.circuit_blocked {
            record_provider_circuit_open(provider);
        }
        self.inner.record_request(event);
    }

    fn snapshot(&self) -> ProviderUsageSnapshot {
        self.inner.snapshot()
    }
}

/// Provider kinds memcore can construct; anything else is `other`.
pub fn provider_label(name: &str) -> &'static str {
    match name {
        "openai" => "openai",
        "mock" => "mock",
        "openrouter" => "openrouter",
        "anthropic" => "anthropic",
        "groq" => "groq",
        "bedrock" => "bedrock",
        _ => "other",
    }
}

fn role_label(capability: ProviderUsageCapability) -> &'static str {
    match capability {
        ProviderUsageCapability::Llm => "llm",
        ProviderUsageCapability::Embedding => "embedding",
        ProviderUsageCapability::Summarization => "summarization",
    }
}

/// The router's fixed operation names (code constants); anything else is `other`.
const OPERATIONS: &[&str] = &[
    "llm_extract_facts",
    "llm_classify_fact_operation",
    "llm_summarize_memory",
    "llm_annotate_entities",
    "llm_annotate_entities_retry",
    "llm_confirm_entity_match",
    "llm_decide_consolidation",
    "llm_decompose_query",
    "embedding_embed_text",
    "embedding_embed_query",
    "embedding_embed_batch",
    "chat",
    "embedding",
    "extraction",
    "summarization",
    "rerank",
];

fn sanitize_operation(name: &str) -> &'static str {
    OPERATIONS
        .iter()
        .copied()
        .find(|operation| *operation == name)
        .unwrap_or("other")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_bounded() {
        assert_eq!(provider_label("openai"), "openai");
        assert_eq!(provider_label("https://secret-host.example/v1"), "other");
        assert_eq!(sanitize_operation("llm_extract_facts"), "llm_extract_facts");
        assert_eq!(
            sanitize_operation("embedding_embed_query"),
            "embedding_embed_query"
        );
        assert_eq!(sanitize_operation("user supplied text"), "other");
    }
}
