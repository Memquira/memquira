use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::EpisodeStore;
use memcore_core::{Episode, TenantContext, message_role_from_str, message_role_to_str};
use sqlx::sqlite::{SqliteConnection, SqlitePool, SqliteRow};
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::sqlite::conversions::{
    datetime_from_str, datetime_to_str, metadata_from_str, metadata_to_str,
    optional_datetime_from_str, optional_datetime_to_str,
};

const COLUMNS: &str = "id, org_id, user_id, session_id, message_id, speaker, role, content, observed_at, recorded_at, embedded, metadata";

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn parse_row(row: &SqliteRow) -> MemcoreResult<Episode> {
    let text = |name: &'static str| -> MemcoreResult<String> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("episode row {name}"), error))
    };
    let optional = |name: &'static str| -> MemcoreResult<Option<String>> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("episode row {name}"), error))
    };
    let role = text("role")?;
    Ok(Episode {
        id: Uuid::parse_str(&text("id")?)
            .map_err(|error| storage_error("episode row id", error))?,
        org_id: text("org_id")?,
        user_id: text("user_id")?,
        session_id: optional("session_id")?,
        message_id: optional("message_id")?,
        speaker: optional("speaker")?,
        role: message_role_from_str(&role)
            .ok_or_else(|| storage_error("episode row role", format!("unknown role '{role}'")))?,
        content: text("content")?,
        observed_at: optional_datetime_from_str(optional("observed_at")?)?,
        recorded_at: datetime_from_str(&text("recorded_at")?)?,
        embedded: row
            .try_get::<i64, _>("embedded")
            .map_err(|error| storage_error("episode row embedded", error))?
            != 0,
        metadata: metadata_from_str(&text("metadata")?)?,
    })
}

/// SQLite episode storage (migration 0012).
#[derive(Clone, Debug)]
pub struct SqliteEpisodeStore {
    pool: SqlitePool,
}

impl SqliteEpisodeStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;
        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;
        Ok(Self::new(pool))
    }
}

