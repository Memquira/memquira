use async_trait::async_trait;
use memcore_common::MemcoreResult;

use crate::org::OrgPlanConfig;

#[async_trait]
pub trait OrgPlanStore: Send + Sync {
    async fn get_org_plan(&self, org_id: &str) -> MemcoreResult<Option<OrgPlanConfig>>;

    async fn upsert_org_plan(&self, plan: OrgPlanConfig) -> MemcoreResult<OrgPlanConfig>;

    async fn delete_org_plan(&self, org_id: &str) -> MemcoreResult<bool>;
}
