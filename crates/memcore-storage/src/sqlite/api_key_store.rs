use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{ApiKeyRecord, ApiKeyScope};
use sqlx::sqlite::SqlitePool;
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use memcore_core::ports::ApiKeyStore;

use super::conversions::{
    datetime_from_str, datetime_to_str, optional_datetime_from_str, optional_datetime_to_str,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

pub(crate) fn api_key_scope_to_str(value: ApiKeyScope) -> &'static str {
    match value {
        ApiKeyScope::MemoryRead => "memory_read",
        ApiKeyScope::MemoryWrite => "memory_write",
        ApiKeyScope::MemoryDelete => "memory_delete",
        ApiKeyScope::UserDelete => "user_delete",
        ApiKeyScope::AuditRead => "audit_read",
        ApiKeyScope::AdminRead => "admin_read",
        ApiKeyScope::AdminWrite => "admin_write",
    }
}

pub(crate) fn api_key_scope_from_str(value: &str) -> MemcoreResult<ApiKeyScope> {
    match value {
        "memory_read" => Ok(ApiKeyScope::MemoryRead),
        "memory_write" => Ok(ApiKeyScope::MemoryWrite),
        "memory_delete" => Ok(ApiKeyScope::MemoryDelete),
        "user_delete" => Ok(ApiKeyScope::UserDelete),
        "audit_read" => Ok(ApiKeyScope::AuditRead),
        "admin_read" => Ok(ApiKeyScope::AdminRead),
        "admin_write" => Ok(ApiKeyScope::AdminWrite),
        _ => Err(MemcoreError::StorageError(format!(
            "invalid api key scope value: {value}"
        ))),
    }
}

fn scopes_to_json(scopes: &[ApiKeyScope]) -> MemcoreResult<String> {
    let labels: Vec<&str> = scopes.iter().copied().map(api_key_scope_to_str).collect();
    serde_json::to_string(&labels).map_err(|error| {
        MemcoreError::StorageError(format!("failed to serialize api key scopes: {error}"))
    })
}

fn scopes_from_json(value: &str) -> MemcoreResult<Vec<ApiKeyScope>> {
    let labels: Vec<String> = serde_json::from_str(value).map_err(|error| {
        MemcoreError::StorageError(format!("failed to deserialize api key scopes: {error}"))
    })?;
    labels
        .iter()
        .map(|label| api_key_scope_from_str(label))
        .collect()
}

fn row_to_api_key_record(
    id: String,
    org_id: String,
    name: String,
    key_hash: String,
    scopes: String,
    created_at: String,
    revoked_at: Option<String>,
) -> MemcoreResult<ApiKeyRecord> {
    Ok(ApiKeyRecord {
        id: Uuid::parse_str(&id).map_err(|error| {
            MemcoreError::StorageError(format!("invalid api key id '{id}': {error}"))
        })?,
        org_id,
        name,
        key_hash,
        scopes: scopes_from_json(&scopes)?,
        created_at: datetime_from_str(&created_at)?,
        revoked_at: optional_datetime_from_str(revoked_at)?,
    })
}

fn parse_row(row: &sqlx::sqlite::SqliteRow) -> MemcoreResult<ApiKeyRecord> {
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
pub struct SqliteApiKeyStore {
    pool: SqlitePool,
}

impl SqliteApiKeyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self::new(pool))
    }

    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }
}

