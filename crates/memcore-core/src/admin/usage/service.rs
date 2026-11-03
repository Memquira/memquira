use chrono::{DateTime, Duration, Utc};
use memcore_common::{MemcoreError, MemcoreResult};

use crate::{ProviderUsagePersistedSummary, validate_event_date_range};

use super::types::{
    DEFAULT_MEMORY_USAGE_SNAPSHOT_LIMIT, DEFAULT_ORG_USAGE_DASHBOARD_DAYS,
    MAX_MEMORY_USAGE_SNAPSHOT_LIMIT, MAX_ORG_USAGE_DASHBOARD_DAYS, ProviderUsageDashboardSummary,
};

pub fn resolve_org_usage_window(
    created_after: Option<DateTime<Utc>>,
    created_before: Option<DateTime<Utc>>,
    days: Option<u32>,
    now: DateTime<Utc>,
) -> MemcoreResult<(DateTime<Utc>, DateTime<Utc>)> {
    match (created_after, created_before) {
        (Some(after), Some(before)) => {
            validate_event_date_range(Some(after), Some(before))?;
            Ok((after, before))
        }
        (Some(_), None) | (None, Some(_)) => Err(MemcoreError::ValidationError(
            "created_after and created_before must be provided together".to_string(),
        )),
        (None, None) => {
            let days = validate_org_usage_days(days)?;
            Ok((now - Duration::days(i64::from(days)), now))
        }
    }
}

pub fn validate_org_usage_days(days: Option<u32>) -> MemcoreResult<u32> {
    let days = days.unwrap_or(DEFAULT_ORG_USAGE_DASHBOARD_DAYS);
    if !(1..=MAX_ORG_USAGE_DASHBOARD_DAYS).contains(&days) {
        return Err(MemcoreError::ValidationError(
            "days must be between 1 and 90".to_string(),
        ));
    }
    Ok(days)
}

pub fn validate_memory_usage_snapshot_limit(limit: usize) -> MemcoreResult<usize> {
    if limit == 0 {
        return Ok(DEFAULT_MEMORY_USAGE_SNAPSHOT_LIMIT);
    }

    if limit > MAX_MEMORY_USAGE_SNAPSHOT_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_MEMORY_USAGE_SNAPSHOT_LIMIT}"
        )));
    }

    Ok(limit)
}

impl From<ProviderUsagePersistedSummary> for ProviderUsageDashboardSummary {
    fn from(summary: ProviderUsagePersistedSummary) -> Self {
        Self {
            total_requests: summary.total_requests,
            total_successes: summary.total_successes,
            total_errors: summary.total_errors,
            total_retries: summary.total_retries,
            total_fallbacks: summary.total_fallbacks,
            total_circuit_blocks: summary.total_circuit_blocks,
            total_timeouts: summary.total_timeouts,
            total_input_tokens: summary.total_input_tokens,
            total_output_tokens: summary.total_output_tokens,
            total_tokens: summary.total_tokens,
            total_estimated_cost_usd: summary.total_estimated_cost_usd,
        }
    }
}

pub fn empty_provider_usage_summary() -> ProviderUsageDashboardSummary {
    ProviderUsagePersistedSummary::default().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn default_window_uses_last_30_days() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        let (start, end) = resolve_org_usage_window(None, None, None, now).expect("window");
        assert_eq!(end, now);
        assert_eq!(end - start, Duration::days(30));
    }

    #[test]
    fn custom_days_window_is_supported() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        let (start, end) = resolve_org_usage_window(None, None, Some(7), now).expect("window");
        assert_eq!(end, now);
        assert_eq!(end - start, Duration::days(7));
    }

    #[test]
    fn days_above_max_fails() {
        let now = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        let error = resolve_org_usage_window(None, None, Some(91), now).expect_err("invalid days");
        assert_eq!(
            error,
            MemcoreError::ValidationError("days must be between 1 and 90".to_string())
        );
    }

    #[test]
    fn explicit_invalid_range_fails() {
        let after = Utc.with_ymd_and_hms(2026, 6, 18, 0, 0, 0).unwrap();
        let before = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let error = resolve_org_usage_window(Some(after), Some(before), None, Utc::now())
            .expect_err("range");
        assert_eq!(
            error,
            MemcoreError::ValidationError(
                "created_after must be earlier than created_before".to_string()
            )
        );
    }
}
