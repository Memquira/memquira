use async_trait::async_trait;
use memquira_common::MemquiraResult;

use crate::org::OrgPlanConfig;

#[async_trait]
pub trait OrgPlanStore: Send + Sync {
    async fn get_org_plan(&self, org_id: &str) -> MemquiraResult<Option<OrgPlanConfig>>;

    async fn upsert_org_plan(&self, plan: OrgPlanConfig) -> MemquiraResult<OrgPlanConfig>;

    async fn delete_org_plan(&self, org_id: &str) -> MemquiraResult<bool>;
}
