use memcore_config::Settings;
use memcore_core::{ApplyRetentionInput, ApplyRetentionOutput, RetentionPolicy, TenantContext};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

fn default_dry_run_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ApplyRetentionRequest {
    #[serde(default = "default_dry_run_true")]
    pub dry_run: bool,
    #[serde(default)]
    pub fact_retention_days: Option<u32>,
    #[serde(default)]
    pub event_retention_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyRetentionResponse {
    pub status: &'static str,
    pub summary: ApplyRetentionSummaryResponse,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyRetentionSummaryResponse {
    pub dry_run: bool,
    pub facts_matched: usize,
    pub facts_deleted: usize,
    pub events_matched: usize,
    pub events_deleted: usize,
    /// Present only when episodes are enabled; episodes follow the fact retention window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes_matched: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes_deleted: Option<usize>,
}

impl From<ApplyRetentionOutput> for ApplyRetentionSummaryResponse {
    fn from(output: ApplyRetentionOutput) -> Self {
        Self {
            dry_run: output.dry_run,
            facts_matched: output.facts_matched,
            facts_deleted: output.facts_deleted,
            events_matched: output.events_matched,
            events_deleted: output.events_deleted,
            episodes_matched: output.episodes_matched,
            episodes_deleted: output.episodes_deleted,
        }
    }
}

impl From<ApplyRetentionOutput> for ApplyRetentionResponse {
    fn from(output: ApplyRetentionOutput) -> Self {
        Self {
            status: "success",
            summary: ApplyRetentionSummaryResponse::from(output),
        }
    }
}

fn resolve_retention_days(override_days: Option<u32>, default_days: u32) -> Option<u32> {
    match override_days {
        Some(0) => None,
        Some(days) => Some(days),
        None => {
            if default_days == 0 {
                None
            } else {
                Some(default_days)
            }
        }
    }
}

pub fn retention_policy_from_settings(
    settings: &Settings,
    request: &ApplyRetentionRequest,
) -> RetentionPolicy {
    RetentionPolicy {
        enabled: settings.retention_enabled,
        fact_retention_days: resolve_retention_days(
            request.fact_retention_days,
            settings.fact_retention_days,
        ),
        event_retention_days: resolve_retention_days(
            request.event_retention_days,
            settings.event_retention_days,
        ),
    }
}

impl ApplyRetentionRequest {
    pub fn into_input(self, tenant: TenantContext, settings: &Settings) -> ApplyRetentionInput {
        ApplyRetentionInput {
            tenant,
            policy: retention_policy_from_settings(settings, &self),
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
        let json = r#"{}"#;
        let request: ApplyRetentionRequest =
            serde_json::from_str(json).expect("deserialize retention request");
        assert!(request.dry_run);
    }

    #[test]
    fn zero_days_disables_category() {
        let settings = Settings {
            retention_enabled: true,
            fact_retention_days: 365,
            event_retention_days: 90,
            ..Settings::default()
        };
        let request = ApplyRetentionRequest {
            dry_run: true,
            fact_retention_days: Some(0),
            event_retention_days: None,
        };
        let policy = retention_policy_from_settings(&settings, &request);
        assert!(policy.fact_retention_days.is_none());
        assert_eq!(policy.event_retention_days, Some(90));
    }
}
