use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::TenantContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub enabled: bool,
    pub fact_retention_days: Option<u32>,
    pub event_retention_days: Option<u32>,
}

impl RetentionPolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            fact_retention_days: None,
            event_retention_days: None,
        }
    }

    pub fn fact_days_active(&self) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        self.fact_retention_days.filter(|days| *days > 0)
    }

    pub fn event_days_active(&self) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        self.event_retention_days.filter(|days| *days > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyRetentionInput {
    pub tenant: TenantContext,
    pub policy: RetentionPolicy,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyRetentionOutput {
    pub dry_run: bool,
    pub facts_matched: usize,
    pub facts_deleted: usize,
    pub events_matched: usize,
    pub events_deleted: usize,
    /// Episodes past the fact retention window (by `recorded_at`); `None` when episodes are
    /// disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes_matched: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes_deleted: Option<usize>,
}

impl ApplyRetentionOutput {
    pub fn zero(dry_run: bool) -> Self {
        Self {
            dry_run,
            facts_matched: 0,
            facts_deleted: 0,
            events_matched: 0,
            events_deleted: 0,
            episodes_matched: None,
            episodes_deleted: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyProviderUsageRetentionInput {
    pub org_id: String,
    pub retention_days: u32,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyProviderUsageRetentionOutput {
    pub dry_run: bool,
    pub matched_events: usize,
    pub deleted_events: usize,
    pub cutoff: DateTime<Utc>,
}

impl ApplyProviderUsageRetentionOutput {
    pub fn zero(dry_run: bool, cutoff: DateTime<Utc>) -> Self {
        Self {
            dry_run,
            matched_events: 0,
            deleted_events: 0,
            cutoff,
        }
    }
}
