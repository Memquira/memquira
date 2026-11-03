//! Regression test for the PostgreSQL provider-usage summary (skipped without MEMCORE_TEST_POSTGRES_URL).
//!
//! The summary summed BIGINT columns without a cast; PostgreSQL returns NUMERIC for those aggregates and
//! decoding them as i64 failed, so `query_usage` (and every write with usage persistence and quotas on)
//! answered STORAGE_ERROR. Invented data only.

use chrono::{Duration, Utc};
use memcore_core::{
    ProviderCallStatus, ProviderUsageCapability, ProviderUsageDailyQuery, ProviderUsageEventRecord,
    ProviderUsageQuery,
};
use memcore_storage::{PostgresProviderUsageStore, ProviderUsageStore};
use uuid::Uuid;

fn postgres_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn event(
    org: &str,
    status: ProviderCallStatus,
    input: u64,
    output: u64,
    retries: u64,
    timed_out: bool,
) -> ProviderUsageEventRecord {
    ProviderUsageEventRecord {
        id: Uuid::new_v4(),
        org_id: org.to_string(),
        user_id: Some("user-1".to_string()),
        provider_name: "openai".to_string(),
        model_name: Some("gpt-4o-mini".to_string()),
        capability: ProviderUsageCapability::Llm,
        operation_name: "extract_facts".to_string(),
        status,
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
        retry_count: retries,
        fallback_used: false,
        circuit_blocked: false,
        timed_out,
        estimated_cost_usd: Some(0.001),
        metadata: None,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn postgres_provider_usage_summary_decodes_aggregates() {
    let Some(url) = postgres_url() else {
        eprintln!("skipping postgres provider usage summary: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    let store = PostgresProviderUsageStore::connect(&url)
        .await
        .expect("postgres provider usage store should connect");
    let org = format!("org_usage_summary_{}", Uuid::new_v4().simple());

    // Empty organization: every aggregate is zero (COUNT and COALESCE paths).
    let empty = store
        .query_usage(ProviderUsageQuery::new(org.clone(), 10))
        .await
        .expect("empty summary should decode");
    assert_eq!(empty.summary.total_requests, 0);
    assert_eq!(empty.summary.total_tokens, 0);

    for e in [
        event(&org, ProviderCallStatus::Success, 1_000, 200, 0, false),
        event(
            &org,
            ProviderCallStatus::Success,
            3_000_000_000,
            400,
            2,
            false,
        ),
        event(&org, ProviderCallStatus::Error, 500, 0, 1, true),
    ] {
        store
            .record_usage_event(e)
            .await
            .expect("record usage event");
    }

    let result = store
        .query_usage(ProviderUsageQuery::new(org.clone(), 10))
        .await
        .expect("summary with BIGINT sums should decode");
    let s = result.summary;
    assert_eq!(s.total_requests, 3);
    assert_eq!(s.total_successes, 2);
    assert_eq!(s.total_errors, 1);
    assert_eq!(s.total_retries, 3);
    assert_eq!(s.total_timeouts, 1);
    // Larger than i32: the sums really are BIGINT-sized.
    assert_eq!(s.total_input_tokens, 3_000_001_500);
    assert_eq!(s.total_output_tokens, 600);
    assert_eq!(s.total_tokens, 3_000_002_100);
    let cost = s.total_estimated_cost_usd.expect("estimated cost sum");
    assert!((cost - 0.003).abs() < 1e-9, "cost {cost}");
    assert_eq!(result.events.len(), 3);

    // The daily view agrees with the summary.
    let now = Utc::now();
    let daily = store
        .query_usage_daily(ProviderUsageDailyQuery {
            org_id: org.clone(),
            provider_name: None,
            model_name: None,
            capability: Some(ProviderUsageCapability::Llm),
            created_after: now - Duration::days(1),
            created_before: now + Duration::days(1),
        })
        .await
        .expect("daily usage should decode");
    let input: u64 = daily.iter().map(|b| b.total_input_tokens).sum();
    assert_eq!(input, 3_000_001_500);
}
