use std::str::FromStr;

use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::quota::OrgQuotaLimits;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrgPlanTier {
    Free,
    Starter,
    Pro,
    Enterprise,
    Custom,
}

impl OrgPlanTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Starter => "Starter",
            Self::Pro => "Pro",
            Self::Enterprise => "Enterprise",
            Self::Custom => "Custom",
        }
    }

    pub fn storage_label(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Starter => "starter",
            Self::Pro => "pro",
            Self::Enterprise => "enterprise",
            Self::Custom => "custom",
        }
    }
}

impl FromStr for OrgPlanTier {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "free" => Ok(Self::Free),
            "starter" => Ok(Self::Starter),
            "pro" => Ok(Self::Pro),
            "enterprise" => Ok(Self::Enterprise),
            "custom" => Ok(Self::Custom),
            _ => Err(MemcoreError::ValidationError(format!(
                "invalid org plan tier: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgPlanLimits {
    pub max_users_per_org: Option<u64>,
    pub max_memories_per_user: Option<u64>,
    pub max_memories_per_org: Option<u64>,
    pub daily_provider_request_limit: Option<u64>,
    pub daily_provider_token_limit: Option<u64>,
}

impl OrgPlanLimits {
    pub fn from_raw(
        max_users_per_org: u64,
        max_memories_per_user: u64,
        max_memories_per_org: u64,
        daily_provider_request_limit: u64,
        daily_provider_token_limit: u64,
    ) -> Self {
        Self {
            max_users_per_org: non_zero_limit(max_users_per_org),
            max_memories_per_user: non_zero_limit(max_memories_per_user),
            max_memories_per_org: non_zero_limit(max_memories_per_org),
            daily_provider_request_limit: non_zero_limit(daily_provider_request_limit),
            daily_provider_token_limit: non_zero_limit(daily_provider_token_limit),
        }
    }

    pub fn to_quota_limits(&self, enabled: bool) -> OrgQuotaLimits {
        OrgQuotaLimits {
            enabled,
            max_users_per_org: self.max_users_per_org,
            max_memories_per_user: self.max_memories_per_user,
            max_memories_per_org: self.max_memories_per_org,
            daily_provider_request_limit: self.daily_provider_request_limit,
            daily_provider_token_limit: self.daily_provider_token_limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgPlanConfig {
    pub org_id: String,
    pub tier: OrgPlanTier,
    pub limits: OrgPlanLimits,
    pub is_active: bool,
    pub metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrgPlanConfig {
    pub fn validate(&self) -> MemcoreResult<()> {
        if self.org_id.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "org_id cannot be empty".to_string(),
            ));
        }
        validate_org_plan_metadata(self.metadata.as_ref())
    }
}

pub fn validate_org_plan_metadata(metadata: Option<&Value>) -> MemcoreResult<()> {
    if let Some(value) = metadata {
        validate_metadata_value(value)?;
    }
    Ok(())
}

fn validate_metadata_value(value: &Value) -> MemcoreResult<()> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let lower = key.to_ascii_lowercase();
                if lower.contains("secret")
                    || lower.contains("password")
                    || lower.contains("token")
                    || lower.contains("api_key")
                    || lower.contains("apikey")
                    || lower.contains("bearer")
                    || lower.contains("stripe")
                    || lower.contains("payment")
                    || lower.contains("customer_id")
                {
                    return Err(MemcoreError::ValidationError(format!(
                        "plan metadata contains a forbidden key: {key}"
                    )));
                }
                validate_metadata_value(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_metadata_value(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn non_zero_limit(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tier_parses_valid_values() {
        assert_eq!("pro".parse::<OrgPlanTier>().unwrap(), OrgPlanTier::Pro);
        assert_eq!(
            "Enterprise".parse::<OrgPlanTier>().unwrap(),
            OrgPlanTier::Enterprise
        );
    }

    #[test]
    fn invalid_tier_returns_validation_error() {
        let error = "gold".parse::<OrgPlanTier>().unwrap_err();
        assert_eq!(error.code(), "validation_error");
    }

    #[test]
    fn zero_limit_normalizes_to_unlimited() {
        let limits = OrgPlanLimits::from_raw(0, 1, 0, 2, 0);
        assert_eq!(limits.max_users_per_org, None);
        assert_eq!(limits.max_memories_per_user, Some(1));
        assert_eq!(limits.max_memories_per_org, None);
        assert_eq!(limits.daily_provider_request_limit, Some(2));
        assert_eq!(limits.daily_provider_token_limit, None);
    }

    #[test]
    fn metadata_rejects_obvious_secret_keys() {
        let error = validate_org_plan_metadata(Some(&json!({
            "nested": { "api_key": "secret" }
        })))
        .unwrap_err();
        assert_eq!(error.code(), "validation_error");
    }
}