/// Inserts `episode`, or (its client message ID already stored in the scope) returns the existing
/// row. `None` means inserted. Runs on the caller's connection; inside a transaction whose first
/// statement is this insert, SQLite's write lock serialises concurrent writers (WAL, busy timeout),
/// so the lookup sees any row a concurrent writer committed first.
async fn insert_or_existing(
    conn: &mut SqliteConnection,
    tenant: &TenantContext,
    episode: &Episode,
) -> MemcoreResult<Option<Episode>> {
    if episode.org_id != tenant.org_id || episode.user_id != tenant.user_id {
        return Err(MemcoreError::Forbidden);
    }
    let inserted = sqlx::query(&format!(
        "INSERT INTO episodes ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
    ))
    .bind(episode.id.to_string())
    .bind(&episode.org_id)
    .bind(&episode.user_id)
    .bind(&episode.session_id)
    .bind(&episode.message_id)
    .bind(&episode.speaker)
    .bind(message_role_to_str(&episode.role))
    .bind(&episode.content)
    .bind(optional_datetime_to_str(episode.observed_at))
    .bind(datetime_to_str(episode.recorded_at))
    .bind(i64::from(episode.embedded))
    .bind(metadata_to_str(&episode.metadata)?)
    .execute(&mut *conn)
    .await
    .map_err(|error| storage_error("failed to insert episode", error))?
    .rows_affected();

    if inserted == 1 {
        return Ok(None);
    }
    let existing = match &episode.message_id {
        Some(message_id) => {
            let row = sqlx::query(&format!(
                "SELECT {COLUMNS} FROM episodes WHERE org_id = ? AND user_id = ? AND COALESCE(session_id, '') = ? AND message_id = ?"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(episode.session_id.as_deref().unwrap_or(""))
            .bind(message_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|error| storage_error("failed to fetch episode by message id", error))?;
            row.as_ref().map(parse_row).transpose()?
        }
        None => None,
    };
    existing
        .map(Some)
        .ok_or_else(|| storage_error("failed to insert episode", "conflict without a stored row"))
}

#[async_trait]
impl EpisodeStore for SqliteEpisodeStore {
    async fn episode_id_exists_in_any_tenant(&self, episode_id: Uuid) -> MemcoreResult<bool> {
        let mut query =
            sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT id FROM episodes WHERE id = ");
        query.push_bind(episode_id.to_string());
        query.push(" LIMIT 1");
        let row = query
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage_error("failed to check episode id", error))?;
        Ok(row.is_some())
    }

    async fn insert_episodes(
        &self,
        tenant: &TenantContext,
        episodes: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| storage_error("failed to acquire a connection", error))?;
        let mut stored = Vec::with_capacity(episodes.len());
        for episode in episodes {
            match insert_or_existing(&mut conn, tenant, &episode).await? {
                None => stored.push(episode),
                Some(existing) => stored.push(existing),
            }
        }
        Ok(stored)
    }

    async fn insert_episodes_checked(
        &self,
        tenant: &TenantContext,
        episodes: Vec<Episode>,
    ) -> MemcoreResult<Vec<Episode>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| storage_error("failed to begin an episode transaction", error))?;
        let mut stored = Vec::with_capacity(episodes.len());
        for (position, episode) in episodes.into_iter().enumerate() {
            match insert_or_existing(&mut tx, tenant, &episode).await? {
                None => stored.push(episode),
                Some(existing) if existing.same_occurrence(&episode) => stored.push(existing),
                // Dropping the transaction rolls back every episode of this batch.
                Some(_) => return Err(MemcoreError::message_id_conflict(position)),
            }
        }
        tx.commit()
            .await
            .map_err(|error| storage_error("failed to commit episodes", error))?;
        Ok(stored)
    }

    async fn get_episodes(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<Vec<Episode>> {
        let mut episodes = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let mut query = QueryBuilder::<Sqlite>::new(format!(
                "SELECT {COLUMNS} FROM episodes WHERE org_id = "
            ));
            query.push_bind(&tenant.org_id);
            query.push(" AND user_id = ");
            query.push_bind(&tenant.user_id);
            query.push(" AND id IN (");
            let mut separated = query.separated(", ");
            for id in chunk {
                separated.push_bind(id.to_string());
            }
            separated.push_unseparated(")");
            let rows = query
                .build()
                .fetch_all(&self.pool)
                .await
                .map_err(|error| storage_error("failed to fetch episodes", error))?;
            for row in &rows {
                episodes.push(parse_row(row)?);
            }
        }
        Ok(episodes)
    }

    async fn mark_episodes_embedded(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<()> {
        for id in ids {
            sqlx::query(
                "UPDATE episodes SET embedded = 1 WHERE org_id = ? AND user_id = ? AND id = ?",
            )
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to mark episode embedded", error))?;
        }
        Ok(())
    }

    async fn list_episodes(
        &self,
        tenant: &TenantContext,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM episodes WHERE org_id = ? AND user_id = ? ORDER BY recorded_at ASC, id ASC LIMIT ?"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list episodes", error))?;
        rows.iter().map(parse_row).collect()
    }

    async fn list_episodes_after(
        &self,
        tenant: &TenantContext,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = match after {
            None => sqlx::query(&format!(
                "SELECT {COLUMNS} FROM episodes WHERE org_id = ? AND user_id = ? ORDER BY recorded_at ASC, id ASC LIMIT ?"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await,
            Some((recorded_at, id)) => {
                let recorded_at = datetime_to_str(recorded_at);
                sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM episodes WHERE org_id = ? AND user_id = ? \
                     AND (recorded_at > ? OR (recorded_at = ? AND id > ?)) \
                     ORDER BY recorded_at ASC, id ASC LIMIT ?"
                ))
                .bind(&tenant.org_id)
                .bind(&tenant.user_id)
                .bind(&recorded_at)
                .bind(&recorded_at)
                .bind(id.to_string())
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|error| storage_error("failed to page episodes", error))?;
        rows.iter().map(parse_row).collect()
    }

    async fn delete_user_episodes(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let deleted = sqlx::query("DELETE FROM episodes WHERE org_id = ? AND user_id = ?")
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to delete user episodes", error))?
            .rows_affected();
        Ok(usize::try_from(deleted).unwrap_or(usize::MAX))
    }

    async fn delete_episodes_recorded_before(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<Vec<Uuid>> {
        let cutoff = datetime_to_str(cutoff);
        let rows = sqlx::query(
            "SELECT id FROM episodes WHERE org_id = ? AND user_id = ? AND recorded_at < ?",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(&cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to find expired episodes", error))?;
        let ids = rows
            .iter()
            .map(|row| {
                let id: String = row
                    .try_get("id")
                    .map_err(|error| storage_error("episode row id", error))?;
                Uuid::parse_str(&id).map_err(|error| storage_error("episode row id", error))
            })
            .collect::<MemcoreResult<Vec<Uuid>>>()?;
        if !dry_run && !ids.is_empty() {
            sqlx::query(
                "DELETE FROM episodes WHERE org_id = ? AND user_id = ? AND recorded_at < ?",
            )
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(&cutoff)
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to delete expired episodes", error))?;
        }
        Ok(ids)
    }

    async fn count_episodes_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM episodes WHERE org_id = ?")
            .bind(org_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count episodes", error))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}
