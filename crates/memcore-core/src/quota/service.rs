use std::sync::Arc;

use chrono::{Duration, Utc};
use memcore_common::MemcoreResult;

use crate::ports::{
    DEFAULT_PROVIDER_USAGE_LIMIT, FactStore, OrgPlanStore, ProviderUsageQuery, ProviderUsageStore,
};
use crate::{TenantContext, validate_event_date_range};

use super::types::{
    CheckMemoryWriteQuotaInput, CheckProviderQuotaInput, GetOrgQuotaStatusInput, OrgQuotaLimits,
    OrgQuotaUsage, QuotaCheckResult, QuotaLimitKind, QuotaLimitSource, QuotaViolation,
    ResolvedOrgQuotaLimits,
};

#[derive(Clone)]
pub struct QuotaService {
    fact_store: Arc<dyn FactStore>,
    provider_usage_store: Option<Arc<dyn ProviderUsageStore>>,
    org_plan_store: Option<Arc<dyn OrgPlanStore>>,
    global_limits: OrgQuotaLimits,
}

impl QuotaService {
    pub fn new(
        fact_store: Arc<dyn FactStore>,
        provider_usage_store: Option<Arc<dyn ProviderUsageStore>>,
        org_plan_store: Option<Arc<dyn OrgPlanStore>>,
        global_limits: OrgQuotaLimits,
    ) -> Self {
        Self {
            fact_store,
            provider_usage_store,
            org_plan_store,
            global_limits,
        }
    }

    pub async fn resolve_org_quota_limits(
        &self,
        org_id: &str,
    ) -> MemcoreResult<ResolvedOrgQuotaLimits> {
        if let Some(store) = &self.org_plan_store
            && let Some(plan) = store.get_org_plan(org_id).await?
            && plan.is_active
        {
            return Ok(ResolvedOrgQuotaLimits {
                source: QuotaLimitSource::OrgPlan,
                limits: plan.limits.to_quota_limits(self.global_limits.enabled),
            });
        }

        Ok(ResolvedOrgQuotaLimits {
            source: QuotaLimitSource::GlobalConfig,
            limits: self.global_limits.clone(),
        })
    }

    pub async fn get_org_quota_status(
        &self,
        input: GetOrgQuotaStatusInput,
    ) -> MemcoreResult<QuotaCheckResult> {
        let resolved = self.resolve_org_quota_limits(&input.org_id).await?;
        let usage = self
            .collect_usage(&input.org_id, input.user_id.as_deref())
            .await?;
        let violations = if resolved.limits.enabled {
            self.evaluate_limits(&resolved.limits, &usage, 0, 0)
        } else {
            Vec::new()
        };

        Ok(QuotaCheckResult {
            allowed: violations.is_empty(),
            source: resolved.source,
            violations,
            usage,
            limits: resolved.limits,
        })
    }

    pub async fn check_memory_write_allowed(
        &self,
        input: CheckMemoryWriteQuotaInput,
    ) -> MemcoreResult<QuotaCheckResult> {
        let resolved = self.resolve_org_quota_limits(&input.org_id).await?;
        let usage = self
            .collect_usage(&input.org_id, Some(input.user_id.as_str()))
            .await?;
        let violations = if resolved.limits.enabled {
            self.evaluate_memory_limits(&resolved.limits, &usage, input.requested_new_memories)
        } else {
            Vec::new()
        };

        Ok(QuotaCheckResult {
            allowed: violations.is_empty(),
            source: resolved.source,
            violations,
            usage,
            limits: resolved.limits,
        })
    }

    pub async fn check_provider_call_allowed(
        &self,
        input: CheckProviderQuotaInput,
    ) -> MemcoreResult<QuotaCheckResult> {
        let resolved = self.resolve_org_quota_limits(&input.org_id).await?;
        let requested_tokens = input.requested_tokens.unwrap_or(0);
        let usage = self.collect_usage(&input.org_id, None).await?;
        let violations = if resolved.limits.enabled {
            self.evaluate_provider_limits(&resolved.limits, &usage, 1, requested_tokens)
        } else {
            Vec::new()
        };

        Ok(QuotaCheckResult {
            allowed: violations.is_empty(),
            source: resolved.source,
            violations,
            usage,
            limits: resolved.limits,
        })
    }

