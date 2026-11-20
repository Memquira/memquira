use serde::{Deserialize, Serialize};

use crate::ports::OrgUserSummary;

/// Default page size for listing organization users.
pub const DEFAULT_LIST_ORG_USERS_LIMIT: usize = 50;

/// Maximum page size for listing organization users.
pub const MAX_LIST_ORG_USERS_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgSummaryInput {
    pub org_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgSummaryOutput {
    pub org_id: String,
    pub total_users: usize,
    pub total_facts: usize,
    pub total_events: Option<usize>,
    /// Episodes, counted separately from facts; `None` when episodes are disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_episodes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListOrgUsersInput {
    pub org_id: String,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListOrgUsersOutput {
    pub users: Vec<OrgUserSummary>,
    pub next_cursor: Option<String>,
}

/// Default page size for admin org memory event search.
pub const DEFAULT_SEARCH_ORG_MEMORY_EVENTS_LIMIT: usize = 50;

/// Maximum page size for admin org memory event search.
pub const MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchOrgMemoryEventsInput {
    pub org_id: String,
    pub user_id: Option<String>,
    pub fact_id: Option<uuid::Uuid>,
    pub operation: Option<crate::MemoryEventOperation>,
    pub created_after: Option<chrono::DateTime<chrono::Utc>>,
    pub created_before: Option<chrono::DateTime<chrono::Utc>>,
    pub query_text: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchOrgMemoryEventsOutput {
    pub events: Vec<crate::MemoryEvent>,
    pub next_cursor: Option<String>,
}
