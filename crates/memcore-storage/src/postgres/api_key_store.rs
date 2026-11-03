use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{ApiKeyRecord, ApiKeyScope};
use sqlx::postgres::PgPool;
use sqlx::{Postgres, Row};
use uuid::Uuid;

use memcore_core::ports::ApiKeyStore;

use super::conversions::{api_key_scope_from_str, api_key_scope_to_str};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn scopes_to_strings(scopes: &[ApiKeyScope]) -> Vec<String> {
    scopes
        .iter()
        .copied()
        .map(api_key_scope_to_str)
        .map(str::to_string)
        .collect()
}

fn scopes_from_strings(values: Vec<String>) -> MemcoreResult<Vec<ApiKeyScope>> {
    values
        .iter()
        .map(|value| api_key_scope_from_str(value))
        .collect()
}

fn row_to_api_key_record(
    id: Uuid,
    org_id: String,
    name: String,
    key_hash: String,
    scopes: Vec<String>,
    created_at: chrono::DateTime<Utc>,
    revoked_at: Option<chrono::DateTime<Utc>>,
) -> MemcoreResult<ApiKeyRecord> {
    Ok(ApiKeyRecord {
        id,
        org_id,
        name,
        key_hash,
        scopes: scopes_from_strings(scopes)?,
        created_at,
        revoked_at,
    })
}

fn parse_row(row: &sqlx::postgres::PgRow) -> MemcoreResult<ApiKeyRecord> {
    row_to_api_key_record(
        row.try_get("id")
            .map_err(|error| storage_error("row id", error))?,
        row.try_get("org_id")
            .map_err(|error| storage_error("row org_id", error))?,
        row.try_get("name")
            .map_err(|error| storage_error("row name", error))?,
        row.try_get("key_hash")
            .map_err(|error| storage_error("row key_hash", error))?,
        row.try_get("scopes")
            .map_err(|error| storage_error("row scopes", error))?,
        row.try_get("created_at")
            .map_err(|error| storage_error("row created_at", error))?,
        row.try_get("revoked_at")
            .map_err(|error| storage_error("row revoked_at", error))?,
    )
}

#[derive(Clone, Debug)]
pub struct PostgresApiKeyStore {
    pool: PgPool,
}

impl PostgresApiKeyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|error| storage_error("failed to connect postgres database", error))?;

        crate::migrations::postgres::run_postgres_migrations(&pool).await?;

        Ok(Self::new(pool))
    }

    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
}

#[async_trait]
impl ApiKeyStore for PostgresApiKeyStore {
    async fn find_by_hash(&self, key_hash: &str) -> MemcoreResult<Option<ApiKeyRecord>> {
        let row = sqlx::query(
            "SELECT id, org_id, name, key_hash, scopes, created_at, revoked_at FROM api_keys WHERE key_hash = $1 AND revoked_at IS NULL",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to find api key by hash", error))?;

        row.as_ref().map(parse_row).transpose()
    }

    async fn insert_api_key(&self, record: ApiKeyRecord) -> MemcoreResult<ApiKeyRecord> {
        sqlx::query(
            r#"
            INSERT INTO api_keys (id, org_id, name, key_hash, scopes, created_at, revoked_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(record.id)
        .bind(&record.org_id)
        .bind(&record.name)
        .bind(&record.key_hash)
        .bind(scopes_to_strings(&record.scopes))
        .bind(record.created_at)
        .bind(record.revoked_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to insert api key", error))?;

        Ok(record)
    }

    async fn revoke_api_key(&self, org_id: &str, key_id: Uuid) -> MemcoreResult<()> {
        let revoked_at = Utc::now();
        let result = sqlx::query(
            "UPDATE api_keys SET revoked_at = $1 WHERE id = $2 AND org_id = $3 AND revoked_at IS NULL",
        )
        .bind(revoked_at)
        .bind(key_id)
        .bind(org_id)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to revoke api key", error))?;

        if result.rows_affected() == 0 {
            return Err(MemcoreError::NotFound(format!(
                "api key not found: {key_id}"
            )));
        }

        Ok(())
    }

    async fn list_api_keys(
        &self,
        query: memcore_core::ports::ApiKeyListQuery,
    ) -> MemcoreResult<Vec<ApiKeyRecord>> {
        use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};
        use sqlx::QueryBuilder;

        let fetch = i64::try_from(fetch_limit(query.limit)).map_err(|error| {
            storage_error("api key list limit out of range for postgres", error)
        })?;

        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, org_id, name, key_hash, scopes, created_at, revoked_at FROM api_keys WHERE org_id = ",
        );
        builder.push_bind(query.org_id.clone());
        if !query.include_revoked {
            builder.push(" AND revoked_at IS NULL");
        }
        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "created_at", "id", cursor);
        }
        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(fetch);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to list api keys", error))?;

        rows.iter().map(parse_row).collect()
    }
}
