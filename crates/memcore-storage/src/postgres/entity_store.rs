use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::pagination::PageCursor;
use memcore_core::ports::EntityStore;
use memcore_core::{
    Entity, EntityAlias, EntityRelation, EntityRole, EntityType, FactEntityLink, TenantContext,
};
use serde_json::Value;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgRow};
use uuid::Uuid;

const ENTITY_COLUMNS: &str = "id, org_id, user_id, canonical_name, entity_type, aliases, summary, is_speaker, name_embedding, name_embedding_model, name_embedding_dimensions, created_at, updated_at";
const LINK_COLUMNS: &str = "org_id, user_id, fact_id, entity_id, role, mention, created_at";
const RELATION_COLUMNS: &str = "id, org_id, user_id, source_entity_id, relation, target_entity_id, valid_at, invalid_at, source_fact_id, created_at";

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn get<'r, T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
    row: &'r PgRow,
    name: &'static str,
) -> MemcoreResult<T> {
    row.try_get(name)
        .map_err(|error| storage_error(format!("entity row {name}"), error))
}

fn parse_entity(row: &PgRow) -> MemcoreResult<Entity> {
    let aliases: Vec<EntityAlias> = serde_json::from_value(get::<Value>(row, "aliases")?)
        .map_err(|error| storage_error("entity row aliases", error))?;
    let name_embedding = get::<Option<Value>>(row, "name_embedding")?
        .map(serde_json::from_value::<Vec<f32>>)
        .transpose()
        .map_err(|error| storage_error("entity row name_embedding", error))?;
    let entity_type: String = get(row, "entity_type")?;
    Ok(Entity {
        id: get(row, "id")?,
        org_id: get(row, "org_id")?,
        user_id: get(row, "user_id")?,
        canonical_name: get(row, "canonical_name")?,
        entity_type: EntityType::parse(&entity_type),
        aliases,
        summary: get(row, "summary")?,
        is_speaker: get(row, "is_speaker")?,
        name_embedding,
        name_embedding_model: get(row, "name_embedding_model")?,
        name_embedding_dimensions: get::<Option<i32>>(row, "name_embedding_dimensions")?
            .map(|dimensions| dimensions as usize),
        created_at: get(row, "created_at")?,
        updated_at: get(row, "updated_at")?,
    })
}

fn parse_link(row: &PgRow) -> MemcoreResult<FactEntityLink> {
    let role: String = get(row, "role")?;
    Ok(FactEntityLink {
        org_id: get(row, "org_id")?,
        user_id: get(row, "user_id")?,
        fact_id: get(row, "fact_id")?,
        entity_id: get(row, "entity_id")?,
        role: EntityRole::parse(&role),
        mention: get(row, "mention")?,
        created_at: get(row, "created_at")?,
    })
}

fn parse_relation(row: &PgRow) -> MemcoreResult<EntityRelation> {
    Ok(EntityRelation {
        id: get(row, "id")?,
        org_id: get(row, "org_id")?,
        user_id: get(row, "user_id")?,
        source_entity_id: get(row, "source_entity_id")?,
        relation: get(row, "relation")?,
        target_entity_id: get(row, "target_entity_id")?,
        valid_at: get(row, "valid_at")?,
        invalid_at: get(row, "invalid_at")?,
        source_fact_id: get(row, "source_fact_id")?,
        created_at: get(row, "created_at")?,
    })
}

fn ensure_tenant(org_id: &str, user_id: &str, tenant: &TenantContext) -> MemcoreResult<()> {
    if org_id == tenant.org_id && user_id == tenant.user_id {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn cursor_parts(cursor: Option<&PageCursor>) -> MemcoreResult<Option<(DateTime<Utc>, Uuid)>> {
    cursor
        .map(|cursor| {
            Uuid::parse_str(&cursor.last_id)
                .map(|id| (cursor.last_sort_value, id))
                .map_err(|_| MemcoreError::ValidationError("invalid cursor".to_string()))
        })
        .transpose()
}

/// Postgres entity storage (migration 0014).
#[derive(Clone, Debug)]
pub struct PostgresEntityStore {
    pool: PgPool,
}

impl PostgresEntityStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|error| storage_error("failed to connect to postgres", error))?;
        crate::migrations::postgres::run_postgres_migrations(&pool).await?;
        Ok(Self::new(pool))
    }
}

