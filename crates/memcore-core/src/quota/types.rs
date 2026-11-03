use chrono::{DateTime, Utc};
use memcore_common::MemcoreError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgQuotaLimits {
    pub enabled: bool,
    pub max_users_per_org: Option<u64>,
    pub max_memories_per_user: Option<u64>,
    pub max_memories_per_org: Option<u64>,
    pub daily_provider_request_limit: Option<u64>,
    pub daily_provider_token_limit: Option<u64>,
}

impl OrgQuotaLimits {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            max_users_per_org: None,
            max_memories_per_user: None,
            max_memories_per_org: None,
            daily_provider_request_limit: None,
            daily_provider_token_limit: None,
        }
    }

    pub fn from_raw(
        enabled: bool,
        max_users_per_org: u64,
        max_memories_per_user: u64,
        max_memories_per_org: u64,
        daily_provider_request_limit: u64,
        daily_provider_token_limit: u64,
    ) -> Self {
        Self {
            enabled,
            max_users_per_org: non_zero_limit(max_users_per_org),
            max_memories_per_user: non_zero_limit(max_memories_per_user),
            max_memories_per_org: non_zero_limit(max_memories_per_org),
            daily_provider_request_limit: non_zero_limit(daily_provider_request_limit),
            daily_provider_token_limit: non_zero_limit(daily_provider_token_limit),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgQuotaUsage {
    pub org_id: String,
    pub total_users: u64,
    pub total_memories: u64,
    pub user_memory_count: Option<u64>,
    pub daily_provider_requests: u64,
    pub daily_provider_tokens: u64,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaLimitSource {
    GlobalConfig,
    OrgPlan,
}

impl QuotaLimitSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GlobalConfig => "global_config",
            Self::OrgPlan => "org_plan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedOrgQuotaLimits {
    pub source: QuotaLimitSource,
    pub limits: OrgQuotaLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaLimitKind {
    UsersPerOrg,
    MemoriesPerUser,
    MemoriesPerOrg,
    DailyProviderRequests,
    DailyProviderTokens,
}

impl QuotaLimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UsersPerOrg => "UsersPerOrg",
            Self::MemoriesPerUser => "MemoriesPerUser",
            Self::MemoriesPerOrg => "MemoriesPerOrg",
            Self::DailyProviderRequests => "DailyProviderRequests",
            Self::DailyProviderTokens => "DailyProviderTokens",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaViolation {
    pub kind: QuotaLimitKind,
    pub limit: u64,
    pub current: u64,
    pub requested: u64,
    pub message: String,
}

impl QuotaViolation {
    pub fn new(kind: QuotaLimitKind, limit: u64, current: u64, requested: u64) -> Self {
        Self {
            kind,
            limit,
            current,
            requested,
            message: quota_message(kind),
        }
    }

    pub fn into_error(self) -> MemcoreError {
        MemcoreError::quota_exceeded(
            self.message,
            self.kind.as_str(),
            self.limit,
            self.current,
            self.requested,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaCheckResult {
    pub allowed: bool,
    pub source: QuotaLimitSource,
    pub violations: Vec<QuotaViolation>,
    pub usage: OrgQuotaUsage,
    pub limits: OrgQuotaLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetOrgQuotaStatusInput {
    pub org_id: String,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckMemoryWriteQuotaInput {
    pub org_id: String,
    pub user_id: String,
    pub requested_new_memories: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckProviderQuotaInput {
    pub org_id: String,
    pub requested_tokens: Option<u64>,
}

fn non_zero_limit(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

fn quota_message(kind: QuotaLimitKind) -> String {
    match kind {
        QuotaLimitKind::UsersPerOrg => "organization user limit exceeded",
        QuotaLimitKind::MemoriesPerUser => "user memory limit exceeded",
        QuotaLimitKind::MemoriesPerOrg => "organization memory limit exceeded",
        QuotaLimitKind::DailyProviderRequests => "daily provider request limit exceeded",
        QuotaLimitKind::DailyProviderTokens => "daily provider token limit exceeded",
    }
    .to_string()
}
