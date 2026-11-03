use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use memcore_core::ports::{ProviderUsageEventRecord, ProviderUsageStore};
use uuid::Uuid;

use super::ProviderUsageRecorder;
use super::types::ProviderUsageEvent;

/// Records usage in-memory and persists events without failing provider calls.
pub struct PersistentProviderUsageRecorder {
    inner: Arc<dyn ProviderUsageRecorder>,
    store: Option<Arc<dyn ProviderUsageStore>>,
    persistence_errors: Arc<AtomicU64>,
}

impl PersistentProviderUsageRecorder {
    pub fn new(
        inner: Arc<dyn ProviderUsageRecorder>,
        store: Option<Arc<dyn ProviderUsageStore>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            store,
            persistence_errors: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn persistence_errors(&self) -> u64 {
        self.persistence_errors.load(Ordering::Relaxed)
    }
}

impl ProviderUsageRecorder for PersistentProviderUsageRecorder {
    fn record_request(&self, event: ProviderUsageEvent) {
        self.inner.record_request(event.clone());

        let Some(store) = &self.store else {
            return;
        };

        let Some(org_id) = event.org_id.clone() else {
            tracing::warn!("skipping provider usage persistence: missing org_id");
            return;
        };

        let record = ProviderUsageEventRecord {
            id: Uuid::new_v4(),
            org_id,
            user_id: event.user_id.clone(),
            provider_name: event.provider_name,
            model_name: event.model_name,
            capability: event.capability,
            operation_name: event.operation_name,
            status: event.status,
            input_tokens: event.input_tokens,
            output_tokens: event.output_tokens,
            total_tokens: match (event.input_tokens, event.output_tokens) {
                (Some(input), Some(output)) => Some(input.saturating_add(output)),
                (Some(input), None) => Some(input),
                (None, Some(output)) => Some(output),
                (None, None) => None,
            },
            retry_count: event.retry_count,
            fallback_used: event.fallback_used,
            circuit_blocked: event.circuit_blocked,
            timed_out: event.timed_out,
            estimated_cost_usd: event.estimated_cost_usd,
            metadata: Some(serde_json::json!({
                "token_source": event.token_source.as_str(),
                "usage_report": event.usage_report.as_str(),
            })),
            created_at: Utc::now(),
        };

        let store = store.clone();
        let errors = self.persistence_errors.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = store.record_usage_event(record).await {
                    errors.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        error_code = error.code(),
                        "provider usage persistence failed"
                    );
                }
            });
        } else {
            errors.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("provider usage persistence skipped: no tokio runtime");
        }
    }

    fn snapshot(&self) -> super::types::ProviderUsageSnapshot {
        self.inner.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use memcore_core::ports::{ProviderCallStatus, ProviderUsageCapability, ProviderUsageQuery};

    use super::*;
    use crate::usage::{InMemoryProviderUsageRecorder, ProviderUsageEvent};
    use memcore_storage::MockProviderUsageStore;

    fn sample_event(org_id: &str) -> ProviderUsageEvent {
        ProviderUsageEvent {
            org_id: Some(org_id.to_string()),
            user_id: Some("user_a".to_string()),
            provider_name: "mock".to_string(),
            model_name: Some("mock-llm".to_string()),
            capability: ProviderUsageCapability::Llm,
            operation_name: "llm_extract_facts".to_string(),
            status: ProviderCallStatus::Success,
            input_tokens: Some(10),
            output_tokens: Some(2),
            retry_count: 0,
            fallback_used: false,
            circuit_blocked: false,
            timed_out: false,
            estimated_cost_usd: None,
            token_source: crate::usage::TokenSource::Estimated,
            usage_report: crate::usage::UsageReportStatus::NotReported,
        }
    }

    #[tokio::test]
    async fn records_to_inner_and_persistent_store() {
        let inner = InMemoryProviderUsageRecorder::new();
        let store = Arc::new(MockProviderUsageStore::new());
        let recorder = PersistentProviderUsageRecorder::new(inner.clone(), Some(store.clone()));

        recorder.record_request(sample_event("org_persist"));
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(recorder.snapshot().total_requests, 1);
        let result = store
            .query_usage(ProviderUsageQuery::new("org_persist", 10))
            .await
            .expect("query");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].user_id.as_deref(), Some("user_a"));
    }

    #[tokio::test]
    async fn persistence_failure_does_not_fail_caller() {
        struct FailingStore;

        #[async_trait::async_trait]
        impl ProviderUsageStore for FailingStore {
            async fn record_usage_event(
                &self,
                _event: ProviderUsageEventRecord,
            ) -> memcore_common::MemcoreResult<()> {
                Err(memcore_common::MemcoreError::StorageError(
                    "fail".to_string(),
                ))
            }

            async fn query_usage(
                &self,
                _query: memcore_core::ports::ProviderUsageQuery,
            ) -> memcore_common::MemcoreResult<memcore_core::ports::ProviderUsageQueryResult>
            {
                Err(memcore_common::MemcoreError::StorageError(
                    "fail".to_string(),
                ))
            }

            async fn query_usage_daily(
                &self,
                _query: memcore_core::ports::ProviderUsageDailyQuery,
            ) -> memcore_common::MemcoreResult<Vec<memcore_core::ProviderUsageDailyBucket>>
            {
                Err(memcore_common::MemcoreError::StorageError(
                    "fail".to_string(),
                ))
            }

            async fn delete_usage_events_older_than(
                &self,
                _org_id: &str,
                _cutoff: chrono::DateTime<chrono::Utc>,
                _dry_run: bool,
            ) -> memcore_common::MemcoreResult<usize> {
                Err(memcore_common::MemcoreError::StorageError(
                    "fail".to_string(),
                ))
            }
        }

        let inner = InMemoryProviderUsageRecorder::new();
        let recorder =
            PersistentProviderUsageRecorder::new(inner.clone(), Some(Arc::new(FailingStore)));
        recorder.record_request(sample_event("org_fail"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(recorder.snapshot().total_requests, 1);
        assert!(recorder.persistence_errors() >= 1);
    }

    #[test]
    fn event_without_org_id_is_not_persisted() {
        let inner = InMemoryProviderUsageRecorder::new();
        let store = Arc::new(MockProviderUsageStore::new());
        let recorder = PersistentProviderUsageRecorder::new(inner.clone(), Some(store.clone()));
        let mut event = sample_event("org_x");
        event.org_id = None;
        recorder.record_request(event);
        assert_eq!(recorder.persistence_errors(), 0);
    }
}
