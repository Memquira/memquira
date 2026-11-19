use memquira_common::MemquiraResult;
use serde::{Deserialize, Serialize};

use super::validation::validate_non_empty;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantContext {
    pub org_id: String,
    pub user_id: String,
}

impl TenantContext {
    pub fn new(org_id: impl Into<String>, user_id: impl Into<String>) -> MemquiraResult<Self> {
        let org_id = org_id.into();
        let user_id = user_id.into();
        validate_non_empty("org_id", &org_id)?;
        validate_non_empty("user_id", &user_id)?;
        Ok(Self { org_id, user_id })
    }
}