#[async_trait]
impl EntityStore for PostgresEntityStore {
    async fn upsert_entity(&self, tenant: &TenantContext, entity: Entity) -> MemcoreResult<Entity> {
        ensure_tenant(&entity.org_id, &entity.user_id, tenant)?;
        let aliases = serde_json::to_value(&entity.aliases)
            .map_err(|error| storage_error("serialize aliases", error))?;
        let embedding = entity
            .name_embedding
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| storage_error("serialize name_embedding", error))?;
        sqlx::query(
            "INSERT INTO entities (id, org_id, user_id, canonical_name, entity_type, aliases, \
             summary, is_speaker, name_embedding, name_embedding_model, \
             name_embedding_dimensions, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT (id) DO UPDATE SET canonical_name = EXCLUDED.canonical_name, \
             entity_type = EXCLUDED.entity_type, aliases = EXCLUDED.aliases, \
             summary = EXCLUDED.summary, is_speaker = EXCLUDED.is_speaker, \
             name_embedding = EXCLUDED.name_embedding, \
             name_embedding_model = EXCLUDED.name_embedding_model, \
             name_embedding_dimensions = EXCLUDED.name_embedding_dimensions, \
             updated_at = EXCLUDED.updated_at \
             WHERE entities.org_id = EXCLUDED.org_id AND entities.user_id = EXCLUDED.user_id",
        )
        .bind(entity.id)
        .bind(&entity.org_id)
        .bind(&entity.user_id)
        .bind(&entity.canonical_name)
        .bind(entity.entity_type.as_str())
        .bind(aliases)
        .bind(&entity.summary)
        .bind(entity.is_speaker)
        .bind(embedding)
        .bind(&entity.name_embedding_model)
        .bind(
            entity
                .name_embedding_dimensions
                .map(|dimensions| dimensions as i32),
        )
        .bind(entity.created_at)
        .bind(entity.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to upsert entity", error))?;
        self.get_entity(tenant, entity.id)
            .await?
            .ok_or(MemcoreError::Forbidden)
    }

    async fn get_entity(&self, tenant: &TenantContext, id: Uuid) -> MemcoreResult<Option<Entity>> {
        sqlx::query(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE id = $1 AND org_id = $2 AND user_id = $3"
        ))
        .bind(id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to get entity", error))?
        .as_ref()
        .map(parse_entity)
        .transpose()
    }

