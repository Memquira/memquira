use chrono::{DateTime, Utc};
use memcore_config::Settings;
use memcore_core::{ApplyProviderUsageRetentionInput, ApplyProviderUsageRetentionOutput};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

fn default_dry_run_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ApplyProviderUsageRetentionRequest {
    #[serde(default = "default_dry_run_true")]
    pub dry_run: bool,
    #[serde(default)]
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyProviderUsageRetentionResponse {
    pub status: &'static str,
    pub summary: ApplyProviderUsageRetentionSummaryResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyProviderUsageRetentionSummaryResponse {
    pub dry_run: bool,
    pub matched_events: usize,
    pub deleted_events: usize,
    pub cutoff: DateTime<Utc>,
}

impl From<ApplyProviderUsageRetentionOutput> for ApplyProviderUsageRetentionSummaryResponse {
    fn from(output: ApplyProviderUsageRetentionOutput) -> Self {
        Self {
            dry_run: output.dry_run,
            matched_events: output.matched_events,
            deleted_events: output.deleted_events,
            cutoff: output.cutoff,
        }
    }
}

impl From<ApplyProviderUsageRetentionOutput> for ApplyProviderUsageRetentionResponse {
    fn from(output: ApplyProviderUsageRetentionOutput) -> Self {
        Self {
            status: "success",
            summary: ApplyProviderUsageRetentionSummaryResponse::from(output),
        }
    }
}

pub fn resolve_provider_usage_retention_days(override_days: Option<u32>, default_days: u32) -> u32 {
    override_days.unwrap_or(default_days)
}

impl ApplyProviderUsageRetentionRequest {
    pub fn into_input(
        self,
        org_id: String,
        settings: &Settings,
    ) -> ApplyProviderUsageRetentionInput {
        ApplyProviderUsageRetentionInput {
            org_id,
            retention_days: resolve_provider_usage_retention_days(
                self.retention_days,
                settings.provider_usage_retention_days,
            ),
            dry_run: self.dry_run,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore_config::Settings;

    #[test]
    fn dry_run_defaults_to_true() {
        let request: ApplyProviderUsageRetentionRequest =
            serde_json::from_str(r#"{}"#).expect("deserialize");
        assert!(request.dry_run);
    }

    #[test]
    fn omitted_retention_days_uses_config_default() {
        let settings = Settings::default();
        let request = ApplyProviderUsageRetentionRequest {
            dry_run: true,
            retention_days: None,
        };
        let input = request.into_input("org_a".to_string(), &settings);
        assert_eq!(input.retention_days, 180);
    }

    #[test]
    fn zero_retention_days_disables_cleanup() {
        let settings = Settings::default();
        let request = ApplyProviderUsageRetentionRequest {
            dry_run: false,
            retention_days: Some(0),
        };
        let input = request.into_input("org_a".to_string(), &settings);
        assert_eq!(input.retention_days, 0);
    }
}
