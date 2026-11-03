use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{OrgPlanConfig, OrgPlanLimits, OrgPlanStore};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow};

use crate::sqlite::{datetime_from_str, datetime_to_str};

use super::types::{
    optional_metadata_from_str, optional_metadata_to_str, storage_error, tier_from_storage,
    tier_to_storage, validate_plan_for_storage,
};

#[derive(Debug, Clone)]
pub struct SqliteOrgPlanStore {
    pool: SqlitePool,
}

impl SqliteOrgPlanStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let options = crate::startup_checks::sqlite_connect_options(database_url)?;
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|error| storage_error("connect sqlite org plan store", error))?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl OrgPlanStore for SqliteOrgPlanStore {
    async fn get_org_plan(&self, org_id: &str) -> MemcoreResult<Option<OrgPlanConfig>> {
        validate_org_id(org_id)?;

        let row = sqlx::query(
            r#"
            SELECT org_id, tier, max_users_per_org, max_memories_per_user,
                   max_memories_per_org, daily_provider_request_limit,
                   daily_provider_token_limit, is_active, metadata, created_at, updated_at
            FROM org_plan_configs
            WHERE org_id = ?
            "#,
        )
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("get sqlite org plan", error))?;

        row.map(|row| row_to_plan(&row)).transpose()
    }

    async fn upsert_org_plan(&self, plan: OrgPlanConfig) -> MemcoreResult<OrgPlanConfig> {
        validate_plan_for_storage(&plan)?;
        let metadata = optional_metadata_to_str(&plan.metadata)?;

        sqlx::query(
            r#"
            INSERT INTO org_plan_configs (
                org_id, tier, max_users_per_org, max_memories_per_user,
                max_memories_per_org, daily_provider_request_limit,
                daily_provider_token_limit, is_active, metadata, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(org_id) DO UPDATE SET
                tier = excluded.tier,
                max_users_per_org = excluded.max_users_per_org,
                max_memories_per_user = excluded.max_memories_per_user,
                max_memories_per_org = excluded.max_memories_per_org,
                daily_provider_request_limit = excluded.daily_provider_request_limit,
                daily_provider_token_limit = excluded.daily_provider_token_limit,
                is_active = excluded.is_active,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&plan.org_id)
        .bind(tier_to_storage(plan.tier))
        .bind(plan.limits.max_users_per_org.map(|value| value as i64))
        .bind(plan.limits.max_memories_per_user.map(|value| value as i64))
        .bind(plan.limits.max_memories_per_org.map(|value| value as i64))
        .bind(
            plan.limits
                .daily_provider_request_limit
                .map(|value| value as i64),
        )
        .bind(
            plan.limits
                .daily_provider_token_limit
                .map(|value| value as i64),
        )
        .bind(i64::from(plan.is_active))
        .bind(metadata)
        .bind(datetime_to_str(plan.created_at))
        .bind(datetime_to_str(plan.updated_at))
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("upsert sqlite org plan", error))?;

        self.get_org_plan(&plan.org_id).await?.ok_or_else(|| {
            MemcoreError::StorageError("sqlite org plan missing after upsert".to_string())
        })
    }

    async fn delete_org_plan(&self, org_id: &str) -> MemcoreResult<bool> {
        validate_org_id(org_id)?;

        let result = sqlx::query("DELETE FROM org_plan_configs WHERE org_id = ?")
            .bind(org_id)
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("delete sqlite org plan", error))?;

        Ok(result.rows_affected() > 0)
    }
}

fn row_to_plan(row: &SqliteRow) -> MemcoreResult<OrgPlanConfig> {
    Ok(OrgPlanConfig {
        org_id: row
            .try_get("org_id")
            .map_err(|error| storage_error("row org_id", error))?,
        tier: tier_from_storage(
            row.try_get::<String, _>("tier")
                .map_err(|error| storage_error("row tier", error))?
                .as_str(),
        )?,
        limits: OrgPlanLimits {
            max_users_per_org: optional_i64_to_u64(row.try_get("max_users_per_org").ok().flatten()),
            max_memories_per_user: optional_i64_to_u64(
                row.try_get("max_memories_per_user").ok().flatten(),
            ),
            max_memories_per_org: optional_i64_to_u64(
                row.try_get("max_memories_per_org").ok().flatten(),
            ),
            daily_provider_request_limit: optional_i64_to_u64(
                row.try_get("daily_provider_request_limit").ok().flatten(),
            ),
            daily_provider_token_limit: optional_i64_to_u64(
                row.try_get("daily_provider_token_limit").ok().flatten(),
            ),
        },
        is_active: row
            .try_get::<i64, _>("is_active")
            .map_err(|error| storage_error("row is_active", error))?
            != 0,
        metadata: optional_metadata_from_str(row.try_get("metadata").ok().flatten())?,
        created_at: datetime_from_str(
            row.try_get::<String, _>("created_at")
                .map_err(|error| storage_error("row created_at", error))?
                .as_str(),
        )?,
        updated_at: datetime_from_str(
            row.try_get::<String, _>("updated_at")
                .map_err(|error| storage_error("row updated_at", error))?
                .as_str(),
        )?,
    })
}

fn optional_i64_to_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| if value > 0 { Some(value as u64) } else { None })
}

fn validate_org_id(org_id: &str) -> MemcoreResult<()> {
    if org_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "org_id cannot be empty".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use memcore_core::{OrgPlanLimits, OrgPlanTier};
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    async fn store() -> SqliteOrgPlanStore {
        let file = NamedTempFile::new().expect("temp db");
        let path = file.path().to_string_lossy().to_string();
        std::mem::forget(file);
        SqliteOrgPlanStore::connect(&format!("sqlite://{path}"))
            .await
            .expect("connect")
    }

    fn plan(org_id: &str, tier: OrgPlanTier) -> OrgPlanConfig {
        let now = Utc::now();
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
            metadata: Some(json!({"note": "sqlite"})),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn upsert_get_update_delete_and_metadata_round_trip() {
        let store = store().await;
        assert!(store.get_org_plan("org_sqlite").await.unwrap().is_none());

        let created = store
            .upsert_org_plan(plan("org_sqlite", OrgPlanTier::Free))
            .await
            .unwrap();
        assert_eq!(created.tier, OrgPlanTier::Free);
        assert_eq!(created.metadata, Some(json!({"note": "sqlite"})));

        let mut update = plan("org_sqlite", OrgPlanTier::Pro);
        update.limits.max_users_per_org = Some(99);
        update.metadata = Some(json!({"updated": true}));
        let updated = store.upsert_org_plan(update).await.unwrap();
        assert_eq!(updated.tier, OrgPlanTier::Pro);
        assert_eq!(updated.limits.max_users_per_org, Some(99));
        assert_eq!(updated.metadata, Some(json!({"updated": true})));
        assert_eq!(updated.created_at, created.created_at);

        assert!(store.delete_org_plan("org_sqlite").await.unwrap());
        assert!(!store.delete_org_plan("org_sqlite").await.unwrap());
        assert!(store.get_org_plan("org_sqlite").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn org_isolation_works() {
        let store = store().await;
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

    #[tokio::test]
    async fn invalid_tier_in_database_fails_safely() {
        let store = store().await;
        let now = datetime_to_str(Utc::now());
        sqlx::query(
            r#"
            INSERT INTO org_plan_configs (org_id, tier, is_active, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind("org_bad")
        .bind("gold")
        .bind(1_i64)
        .bind(&now)
        .bind(&now)
        .execute(&store.pool)
        .await
        .unwrap();

        assert!(store.get_org_plan("org_bad").await.is_err());
    }
}
