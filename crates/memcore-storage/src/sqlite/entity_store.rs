use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::pagination::{PageCursor, is_after_cursor_in_desc_order};
use memcore_core::ports::EntityStore;
use memcore_core::{
    Entity, EntityAlias, EntityRelation, EntityRole, EntityType, FactEntityLink, TenantContext,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::sqlite::conversions::{
    datetime_from_str, datetime_to_str, optional_datetime_from_str, optional_datetime_to_str,
};

const ENTITY_COLUMNS: &str = "id, org_id, user_id, canonical_name, entity_type, aliases, summary, is_speaker, name_embedding, name_embedding_model, name_embedding_dimensions, created_at, updated_at";
const LINK_COLUMNS: &str = "org_id, user_id, fact_id, entity_id, role, mention, created_at";
const RELATION_COLUMNS: &str = "id, org_id, user_id, source_entity_id, relation, target_entity_id, valid_at, invalid_at, source_fact_id, created_at";

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn text(row: &SqliteRow, name: &'static str) -> MemcoreResult<String> {
    row.try_get(name)
        .map_err(|error| storage_error(format!("entity row {name}"), error))
}

fn optional(row: &SqliteRow, name: &'static str) -> MemcoreResult<Option<String>> {
    row.try_get(name)
        .map_err(|error| storage_error(format!("entity row {name}"), error))
}

fn uuid(value: &str, name: &str) -> MemcoreResult<Uuid> {
    Uuid::parse_str(value).map_err(|error| storage_error(format!("entity row {name}"), error))
}

fn parse_entity(row: &SqliteRow) -> MemcoreResult<Entity> {
    let aliases: Vec<EntityAlias> = serde_json::from_str(&text(row, "aliases")?)
        .map_err(|error| storage_error("entity row aliases", error))?;
    let name_embedding = optional(row, "name_embedding")?
        .map(|raw| serde_json::from_str::<Vec<f32>>(&raw))
        .transpose()
        .map_err(|error| storage_error("entity row name_embedding", error))?;
    Ok(Entity {
        id: uuid(&text(row, "id")?, "id")?,
        org_id: text(row, "org_id")?,
        user_id: text(row, "user_id")?,
        canonical_name: text(row, "canonical_name")?,
        entity_type: EntityType::parse(&text(row, "entity_type")?),
        aliases,
        summary: optional(row, "summary")?,
        is_speaker: row
            .try_get::<i64, _>("is_speaker")
            .map_err(|error| storage_error("entity row is_speaker", error))?
            != 0,
        name_embedding,
        name_embedding_model: optional(row, "name_embedding_model")?,
        name_embedding_dimensions: row
            .try_get::<Option<i64>, _>("name_embedding_dimensions")
            .map_err(|error| storage_error("entity row name_embedding_dimensions", error))?
            .map(|dimensions| dimensions as usize),
        created_at: datetime_from_str(&text(row, "created_at")?)?,
        updated_at: datetime_from_str(&text(row, "updated_at")?)?,
    })
}

fn parse_link(row: &SqliteRow) -> MemcoreResult<FactEntityLink> {
    Ok(FactEntityLink {
        org_id: text(row, "org_id")?,
        user_id: text(row, "user_id")?,
        fact_id: uuid(&text(row, "fact_id")?, "fact_id")?,
        entity_id: uuid(&text(row, "entity_id")?, "entity_id")?,
        role: EntityRole::parse(&text(row, "role")?),
        mention: text(row, "mention")?,
        created_at: datetime_from_str(&text(row, "created_at")?)?,
    })
}

fn parse_relation(row: &SqliteRow) -> MemcoreResult<EntityRelation> {
    Ok(EntityRelation {
        id: uuid(&text(row, "id")?, "id")?,
        org_id: text(row, "org_id")?,
        user_id: text(row, "user_id")?,
        source_entity_id: uuid(&text(row, "source_entity_id")?, "source_entity_id")?,
        relation: text(row, "relation")?,
        target_entity_id: uuid(&text(row, "target_entity_id")?, "target_entity_id")?,
        valid_at: optional_datetime_from_str(optional(row, "valid_at")?)?,
        invalid_at: optional_datetime_from_str(optional(row, "invalid_at")?)?,
        source_fact_id: optional(row, "source_fact_id")?
            .map(|id| uuid(&id, "source_fact_id"))
            .transpose()?,
        created_at: datetime_from_str(&text(row, "created_at")?)?,
    })
}

fn ensure_tenant(org_id: &str, user_id: &str, tenant: &TenantContext) -> MemcoreResult<()> {
    if org_id == tenant.org_id && user_id == tenant.user_id {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn oldest_first<T>(items: &mut [T], key: impl Fn(&T) -> (chrono::DateTime<chrono::Utc>, String)) {
    items.sort_by_key(key);
}

/// SQLite entity storage (migration 0014).
#[derive(Clone, Debug)]
pub struct SqliteEntityStore {
    pool: SqlitePool,
}

impl SqliteEntityStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;
        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;
        Ok(Self::new(pool))
    }
}

#[async_trait]
impl EntityStore for SqliteEntityStore {
    async fn upsert_entity(&self, tenant: &TenantContext, entity: Entity) -> MemcoreResult<Entity> {
        ensure_tenant(&entity.org_id, &entity.user_id, tenant)?;
        let aliases = serde_json::to_string(&entity.aliases)
            .map_err(|error| storage_error("serialize aliases", error))?;
        let embedding = entity
            .name_embedding
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| storage_error("serialize name_embedding", error))?;
        sqlx::query(
            "INSERT INTO entities (id, org_id, user_id, canonical_name, entity_type, aliases, \
             summary, is_speaker, name_embedding, name_embedding_model, \
             name_embedding_dimensions, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET canonical_name = excluded.canonical_name, \
             entity_type = excluded.entity_type, aliases = excluded.aliases, \
             summary = excluded.summary, is_speaker = excluded.is_speaker, \
             name_embedding = excluded.name_embedding, \
             name_embedding_model = excluded.name_embedding_model, \
             name_embedding_dimensions = excluded.name_embedding_dimensions, \
             updated_at = excluded.updated_at \
             WHERE entities.org_id = excluded.org_id AND entities.user_id = excluded.user_id",
        )
        .bind(entity.id.to_string())
        .bind(&entity.org_id)
        .bind(&entity.user_id)
        .bind(&entity.canonical_name)
        .bind(entity.entity_type.as_str())
        .bind(aliases)
        .bind(&entity.summary)
        .bind(i64::from(entity.is_speaker))
        .bind(embedding)
        .bind(&entity.name_embedding_model)
        .bind(
            entity
                .name_embedding_dimensions
                .map(|dimensions| dimensions as i64),
        )
        .bind(datetime_to_str(entity.created_at))
        .bind(datetime_to_str(entity.updated_at))
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to upsert entity", error))?;
        self.get_entity(tenant, entity.id)
            .await?
            .ok_or(MemcoreError::Forbidden)
    }

    async fn get_entity(&self, tenant: &TenantContext, id: Uuid) -> MemcoreResult<Option<Entity>> {
        sqlx::query(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE id = ? AND org_id = ? AND user_id = ?"
        ))
        .bind(id.to_string())
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
        let rows = sqlx::query(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE org_id = ? AND user_id = ?"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list entities", error))?;
        let mut entities = rows
            .iter()
            .map(parse_entity)
            .collect::<MemcoreResult<Vec<_>>>()?;
        oldest_first(&mut entities, |entity| {
            (entity.created_at, entity.id.to_string())
        });
        Ok(entities)
    }

    async fn list_entities_page(
        &self,
        tenant: &TenantContext,
        limit: usize,
        cursor: Option<&PageCursor>,
    ) -> MemcoreResult<Vec<Entity>> {
        let mut entities = self.list_all_entities(tenant).await?;
        entities.reverse();
        Ok(entities
            .into_iter()
            .filter(|entity| {
                cursor.is_none_or(|cursor| {
                    is_after_cursor_in_desc_order(entity.created_at, &entity.id.to_string(), cursor)
                })
            })
            .take(limit)
            .collect())
    }

    async fn link_fact(&self, tenant: &TenantContext, link: FactEntityLink) -> MemcoreResult<()> {
        ensure_tenant(&link.org_id, &link.user_id, tenant)?;
        sqlx::query(
            "INSERT INTO fact_entities (org_id, user_id, fact_id, entity_id, role, mention, \
             created_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(&link.org_id)
        .bind(&link.user_id)
        .bind(link.fact_id.to_string())
        .bind(link.entity_id.to_string())
        .bind(link.role.as_str())
        .bind(&link.mention)
        .bind(datetime_to_str(link.created_at))
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
            "DELETE FROM fact_entities WHERE org_id = ? AND user_id = ? AND fact_id = ? \
             AND entity_id = ? AND role = ?",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(link.fact_id.to_string())
        .bind(link.entity_id.to_string())
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
        let rows = sqlx::query(&format!(
            "SELECT {LINK_COLUMNS} FROM fact_entities WHERE org_id = ? AND user_id = ? AND entity_id = ?"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(entity_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list entity links", error))?;
        let mut links = rows
            .iter()
            .map(parse_link)
            .collect::<MemcoreResult<Vec<_>>>()?;
        oldest_first(&mut links, |link| {
            (
                link.created_at,
                format!("{}{}", link.fact_id, link.role.as_str()),
            )
        });
        Ok(links)
    }

    async fn links_for_facts(
        &self,
        tenant: &TenantContext,
        fact_ids: &[Uuid],
    ) -> MemcoreResult<Vec<FactEntityLink>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT {LINK_COLUMNS} FROM fact_entities WHERE org_id = "
        ));
        query.push_bind(&tenant.org_id);
        query.push(" AND user_id = ");
        query.push_bind(&tenant.user_id);
        query.push(" AND fact_id IN (");
        let mut ids = query.separated(", ");
        for id in fact_ids {
            ids.push_bind(id.to_string());
        }
        query.push(")");
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to list fact links", error))?;
        let mut links = rows
            .iter()
            .map(parse_link)
            .collect::<MemcoreResult<Vec<_>>>()?;
        oldest_first(&mut links, |link| {
            (
                link.created_at,
                format!("{}{}", link.entity_id, link.role.as_str()),
            )
        });
        Ok(links)
    }

    async fn insert_relation(
        &self,
        tenant: &TenantContext,
        relation: EntityRelation,
    ) -> MemcoreResult<EntityRelation> {
        ensure_tenant(&relation.org_id, &relation.user_id, tenant)?;
        sqlx::query(&format!(
            "INSERT INTO entity_relations ({RELATION_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(relation.id.to_string())
        .bind(&relation.org_id)
        .bind(&relation.user_id)
        .bind(relation.source_entity_id.to_string())
        .bind(&relation.relation)
        .bind(relation.target_entity_id.to_string())
        .bind(optional_datetime_to_str(relation.valid_at))
        .bind(optional_datetime_to_str(relation.invalid_at))
        .bind(relation.source_fact_id.map(|id| id.to_string()))
        .bind(datetime_to_str(relation.created_at))
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
        let rows = sqlx::query(&format!(
            "SELECT {RELATION_COLUMNS} FROM entity_relations WHERE org_id = ? AND user_id = ? \
             AND (source_entity_id = ? OR target_entity_id = ?)"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(entity_id.to_string())
        .bind(entity_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list entity relations", error))?;
        let mut relations = rows
            .iter()
            .map(parse_relation)
            .collect::<MemcoreResult<Vec<_>>>()?;
        oldest_first(&mut relations, |relation| {
            (relation.created_at, relation.id.to_string())
        });
        Ok(relations)
    }

    async fn delete_user_entities(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| storage_error("failed to begin entity delete", error))?;
        for table in ["fact_entities", "entity_relations"] {
            sqlx::query(&format!(
                "DELETE FROM {table} WHERE org_id = ? AND user_id = ?"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| storage_error(format!("failed to delete {table}"), error))?;
        }
        let deleted = sqlx::query("DELETE FROM entities WHERE org_id = ? AND user_id = ?")
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
            let mut query: QueryBuilder<Sqlite> =
                QueryBuilder::new(format!("DELETE FROM {table} WHERE org_id = "));
            query.push_bind(&tenant.org_id);
            query.push(" AND user_id = ");
            query.push_bind(&tenant.user_id);
            query.push(format!(" AND {column} IN ("));
            let mut ids = query.separated(", ");
            for id in fact_ids {
                ids.push_bind(id.to_string());
            }
            query.push(")");
            query
                .build()
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error(format!("failed to clean {table}"), error))?;
        }
        let orphans: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM entities e WHERE org_id = ? AND user_id = ? AND NOT EXISTS \
             (SELECT 1 FROM fact_entities l WHERE l.org_id = e.org_id AND l.user_id = e.user_id \
             AND l.entity_id = e.id)",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| storage_error("failed to find orphan entities", error))?;
        for id in &orphans {
            for (table, column) in [
                ("entity_relations", "source_entity_id"),
                ("entity_relations", "target_entity_id"),
                ("entities", "id"),
            ] {
                sqlx::query(&format!(
                    "DELETE FROM {table} WHERE org_id = ? AND user_id = ? AND {column} = ?"
                ))
                .bind(&tenant.org_id)
                .bind(&tenant.user_id)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| storage_error(format!("failed to delete from {table}"), error))?;
            }
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage_error("failed to commit entity cleanup", error))?;
        orphans.iter().map(|id| uuid(id, "id")).collect()
    }

    async fn count_entities_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entities WHERE org_id = ?")
            .bind(org_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count entities", error))?;
        Ok(count as usize)
    }
}