    async fn collect_usage(
        &self,
        org_id: &str,
        user_id: Option<&str>,
    ) -> MemcoreResult<OrgQuotaUsage> {
        let (window_start, window_end) = utc_day_window();
        validate_event_date_range(Some(window_start), Some(window_end))?;

        let total_users = self.fact_store.count_users_by_org(org_id).await? as u64;
        let total_memories = self.fact_store.count_facts_by_org(org_id).await? as u64;
        let user_memory_count = match user_id {
            Some(user_id) => {
                let tenant = TenantContext::new(org_id, user_id)?;
                Some(self.fact_store.count_facts_by_user(&tenant).await? as u64)
            }
            None => None,
        };

        let (daily_provider_requests, daily_provider_tokens) = self
            .collect_daily_provider_usage(org_id, window_start, window_end)
            .await?;

        Ok(OrgQuotaUsage {
            org_id: org_id.to_string(),
            total_users,
            total_memories,
            user_memory_count,
            daily_provider_requests,
            daily_provider_tokens,
            window_start,
            window_end,
        })
    }

    async fn collect_daily_provider_usage(
        &self,
        org_id: &str,
        window_start: chrono::DateTime<Utc>,
        window_end: chrono::DateTime<Utc>,
    ) -> MemcoreResult<(u64, u64)> {
        let Some(store) = &self.provider_usage_store else {
            tracing::warn!(
                org_id,
                "provider quota status requested without provider usage store; provider usage is reported as zero"
            );
            return Ok((0, 0));
        };

        let mut query = ProviderUsageQuery::new(org_id, DEFAULT_PROVIDER_USAGE_LIMIT);
        query.created_after = Some(window_start);
        query.created_before = Some(window_end);
        let result = store.query_usage(query).await?;
        Ok((result.summary.total_requests, result.summary.total_tokens))
    }

    fn evaluate_limits(
        &self,
        limits: &super::types::OrgQuotaLimits,
        usage: &OrgQuotaUsage,
        requested_new_memories: u64,
        requested_provider_tokens: u64,
    ) -> Vec<QuotaViolation> {
        let mut violations = self.evaluate_memory_limits(limits, usage, requested_new_memories);
        violations.extend(self.evaluate_provider_limits(
            limits,
            usage,
            0,
            requested_provider_tokens,
        ));
        violations
    }

    fn evaluate_memory_limits(
        &self,
        limits: &super::types::OrgQuotaLimits,
        usage: &OrgQuotaUsage,
        requested_new_memories: u64,
    ) -> Vec<QuotaViolation> {
        let mut violations = Vec::new();

        if let Some(limit) = limits.max_memories_per_org
            && usage.total_memories.saturating_add(requested_new_memories) > limit
        {
            violations.push(QuotaViolation::new(
                QuotaLimitKind::MemoriesPerOrg,
                limit,
                usage.total_memories,
                requested_new_memories,
            ));
        }

        if let (Some(limit), Some(current)) =
            (limits.max_memories_per_user, usage.user_memory_count)
            && current.saturating_add(requested_new_memories) > limit
        {
            violations.push(QuotaViolation::new(
                QuotaLimitKind::MemoriesPerUser,
                limit,
                current,
                requested_new_memories,
            ));
        }

        if let (Some(limit), Some(user_memory_count)) =
            (limits.max_users_per_org, usage.user_memory_count)
        {
            let requested_users = if user_memory_count == 0 { 1 } else { 0 };
            if usage.total_users.saturating_add(requested_users) > limit {
                violations.push(QuotaViolation::new(
                    QuotaLimitKind::UsersPerOrg,
                    limit,
                    usage.total_users,
                    requested_users,
                ));
            }
        }

        violations
    }

    fn evaluate_provider_limits(
        &self,
        limits: &super::types::OrgQuotaLimits,
        usage: &OrgQuotaUsage,
        requested_requests: u64,
        requested_tokens: u64,
    ) -> Vec<QuotaViolation> {
        let mut violations = Vec::new();

        if let Some(limit) = limits.daily_provider_request_limit
            && usage
                .daily_provider_requests
                .saturating_add(requested_requests)
                > limit
        {
            violations.push(QuotaViolation::new(
                QuotaLimitKind::DailyProviderRequests,
                limit,
                usage.daily_provider_requests,
                requested_requests,
            ));
        }

        if let Some(limit) = limits.daily_provider_token_limit
            && usage.daily_provider_tokens.saturating_add(requested_tokens) > limit
        {
            violations.push(QuotaViolation::new(
                QuotaLimitKind::DailyProviderTokens,
                limit,
                usage.daily_provider_tokens,
                requested_tokens,
            ));
        }

        violations
    }
}

pub fn utc_day_window() -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let start = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid UTC time")
        .and_utc();
    (start, start + Duration::days(1))
}
