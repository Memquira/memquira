use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{OrgPlanConfig, OrgPlanLimits, OrgPlanStore};
use serde_json::Value;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};

use super::types::{storage_error, tier_from_storage, tier_to_storage, validate_plan_for_storage};

#[derive(Debug, Clone)]
pub struct PostgresOrgPlanStore {
    pool: PgPool,
}

impl PostgresOrgPlanStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|error| storage_error("connect postgres org plan store", error))?;

        crate::migrations::postgres::run_postgres_migrations(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl OrgPlanStore for PostgresOrgPlanStore {
    async fn get_org_plan(&self, org_id: &str) -> MemcoreResult<Option<OrgPlanConfig>> {
        validate_org_id(org_id)?;

        let row = sqlx::query(
            r#"
            SELECT org_id, tier, max_users_per_org, max_memories_per_user,
                   max_memories_per_org, daily_provider_request_limit,
                   daily_provider_token_limit, is_active, metadata, created_at, updated_at
            FROM org_plan_configs
            WHERE org_id = $1
            "#,
        )
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("get postgres org plan", error))?;

        row.map(|row| row_to_plan(&row)).transpose()
    }

    async fn upsert_org_plan(&self, plan: OrgPlanConfig) -> MemcoreResult<OrgPlanConfig> {
        validate_plan_for_storage(&plan)?;

        sqlx::query(
            r#"
            INSERT INTO org_plan_configs (
                org_id, tier, max_users_per_org, max_memories_per_user,
                max_memories_per_org, daily_provider_request_limit,
                daily_provider_token_limit, is_active, metadata, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT(org_id) DO UPDATE SET
                tier = EXCLUDED.tier,
                max_users_per_org = EXCLUDED.max_users_per_org,
                max_memories_per_user = EXCLUDED.max_memories_per_user,
                max_memories_per_org = EXCLUDED.max_memories_per_org,
                daily_provider_request_limit = EXCLUDED.daily_provider_request_limit,
                daily_provider_token_limit = EXCLUDED.daily_provider_token_limit,
                is_active = EXCLUDED.is_active,
                metadata = EXCLUDED.metadata,
                updated_at = EXCLUDED.updated_at
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
        .bind(plan.is_active)
        .bind(plan.metadata.clone())
        .bind(plan.created_at)
        .bind(plan.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("upsert postgres org plan", error))?;

        self.get_org_plan(&plan.org_id).await?.ok_or_else(|| {
            MemcoreError::StorageError("postgres org plan missing after upsert".to_string())
        })
    }

    async fn delete_org_plan(&self, org_id: &str) -> MemcoreResult<bool> {
        validate_org_id(org_id)?;

        let result = sqlx::query("DELETE FROM org_plan_configs WHERE org_id = $1")
            .bind(org_id)
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("delete postgres org plan", error))?;

        Ok(result.rows_affected() > 0)
    }
}

fn row_to_plan(row: &PgRow) -> MemcoreResult<OrgPlanConfig> {
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
            .try_get("is_active")
            .map_err(|error| storage_error("row is_active", error))?,
        metadata: row.try_get::<Option<Value>, _>("metadata").ok().flatten(),
        created_at: row
            .try_get("created_at")
            .map_err(|error| storage_error("row created_at", error))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|error| storage_error("row updated_at", error))?,
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
