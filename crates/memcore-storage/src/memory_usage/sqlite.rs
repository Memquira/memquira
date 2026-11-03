use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::admin::MemoryUsageSnapshot;
use memcore_core::pagination::{PageCursor, build_page};
use memcore_core::ports::{
    MemoryUsageSnapshotQuery, MemoryUsageSnapshotQueryResult, MemoryUsageSnapshotStore,
};
use serde_json::Value;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use crate::pagination::{fetch_limit, push_sqlite_desc_cursor};
use crate::sqlite::{datetime_from_str, datetime_to_str};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn optional_metadata_to_str(value: &Option<Value>) -> MemcoreResult<Option<String>> {
    match value {
        Some(metadata) => Ok(Some(serde_json::to_string(metadata).map_err(|error| {
            storage_error("serialize memory usage snapshot metadata", error)
        })?)),
        None => Ok(None),
    }
}

fn optional_metadata_from_str(value: Option<String>) -> MemcoreResult<Option<Value>> {
    match value {
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => Ok(Some(serde_json::from_str(&raw).map_err(|error| {
            storage_error("deserialize memory usage snapshot metadata", error)
        })?)),
        None => Ok(None),
    }
}

fn row_to_snapshot(row: &SqliteRow) -> MemcoreResult<MemoryUsageSnapshot> {
    Ok(MemoryUsageSnapshot {
        id: Uuid::parse_str(
            row.try_get::<String, _>("id")
                .map_err(|error| storage_error("row id", error))?
                .as_str(),
        )
        .map_err(|error| storage_error("row id uuid", error))?,
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
        captured_at: datetime_from_str(
            row.try_get::<String, _>("captured_at")
                .map_err(|error| storage_error("row captured_at", error))?
                .as_str(),
        )?,
        metadata: optional_metadata_from_str(row.try_get("metadata").ok())?,
    })
}

#[derive(Debug, Clone)]
pub struct SqliteMemoryUsageSnapshotStore {
    pool: SqlitePool,
}

impl SqliteMemoryUsageSnapshotStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let options = crate::startup_checks::sqlite_connect_options(database_url)?;
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|error| storage_error("connect sqlite memory usage snapshot store", error))?;

        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl MemoryUsageSnapshotStore for SqliteMemoryUsageSnapshotStore {
    async fn insert_snapshot(
        &self,
        snapshot: MemoryUsageSnapshot,
    ) -> MemcoreResult<MemoryUsageSnapshot> {
        let metadata = optional_metadata_to_str(&snapshot.metadata)?;

        sqlx::query(
            r#"
            INSERT INTO memory_usage_snapshots (
                id, org_id, total_users, total_memories, active_memories,
                deleted_memories, captured_at, metadata
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(snapshot.id.to_string())
        .bind(&snapshot.org_id)
        .bind(snapshot.total_users as i64)
        .bind(snapshot.total_memories as i64)
        .bind(snapshot.active_memories as i64)
        .bind(snapshot.deleted_memories.map(|value| value as i64))
        .bind(datetime_to_str(snapshot.captured_at))
        .bind(metadata)
        .execute(&self.pool)
        .await
        .map_err(|error| storage_error("insert sqlite memory usage snapshot", error))?;

        Ok(snapshot)
    }

    async fn query_snapshots(
        &self,
        query: MemoryUsageSnapshotQuery,
    ) -> MemcoreResult<MemoryUsageSnapshotQueryResult> {
        let mut builder: QueryBuilder<Sqlite> = QueryBuilder::new(
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
            builder.push_bind(datetime_to_str(created_after));
        }

        if let Some(created_before) = query.created_before {
            builder.push(" AND captured_at < ");
            builder.push_bind(datetime_to_str(created_before));
        }

        if let Some(cursor) = &query.cursor {
            push_sqlite_desc_cursor(&mut builder, "captured_at", "id", cursor);
        }

        builder.push(" ORDER BY captured_at DESC, id DESC LIMIT ");
        builder.push_bind(fetch_limit(query.limit) as i64);

        let rows = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| storage_error("query sqlite memory usage snapshots", error))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;

    fn test_snapshot(org_id: &str, captured_at: chrono::DateTime<Utc>) -> MemoryUsageSnapshot {
        MemoryUsageSnapshot {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            total_users: 4,
            total_memories: 8,
            active_memories: 8,
            deleted_memories: Some(1),
            captured_at,
            metadata: Some(json!({ "source": "sqlite_test" })),
        }
    }

    async fn store() -> SqliteMemoryUsageSnapshotStore {
        SqliteMemoryUsageSnapshotStore::connect("sqlite::memory:")
            .await
            .expect("store")
    }

    #[tokio::test]
    async fn insert_query_org_isolation_filters_limit_cursor_and_metadata_work() {
        let store = store().await;
        let base = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        store
            .insert_snapshot(test_snapshot("org_a", base - Duration::days(2)))
            .await
            .expect("older");
        let middle = store
            .insert_snapshot(test_snapshot("org_a", base - Duration::days(1)))
            .await
            .expect("middle");
        let latest = store
            .insert_snapshot(test_snapshot("org_a", base))
            .await
            .expect("latest");
        store
            .insert_snapshot(test_snapshot("org_b", base + Duration::days(1)))
            .await
            .expect("other org");

        let first_page = store
            .query_snapshots(MemoryUsageSnapshotQuery {
                org_id: "org_a".to_string(),
                created_after: Some(base - Duration::days(2)),
                created_before: Some(base + Duration::seconds(1)),
                limit: 1,
                cursor: None,
            })
            .await
            .expect("first page");

        assert_eq!(first_page.snapshots.len(), 1);
        assert_eq!(first_page.snapshots[0].id, latest.id);
        assert_eq!(
            first_page.snapshots[0].metadata,
            Some(json!({ "source": "sqlite_test" }))
        );

        let second_page = store
            .query_snapshots(MemoryUsageSnapshotQuery {
                org_id: "org_a".to_string(),
                created_after: Some(base - Duration::days(2)),
                created_before: Some(base + Duration::seconds(1)),
                limit: 1,
                cursor: Some(
                    memcore_core::decode_cursor(first_page.next_cursor.as_deref().expect("cursor"))
                        .expect("decoded cursor"),
                ),
            })
            .await
            .expect("second page");

        assert_eq!(second_page.snapshots.len(), 1);
        assert_eq!(second_page.snapshots[0].id, middle.id);
        assert!(second_page.next_cursor.is_some());
    }
}
