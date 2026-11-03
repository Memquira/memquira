use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::admin::MemoryUsageSnapshot;
use memcore_core::pagination::{PageCursor, build_page};
use memcore_core::ports::{
    MemoryUsageSnapshotQuery, MemoryUsageSnapshotQueryResult, MemoryUsageSnapshotStore,
};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{Postgres, QueryBuilder, Row};

use crate::pagination::{fetch_limit, push_postgres_desc_cursor_uuid};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn row_to_snapshot(row: &PgRow) -> MemcoreResult<MemoryUsageSnapshot> {
    Ok(MemoryUsageSnapshot {
        id: row
            .try_get("id")
            .map_err(|error| storage_error("row id", error))?,
        org_id: row
            .try_get("org_id")
            .map_err(|error| storage_error("row org_id", error))?,
        total_users: row
            .try_get::<i64, _>("total_users")
            .map_err(|error| storage_error("row total_users", error))? as u64,
        total_memories: row
            .try_get::<i64, _>("total_memories")
            .map_err(|error| storage_error("row total_memories", error))?
            as u64,
        active_memories: row
            .try_get::<i64, _>("active_memories")
            .map_err(|error| storage_error("row active_memories", error))?
            as u64,
        deleted_memories: row
            .try_get::<Option<i64>, _>("deleted_memories")
            .ok()
            .flatten()
            .map(|value| value as u64),
        captured_at: row
            .try_get("captured_at")
            .map_err(|error| storage_error("row captured_at", error))?,
        metadata: row.try_get::<Option<Value>, _>("metadata").ok().flatten(),
    })
}

#[derive(Debug, Clone)]
pub struct PostgresMemoryUsageSnapshotStore {
    pool: PgPool,
}

impl PostgresMemoryUsageSnapshotStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|error| {
                storage_error("connect postgres memory usage snapshot store", error)
            })?;

        crate::migrations::postgres::run_postgres_migrations(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl MemoryUsageSnapshotStore for PostgresMemoryUsageSnapshotStore {
    async fn insert_snapshot(
        &self,
        snapshot: MemoryUsageSnapshot,
    ) -> MemcoreResult<MemoryUsageSnapshot> {
        sqlx::query(
            r#"
            INSERT INTO memory_usage_snapshots (
                id, org_id, total_users, total_memories, active_memories,
                deleted_memories, captured_at, metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(snapshot.id)
        .bind(&snapshot.org_id)
        .bind(snapshot.total_users as i64)
        .bind(snapshot.total_memories as i64)
        .bind(snapshot.active_memories as i64)
        .bind(snapshot.deleted_memories.map(|value| value as i64))
        .bind(snapshot.captured_at)
        .bind(snapshot.metadata.clone())
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("insert postgres memory usage snapshot", error))?;

        Ok(snapshot)
    }

    async fn query_snapshots(
        &self,
        query: MemoryUsageSnapshotQuery,
    ) -> MemcoreResult<MemoryUsageSnapshotQueryResult> {
        let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
            r#"
            SELECT id, org_id, total_users, total_memories, active_memories,
                   deleted_memories, captured_at, metadata
            FROM memory_usage_snapshots
            WHERE org_id = 
            "#,
        );
        builder.push_bind(query.org_id.clone());

        if let Some(created_after) = query.created_after {
            builder.push(" AND captured_at >= ");
            builder.push_bind(created_after);
        }

        if let Some(created_before) = query.created_before {
            builder.push(" AND captured_at < ");
            builder.push_bind(created_before);
        }

        if let Some(cursor) = &query.cursor {
            push_postgres_desc_cursor_uuid(&mut builder, "captured_at", "id", cursor);
        }

        builder.push(" ORDER BY captured_at DESC, id DESC LIMIT ");
        builder.push_bind(fetch_limit(query.limit) as i64);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("query postgres memory usage snapshots", error))?;

        let snapshots = rows
            .iter()
            .map(row_to_snapshot)
            .collect::<MemcoreResult<Vec<_>>>()?;

        let page = build_page(snapshots, query.limit, |snapshot| PageCursor {
            last_id: snapshot.id.to_string(),
            last_sort_value: snapshot.captured_at,
        })?;

        Ok(MemoryUsageSnapshotQueryResult {
            snapshots: page.items,
            next_cursor: page.next_cursor,
        })
    }
}