#[async_trait]
impl ApiKeyStore for SqliteApiKeyStore {
    async fn find_by_hash(&self, key_hash: &str) -> MemcoreResult<Option<ApiKeyRecord>> {
        let row = sqlx::query(
            "SELECT id, org_id, name, key_hash, scopes, created_at, revoked_at FROM api_keys WHERE key_hash = ? AND revoked_at IS NULL",
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
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(record.id.to_string())
        .bind(&record.org_id)
        .bind(&record.name)
        .bind(&record.key_hash)
        .bind(scopes_to_json(&record.scopes)?)
        .bind(datetime_to_str(record.created_at))
        .bind(optional_datetime_to_str(record.revoked_at))
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to insert api key", error))?;

        Ok(record)
    }

    async fn revoke_api_key(&self, org_id: &str, key_id: Uuid) -> MemcoreResult<()> {
        let revoked_at = Utc::now();
        let result = sqlx::query(
            "UPDATE api_keys SET revoked_at = ? WHERE id = ? AND org_id = ? AND revoked_at IS NULL",
        )
        .bind(datetime_to_str(revoked_at))
        .bind(key_id.to_string())
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
        use crate::pagination::{fetch_limit, push_sqlite_desc_cursor};

        let fetch = i64::try_from(fetch_limit(query.limit))
            .map_err(|error| storage_error("api key list limit out of range for sqlite", error))?;

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, org_id, name, key_hash, scopes, created_at, revoked_at FROM api_keys WHERE org_id = ",
        );
        builder.push_bind(query.org_id.clone());
        if !query.include_revoked {
            builder.push(" AND revoked_at IS NULL");
        }
        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "created_at", "id", cursor);
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use memcore_common::hash_api_key;
    use memcore_core::ApiKeyScope;
    use uuid::Uuid;

    use super::SqliteApiKeyStore;
    use crate::traits::ApiKeyStore;
    use memcore_core::ApiKeyRecord;

    fn sample_record(org_id: &str, name: &str, pepper: &str, raw_key: &str) -> ApiKeyRecord {
        ApiKeyRecord {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            name: name.to_string(),
            key_hash: hash_api_key(pepper, raw_key),
            scopes: vec![
                ApiKeyScope::MemoryRead,
                ApiKeyScope::MemoryWrite,
                ApiKeyScope::AuditRead,
            ],
            created_at: Utc::now(),
            revoked_at: None,
        }
    }

    #[tokio::test]
    async fn sqlite_list_api_keys_by_org() {
        let store = SqliteApiKeyStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite api key store should connect");
        let active = sample_record("org_a", "active", "pepper", "active-token");
        let revoked = {
            let mut record = sample_record("org_a", "revoked", "pepper", "revoked-token");
            record.revoked_at = Some(Utc::now());
            record
        };
        let other_org = sample_record("org_b", "other", "pepper", "other-token");

        store.insert_api_key(active).await.expect("insert");
        store.insert_api_key(revoked).await.expect("insert");
        store.insert_api_key(other_org).await.expect("insert");

        let active_only = store
            .list_api_keys(memcore_core::ports::ApiKeyListQuery {
                org_id: "org_a".to_string(),
                include_revoked: false,
                limit: 100,
                cursor: None,
            })
            .await
            .expect("list");
        assert_eq!(active_only.len(), 1);
        assert_eq!(active_only[0].name, "active");

        let with_revoked = store
            .list_api_keys(memcore_core::ports::ApiKeyListQuery {
                org_id: "org_a".to_string(),
                include_revoked: true,
                limit: 100,
                cursor: None,
            })
            .await
            .expect("list");
        assert_eq!(with_revoked.len(), 2);

        let org_b = store
            .list_api_keys(memcore_core::ports::ApiKeyListQuery {
                org_id: "org_b".to_string(),
                include_revoked: false,
                limit: 100,
                cursor: None,
            })
            .await
            .expect("list");
        assert_eq!(org_b.len(), 1);
        assert_eq!(org_b[0].org_id, "org_b");
    }

    #[tokio::test]
    async fn insert_find_and_revoke_api_key() {
        let store = SqliteApiKeyStore::connect("sqlite::memory:?cache=shared")
            .await
            .expect("sqlite api key store should connect");
        let record = sample_record("org_a", "test-key", "pepper", "secret-token");

        store
            .insert_api_key(record.clone())
            .await
            .expect("insert should succeed");

        let found = store
            .find_by_hash(&record.key_hash)
            .await
            .expect("find should succeed")
            .expect("record should exist");
        assert_eq!(found.id, record.id);
        assert_eq!(found.scopes, record.scopes);

        store
            .revoke_api_key("org_a", record.id)
            .await
            .expect("revoke should succeed");

        let missing = store
            .find_by_hash(&record.key_hash)
            .await
            .expect("find should succeed");
        assert!(missing.is_none());
    }
}
