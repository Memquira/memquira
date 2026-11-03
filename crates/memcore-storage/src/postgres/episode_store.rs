use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::EpisodeStore;
use memcore_core::{Episode, TenantContext, message_role_from_str, message_role_to_str};
use sqlx::Row;
use sqlx::postgres::{PgConnection, PgPool, PgRow};
use uuid::Uuid;

const COLUMNS: &str = "id, org_id, user_id, session_id, message_id, speaker, role, content, observed_at, recorded_at, embedded, metadata";

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn parse_row(row: &PgRow) -> MemcoreResult<Episode> {
    fn get<'r, T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
        row: &'r PgRow,
        name: &'static str,
    ) -> MemcoreResult<T> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("episode row {name}"), error))
    }
    let role: String = get(row, "role")?;
    Ok(Episode {
        id: get(row, "id")?,
        org_id: get(row, "org_id")?,
        user_id: get(row, "user_id")?,
        session_id: get(row, "session_id")?,
        message_id: get(row, "message_id")?,
        speaker: get(row, "speaker")?,
        role: message_role_from_str(&role)
            .ok_or_else(|| storage_error("episode row role", format!("unknown role '{role}'")))?,
        content: get(row, "content")?,
        observed_at: get(row, "observed_at")?,
        recorded_at: get(row, "recorded_at")?,
        embedded: get(row, "embedded")?,
        metadata: get(row, "metadata")?,
    })
}

/// Postgres episode storage (migration 0012).
#[derive(Clone, Debug)]
pub struct PostgresEpisodeStore {
    pool: PgPool,
}

impl PostgresEpisodeStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Inserts `episode`, or (its client message ID already stored in the scope) returns the existing
/// row. `None` means inserted. Runs on the caller's connection, so inside a transaction the insert
/// waits for a concurrent uncommitted row with the same key and then sees the committed one.
async fn insert_or_existing(
    conn: &mut PgConnection,
    tenant: &TenantContext,
    episode: &Episode,
) -> MemcoreResult<Option<Episode>> {
    if episode.org_id != tenant.org_id || episode.user_id != tenant.user_id {
        return Err(MemcoreError::Forbidden);
    }
    let inserted = sqlx::query(&format!(
        "INSERT INTO episodes ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT DO NOTHING"
    ))
    .bind(episode.id)
    .bind(&episode.org_id)
    .bind(&episode.user_id)
    .bind(&episode.session_id)
    .bind(&episode.message_id)
    .bind(&episode.speaker)
    .bind(message_role_to_str(&episode.role))
    .bind(&episode.content)
    .bind(episode.observed_at)
    .bind(episode.recorded_at)
    .bind(episode.embedded)
    .bind(&episode.metadata)
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
                "SELECT {COLUMNS} FROM episodes WHERE org_id = $1 AND user_id = $2 AND COALESCE(session_id, '') = $3 AND message_id = $4"
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
impl EpisodeStore for PostgresEpisodeStore {
    async fn episode_id_exists_in_any_tenant(&self, episode_id: Uuid) -> MemcoreResult<bool> {
        let mut query =
            sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT id FROM episodes WHERE id = ");
        query.push_bind(episode_id);
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
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM episodes WHERE org_id = $1 AND user_id = $2 AND id = ANY($3)"
        ))
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to fetch episodes", error))?;
        rows.iter().map(parse_row).collect()
    }

    async fn mark_episodes_embedded(
        &self,
        tenant: &TenantContext,
        ids: &[Uuid],
    ) -> MemcoreResult<()> {
        sqlx::query(
            "UPDATE episodes SET embedded = TRUE WHERE org_id = $1 AND user_id = $2 AND id = ANY($3)",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(ids)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("failed to mark episodes embedded", error))?;
        Ok(())
    }

    async fn list_episodes(
        &self,
        tenant: &TenantContext,
        limit: usize,
    ) -> MemcoreResult<Vec<Episode>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM episodes WHERE org_id = $1 AND user_id = $2 ORDER BY recorded_at ASC, id ASC LIMIT $3"
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
                "SELECT {COLUMNS} FROM episodes WHERE org_id = $1 AND user_id = $2 ORDER BY recorded_at ASC, id ASC LIMIT $3"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await,
            Some((recorded_at, id)) => sqlx::query(&format!(
                "SELECT {COLUMNS} FROM episodes WHERE org_id = $1 AND user_id = $2 \
                 AND (recorded_at, id) > ($3, $4) ORDER BY recorded_at ASC, id ASC LIMIT $5"
            ))
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(recorded_at)
            .bind(id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await,
        }
        .map_err(|error| storage_error("failed to page episodes", error))?;
        rows.iter().map(parse_row).collect()
    }

    async fn delete_user_episodes(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let deleted = sqlx::query("DELETE FROM episodes WHERE org_id = $1 AND user_id = $2")
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
        let sql = if dry_run {
            "SELECT id FROM episodes WHERE org_id = $1 AND user_id = $2 AND recorded_at < $3"
        } else {
            "DELETE FROM episodes WHERE org_id = $1 AND user_id = $2 AND recorded_at < $3 RETURNING id"
        };
        let rows = sqlx::query(sql)
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("failed to prune episodes", error))?;
        rows.iter()
            .map(|row| {
                row.try_get("id")
                    .map_err(|error| storage_error("episode row id", error))
            })
            .collect()
    }

    async fn count_episodes_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM episodes WHERE org_id = $1")
            .bind(org_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count episodes", error))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}
