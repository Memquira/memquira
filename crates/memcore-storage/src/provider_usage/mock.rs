use std::collections::BTreeMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ProviderUsageDailyBucket;
use memcore_core::pagination::{PageCursor, build_page, is_after_cursor_in_desc_order};
use memcore_core::ports::{
    ProviderCallStatus, ProviderUsageDailyQuery, ProviderUsageEventRecord,
    ProviderUsagePersistedSummary, ProviderUsageQuery, ProviderUsageQueryResult,
    ProviderUsageStore, validate_provider_usage_limit,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn compute_summary(events: &[ProviderUsageEventRecord]) -> ProviderUsagePersistedSummary {
    let mut summary = ProviderUsagePersistedSummary::default();
    let mut has_cost = false;
    let mut total_cost = 0.0_f64;

    for event in events {
        summary.total_requests = summary.total_requests.saturating_add(1);
        match event.status {
            ProviderCallStatus::Success => {
                summary.total_successes = summary.total_successes.saturating_add(1);
            }
            ProviderCallStatus::Error => {
                summary.total_errors = summary.total_errors.saturating_add(1);
            }
        }
        summary.total_retries = summary.total_retries.saturating_add(event.retry_count);
        if event.fallback_used {
            summary.total_fallbacks = summary.total_fallbacks.saturating_add(1);
        }
        if event.circuit_blocked {
            summary.total_circuit_blocks = summary.total_circuit_blocks.saturating_add(1);
        }
        if event.timed_out {
            summary.total_timeouts = summary.total_timeouts.saturating_add(1);
        }
        summary.total_input_tokens = summary
            .total_input_tokens
            .saturating_add(event.input_tokens.unwrap_or(0));
        summary.total_output_tokens = summary
            .total_output_tokens
            .saturating_add(event.output_tokens.unwrap_or(0));
        summary.total_tokens = summary
            .total_tokens
            .saturating_add(event.total_tokens.unwrap_or(0));
        if let Some(cost) = event.estimated_cost_usd {
            total_cost += cost;
            has_cost = true;
        }
    }

    summary.total_estimated_cost_usd = if has_cost { Some(total_cost) } else { None };
    summary
}

fn matches_query(event: &ProviderUsageEventRecord, query: &ProviderUsageQuery) -> bool {
    if event.org_id != query.org_id {
        return false;
    }
    if let Some(user_id) = &query.user_id
        && event.user_id.as_deref() != Some(user_id.as_str())
    {
        return false;
    }
    if let Some(provider_name) = &query.provider_name
        && &event.provider_name != provider_name
    {
        return false;
    }
    if let Some(model_name) = &query.model_name
        && event.model_name.as_deref() != Some(model_name.as_str())
    {
        return false;
    }
    if let Some(capability) = query.capability
        && event.capability != capability
    {
        return false;
    }
    if let Some(operation_name) = &query.operation_name
        && &event.operation_name != operation_name
    {
        return false;
    }
    if let Some(created_after) = query.created_after
        && event.created_at < created_after
    {
        return false;
    }
    if let Some(created_before) = query.created_before
        && event.created_at >= created_before
    {
        return false;
    }
    true
}

fn matches_daily_query(event: &ProviderUsageEventRecord, query: &ProviderUsageDailyQuery) -> bool {
    if event.org_id != query.org_id {
        return false;
    }
    if let Some(provider_name) = &query.provider_name
        && &event.provider_name != provider_name
    {
        return false;
    }
    if let Some(model_name) = &query.model_name
        && event.model_name.as_deref() != Some(model_name.as_str())
    {
        return false;
    }
    if let Some(capability) = query.capability
        && event.capability != capability
    {
        return false;
    }
    event.created_at >= query.created_after && event.created_at < query.created_before
}

fn add_event_to_bucket(bucket: &mut ProviderUsageDailyBucket, event: &ProviderUsageEventRecord) {
    bucket.total_requests = bucket.total_requests.saturating_add(1);
    match event.status {
        ProviderCallStatus::Success => {
            bucket.total_successes = bucket.total_successes.saturating_add(1);
        }
        ProviderCallStatus::Error => {
            bucket.total_errors = bucket.total_errors.saturating_add(1);
        }
    }
    bucket.total_retries = bucket.total_retries.saturating_add(event.retry_count);
    if event.fallback_used {
        bucket.total_fallbacks = bucket.total_fallbacks.saturating_add(1);
    }
    if event.circuit_blocked {
        bucket.total_circuit_blocks = bucket.total_circuit_blocks.saturating_add(1);
    }
    if event.timed_out {
        bucket.total_timeouts = bucket.total_timeouts.saturating_add(1);
    }
    bucket.total_input_tokens = bucket
        .total_input_tokens
        .saturating_add(event.input_tokens.unwrap_or(0));
    bucket.total_output_tokens = bucket
        .total_output_tokens
        .saturating_add(event.output_tokens.unwrap_or(0));
    bucket.total_tokens = bucket
        .total_tokens
        .saturating_add(event.total_tokens.unwrap_or(0));
    if let Some(cost) = event.estimated_cost_usd {
        bucket.total_estimated_cost_usd =
            Some(bucket.total_estimated_cost_usd.unwrap_or(0.0) + cost);
    }
}

#[derive(Debug, Default)]
pub struct MockProviderUsageStore {
    events: RwLock<Vec<ProviderUsageEventRecord>>,
}

impl MockProviderUsageStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ProviderUsageStore for MockProviderUsageStore {
    async fn record_usage_event(&self, event: ProviderUsageEventRecord) -> MemcoreResult<()> {
        self.events
            .write()
            .map_err(|_| storage_error("mock provider usage lock poisoned", "lock"))?
            .push(event);
        Ok(())
    }

    async fn query_usage(
        &self,
        query: ProviderUsageQuery,
    ) -> MemcoreResult<ProviderUsageQueryResult> {
        let limit = validate_provider_usage_limit(query.limit)?;
        let events = self
            .events
            .read()
            .map_err(|_| storage_error("mock provider usage lock poisoned", "lock"))?;

        let matching: Vec<ProviderUsageEventRecord> = events
            .iter()
            .filter(|event| matches_query(event, &query))
            .cloned()
            .collect();

        let summary = compute_summary(&matching);

        let mut sorted = matching;
        sorted.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });

        if let Some(cursor) = &query.cursor {
            sorted.retain(|event| {
                is_after_cursor_in_desc_order(event.created_at, &event.id.to_string(), cursor)
            });
        }

        let page = build_page(sorted, limit, |event| PageCursor {
            last_id: event.id.to_string(),
            last_sort_value: event.created_at,
        })?;

        Ok(ProviderUsageQueryResult {
            events: page.items,
            next_cursor: page.next_cursor,
            summary,
        })
    }

    async fn query_usage_daily(
        &self,
        query: ProviderUsageDailyQuery,
    ) -> MemcoreResult<Vec<ProviderUsageDailyBucket>> {
        let events = self
            .events
            .read()
            .map_err(|_| storage_error("mock provider usage lock poisoned", "lock"))?;
        let mut buckets: BTreeMap<chrono::NaiveDate, ProviderUsageDailyBucket> = BTreeMap::new();

        for event in events
            .iter()
            .filter(|event| matches_daily_query(event, &query))
        {
            let date = event.created_at.date_naive();
            let bucket = buckets
                .entry(date)
                .or_insert_with(|| ProviderUsageDailyBucket::empty(date));
            add_event_to_bucket(bucket, event);
        }

        Ok(buckets.into_values().collect())
    }

    async fn delete_usage_events_older_than(
        &self,
        org_id: &str,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<usize> {
        let mut events = self
            .events
            .write()
            .map_err(|_| storage_error("mock provider usage lock poisoned", "lock"))?;

        let matching_indices: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.org_id == org_id && event.created_at < cutoff)
            .map(|(index, _)| index)
            .collect();

        let count = matching_indices.len();

        if !dry_run {
            for index in matching_indices.into_iter().rev() {
                events.remove(index);
            }
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use memcore_core::ports::{ProviderUsageCapability, ProviderUsageQuery};
    use uuid::Uuid;

    fn sample_event(
        org_id: &str,
        user_id: Option<&str>,
        provider_name: &str,
        capability: ProviderUsageCapability,
        created_at: DateTime<Utc>,
        status: ProviderCallStatus,
    ) -> ProviderUsageEventRecord {
        ProviderUsageEventRecord {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            user_id: user_id.map(str::to_string),
            provider_name: provider_name.to_string(),
            model_name: Some("mock-llm".to_string()),
            capability,
            operation_name: "llm_extract_facts".to_string(),
            status,
            input_tokens: Some(100),
            output_tokens: Some(20),
            total_tokens: Some(120),
            retry_count: 1,
            fallback_used: false,
            circuit_blocked: false,
            timed_out: false,
            estimated_cost_usd: Some(0.001),
            metadata: None,
            created_at,
        }
    }

    #[tokio::test]
    async fn records_and_queries_by_org_id() {
        let store = MockProviderUsageStore::new();
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        store
            .record_usage_event(sample_event(
                "org_a",
                Some("user_a"),
                "mock",
                ProviderUsageCapability::Llm,
                ts,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");

        let result = store
            .query_usage(ProviderUsageQuery::new("org_a", 10))
            .await
            .expect("query");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.summary.total_requests, 1);
    }

    #[tokio::test]
    async fn excludes_other_org() {
        let store = MockProviderUsageStore::new();
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        store
            .record_usage_event(sample_event(
                "org_a",
                None,
                "mock",
                ProviderUsageCapability::Llm,
                ts,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event(
                "org_b",
                None,
                "mock",
                ProviderUsageCapability::Llm,
                ts,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");

        let result = store
            .query_usage(ProviderUsageQuery::new("org_a", 10))
            .await
            .expect("query");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn filters_by_user_provider_model_capability_operation_and_dates() {
        let store = MockProviderUsageStore::new();
        let early = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mid = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();

        for (user, ts) in [("user_a", early), ("user_b", mid), ("user_a", late)] {
            let mut event = sample_event(
                "org_filter",
                Some(user),
                "mock",
                ProviderUsageCapability::Llm,
                ts,
                ProviderCallStatus::Success,
            );
            event.operation_name = if user == "user_b" {
                "embedding_embed_text".to_string()
            } else {
                "llm_extract_facts".to_string()
            };
            event.capability = if user == "user_b" {
                ProviderUsageCapability::Embedding
            } else {
                ProviderUsageCapability::Llm
            };
            store.record_usage_event(event).await.expect("record");
        }

        let user_filtered = store
            .query_usage(ProviderUsageQuery {
                user_id: Some("user_a".to_string()),
                ..ProviderUsageQuery::new("org_filter", 10)
            })
            .await
            .expect("query");
        assert_eq!(user_filtered.events.len(), 2);

        let capability_filtered = store
            .query_usage(ProviderUsageQuery {
                capability: Some(ProviderUsageCapability::Embedding),
                ..ProviderUsageQuery::new("org_filter", 10)
            })
            .await
            .expect("query");
        assert_eq!(capability_filtered.events.len(), 1);

        let date_filtered = store
            .query_usage(ProviderUsageQuery {
                created_after: Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()),
                created_before: Some(Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap()),
                ..ProviderUsageQuery::new("org_filter", 10)
            })
            .await
            .expect("query");
        assert_eq!(date_filtered.events.len(), 1);
        assert_eq!(date_filtered.events[0].user_id.as_deref(), Some("user_b"));
    }

    #[tokio::test]
    async fn summary_totals_and_pagination_work() {
        let store = MockProviderUsageStore::new();
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        for offset in 0..3 {
            store
                .record_usage_event(sample_event(
                    "org_page",
                    Some("user_a"),
                    "mock",
                    ProviderUsageCapability::Llm,
                    base + chrono::Duration::seconds(offset),
                    ProviderCallStatus::Success,
                ))
                .await
                .expect("record");
        }

        let page1 = store
            .query_usage(ProviderUsageQuery::new("org_page", 2))
            .await
            .expect("page1");
        assert_eq!(page1.events.len(), 2);
        assert!(page1.next_cursor.is_some());
        assert_eq!(page1.summary.total_requests, 3);

        let page2 = store
            .query_usage(ProviderUsageQuery {
                cursor: page1
                    .next_cursor
                    .and_then(|cursor| memcore_core::decode_cursor(&cursor).ok()),
                limit: 2,
                ..ProviderUsageQuery::new("org_page", 2)
            })
            .await
            .expect("page2");
        assert_eq!(page2.events.len(), 1);
    }

    #[tokio::test]
    async fn dry_run_counts_old_events_without_deleting() {
        let store = MockProviderUsageStore::new();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let cutoff = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        store
            .record_usage_event(sample_event(
                "org_a",
                Some("user_a"),
                "mock",
                ProviderUsageCapability::Llm,
                old,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event(
                "org_a",
                Some("user_b"),
                "mock",
                ProviderUsageCapability::Llm,
                recent,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");

        let matched = store
            .delete_usage_events_older_than("org_a", cutoff, true)
            .await
            .expect("dry-run");
        assert_eq!(matched, 1);

        let result = store
            .query_usage(ProviderUsageQuery::new("org_a", 10))
            .await
            .expect("query");
        assert_eq!(result.events.len(), 2);
    }

    #[tokio::test]
    async fn non_dry_run_deletes_old_events_preserves_new_and_other_orgs() {
        let store = MockProviderUsageStore::new();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let cutoff = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        store
            .record_usage_event(sample_event(
                "org_a",
                Some("user_a"),
                "mock",
                ProviderUsageCapability::Llm,
                old,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event(
                "org_a",
                Some("user_b"),
                "mock",
                ProviderUsageCapability::Llm,
                recent,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");
        store
            .record_usage_event(sample_event(
                "org_b",
                None,
                "mock",
                ProviderUsageCapability::Llm,
                old,
                ProviderCallStatus::Success,
            ))
            .await
            .expect("record");

        let deleted = store
            .delete_usage_events_older_than("org_a", cutoff, false)
            .await
            .expect("delete");
        assert_eq!(deleted, 1);

        let org_a = store
            .query_usage(ProviderUsageQuery::new("org_a", 10))
            .await
            .expect("org_a");
        assert_eq!(org_a.events.len(), 1);
        assert_eq!(org_a.summary.total_requests, 1);

        let org_b = store
            .query_usage(ProviderUsageQuery::new("org_b", 10))
            .await
            .expect("org_b");
        assert_eq!(org_b.events.len(), 1);
    }

    #[tokio::test]
    async fn cutoff_uses_created_at() {
        let store = MockProviderUsageStore::new();
        let before = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let at_cutoff = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap();

        for ts in [before, at_cutoff, after] {
            store
                .record_usage_event(sample_event(
                    "org_cutoff",
                    None,
                    "mock",
                    ProviderUsageCapability::Llm,
                    ts,
                    ProviderCallStatus::Success,
                ))
                .await
                .expect("record");
        }

        let matched = store
            .delete_usage_events_older_than("org_cutoff", at_cutoff, true)
            .await
            .expect("count");
        assert_eq!(matched, 1);
    }
}