    async fn list_all_entities(&self, tenant: &TenantContext) -> MemcoreResult<Vec<Entity>> {
        sqlx::query(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE org_id = $1 AND user_id = $2 \
             ORDER BY created_at ASC, id::text ASC"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list entities", error))?
        .iter()
        .map(parse_entity)
        .collect()
    }

    async fn list_entities_page(
        &self,
        tenant: &TenantContext,
        limit: usize,
        cursor: Option<&PageCursor>,
    ) -> MemcoreResult<Vec<Entity>> {
        let rows = match cursor_parts(cursor)? {
            None => {
                sqlx::query(&format!(
                    "SELECT {ENTITY_COLUMNS} FROM entities WHERE org_id = $1 AND user_id = $2 \
                 ORDER BY created_at DESC, id::text DESC LIMIT $3"
                ))
                .bind(&tenant.org_id)
                .bind(&tenant.user_id)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
            }
            Some((sort_value, id)) => {
                sqlx::query(&format!(
                    "SELECT {ENTITY_COLUMNS} FROM entities WHERE org_id = $1 AND user_id = $2 \
                 AND (created_at < $3 OR (created_at = $3 AND id::text < $4)) \
                 ORDER BY created_at DESC, id::text DESC LIMIT $5"
                ))
                .bind(&tenant.org_id)
                .bind(&tenant.user_id)
                .bind(sort_value)
                .bind(id.to_string())
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|error| storage_error("failed to page entities", error))?;
        rows.iter().map(parse_entity).collect()
    }

    async fn link_fact(&self, tenant: &TenantContext, link: FactEntityLink) -> MemcoreResult<()> {
        ensure_tenant(&link.org_id, &link.user_id, tenant)?;
        sqlx::query(
            "INSERT INTO fact_entities (org_id, user_id, fact_id, entity_id, role, mention, \
             created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
        )
        .bind(&link.org_id)
        .bind(&link.user_id)
        .bind(link.fact_id)
        .bind(link.entity_id)
        .bind(link.role.as_str())
        .bind(&link.mention)
        .bind(link.created_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to link fact", error))?;
        Ok(())
    }

    async fn unlink_fact(
        &self,
        tenant: &TenantContext,
        link: &FactEntityLink,
    ) -> MemcoreResult<bool> {
        let result = sqlx::query(
            "DELETE FROM fact_entities WHERE org_id = $1 AND user_id = $2 AND fact_id = $3 \
             AND entity_id = $4 AND role = $5",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(link.fact_id)
        .bind(link.entity_id)
        .bind(link.role.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to unlink fact", error))?;
        Ok(result.rows_affected() > 0)
    }

    async fn links_for_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Vec<FactEntityLink>> {
        sqlx::query(&format!(
            "SELECT {LINK_COLUMNS} FROM fact_entities WHERE org_id = $1 AND user_id = $2 \
             AND entity_id = $3 ORDER BY created_at ASC, fact_id::text ASC, role ASC"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list entity links", error))?
        .iter()
        .map(parse_link)
        .collect()
    }

    async fn links_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<FactEntityLink>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query(&format!(
            "SELECT {LINK_COLUMNS} FROM fact_entities WHERE org_id = $1 AND user_id = $2 \
             AND fact_id = ANY($3) ORDER BY created_at ASC, entity_id::text ASC, role ASC"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(fact_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list fact links", error))?
        .iter()
        .map(parse_link)
        .collect()
    }

    async fn insert_relation(
        &self,
        tenant: &TenantContext,
        relation: EntityRelation,
    ) -> MemcoreResult<EntityRelation> {
        ensure_tenant(&relation.org_id, &relation.user_id, tenant)?;
        sqlx::query(&format!(
            "INSERT INTO entity_relations ({RELATION_COLUMNS}) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
        ))
        .bind(relation.id)
        .bind(&relation.org_id)
        .bind(&relation.user_id)
        .bind(relation.source_entity_id)
        .bind(&relation.relation)
        .bind(relation.target_entity_id)
        .bind(relation.valid_at)
        .bind(relation.invalid_at)
        .bind(relation.source_fact_id)
        .bind(relation.created_at)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to insert entity relation", error))?;
        Ok(relation)
    }

    async fn relations_for_entity(
        &self,
        tenant: &TenantContext,
        entity_id: Uuid,
    ) -> MemcoreResult<Vec<EntityRelation>> {
        sqlx::query(&format!(
            "SELECT {RELATION_COLUMNS} FROM entity_relations WHERE org_id = $1 AND user_id = $2 \
             AND (source_entity_id = $3 OR target_entity_id = $3) ORDER BY created_at ASC, id::text ASC"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list entity relations", error))?
        .iter()
        .map(parse_relation)
        .collect()
    }

    async fn delete_user_entities(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| storage_error("failed to begin entity delete", error))?;
        for table in ["fact_entities", "entity_relations"] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE org_id = $1 AND user_id = $2"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| storage_error(format!("failed to delete {table}"), error))?;
        }
        let deleted = sqlx::query("DELETE FROM entities WHERE org_id = $1 AND user_id = $2")
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| storage_error("failed to delete entities", error))?
            .rows_affected();
        transaction
            .commit()
            .await
            .map_err(|error| storage_error("failed to commit entity delete", error))?;
        Ok(deleted as usize)
    }

    async fn delete_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<Uuid>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| storage_error("failed to begin entity cleanup", error))?;
        for (table, column) in [
            ("fact_entities", "fact_id"),
            ("entity_relations", "source_fact_id"),
        ] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE org_id = $1 AND user_id = $2 AND {column} = ANY($3)"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(fact_ids)
            .execute(&mut *transaction)
            .await
            .map_err(|error| storage_error(format!("failed to clean {table}"), error))?;
        }
        let orphans: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM entities e WHERE org_id = $1 AND user_id = $2 AND NOT EXISTS \
             (SELECT 1 FROM fact_entities l WHERE l.org_id = e.org_id AND l.user_id = e.user_id \
             AND l.entity_id = e.id) ORDER BY id",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| storage_error("failed to find orphan entities", error))?;
        if !orphans.is_empty() {
            sqlx::query(
                "DELETE FROM entity_relations WHERE org_id = $1 AND user_id = $2 \
                 AND (source_entity_id = ANY($3) OR target_entity_id = ANY($3))",
            )
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(&orphans)
            .execute(&mut *transaction)
            .await
            .map_err(|error| storage_error("failed to delete orphan relations", error))?;
            sqlx::query("DELETE FROM entities WHERE org_id = $1 AND user_id = $2 AND id = ANY($3)")
                .bind(&tenant.org_id)
                .bind(&tenant.user_id)
                .bind(&orphans)
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error("failed to delete orphan entities", error))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage_error("failed to commit entity cleanup", error))?;
        Ok(orphans)
    }

    async fn count_entities_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entities WHERE org_id = $1")
            .bind(org_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count entities", error))?;
        Ok(count as usize)
    }
}
