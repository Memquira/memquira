use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{OrgPlanConfig, OrgPlanStore};

use super::types::validate_plan_for_storage;

#[derive(Debug, Default)]
pub struct MockOrgPlanStore {
    plans: RwLock<HashMap<String, OrgPlanConfig>>,
}

impl MockOrgPlanStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OrgPlanStore for MockOrgPlanStore {
    async fn get_org_plan(&self, org_id: &str) -> MemcoreResult<Option<OrgPlanConfig>> {
        if org_id.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "org_id cannot be empty".to_string(),
            ));
        }

        let plans = self.plans.read().map_err(|error| {
            MemcoreError::StorageError(format!("org plan lock poisoned: {error}"))
        })?;
        Ok(plans.get(org_id).cloned())
    }

    async fn upsert_org_plan(&self, mut plan: OrgPlanConfig) -> MemcoreResult<OrgPlanConfig> {
        validate_plan_for_storage(&plan)?;

        let mut plans = self.plans.write().map_err(|error| {
            MemcoreError::StorageError(format!("org plan lock poisoned: {error}"))
        })?;

        if let Some(existing) = plans.get(&plan.org_id) {
            plan.created_at = existing.created_at;
        }

        plans.insert(plan.org_id.clone(), plan.clone());
        Ok(plan)
    }

    async fn delete_org_plan(&self, org_id: &str) -> MemcoreResult<bool> {
        if org_id.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "org_id cannot be empty".to_string(),
            ));
        }

        let mut plans = self.plans.write().map_err(|error| {
            MemcoreError::StorageError(format!("org plan lock poisoned: {error}"))
        })?;
        Ok(plans.remove(org_id).is_some())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use memcore_core::{OrgPlanLimits, OrgPlanTier};
    use serde_json::json;

    use super::*;

    fn plan(org_id: &str, tier: OrgPlanTier) -> OrgPlanConfig {
        OrgPlanConfig {
            org_id: org_id.to_string(),
            tier,
            limits: OrgPlanLimits {
                max_users_per_org: Some(10),
                max_memories_per_user: Some(20),
                max_memories_per_org: Some(30),
                daily_provider_request_limit: Some(40),
                daily_provider_token_limit: Some(50),
            },
            is_active: true,
            metadata: Some(json!({"note": "test"})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn get_missing_plan_returns_none() {
        let store = MockOrgPlanStore::new();
        assert!(store.get_org_plan("org_missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn upsert_creates_and_updates_plan() {
        let store = MockOrgPlanStore::new();
        let created = store
            .upsert_org_plan(plan("org_1", OrgPlanTier::Free))
            .await
            .unwrap();
        assert_eq!(created.tier, OrgPlanTier::Free);

        let mut updated = plan("org_1", OrgPlanTier::Pro);
        updated.limits.max_users_per_org = Some(99);
        let updated = store.upsert_org_plan(updated).await.unwrap();
        assert_eq!(updated.tier, OrgPlanTier::Pro);
        assert_eq!(updated.limits.max_users_per_org, Some(99));
        assert_eq!(updated.created_at, created.created_at);
    }

    #[tokio::test]
    async fn delete_removes_plan() {
        let store = MockOrgPlanStore::new();
        store
            .upsert_org_plan(plan("org_1", OrgPlanTier::Free))
            .await
            .unwrap();
        assert!(store.delete_org_plan("org_1").await.unwrap());
        assert!(!store.delete_org_plan("org_1").await.unwrap());
        assert!(store.get_org_plan("org_1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn org_isolation_works() {
        let store = MockOrgPlanStore::new();
        store
            .upsert_org_plan(plan("org_a", OrgPlanTier::Free))
            .await
            .unwrap();
        store
            .upsert_org_plan(plan("org_b", OrgPlanTier::Enterprise))
            .await
            .unwrap();

        assert_eq!(
            store.get_org_plan("org_a").await.unwrap().unwrap().tier,
            OrgPlanTier::Free
        );
        assert_eq!(
            store.get_org_plan("org_b").await.unwrap().unwrap().tier,
            OrgPlanTier::Enterprise
        );
    }
}
