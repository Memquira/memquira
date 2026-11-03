use std::sync::RwLock;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::admin::MemoryUsageSnapshot;
use memcore_core::pagination::{PageCursor, build_page, is_after_cursor_in_desc_order};
use memcore_core::ports::{
    MemoryUsageSnapshotQuery, MemoryUsageSnapshotQueryResult, MemoryUsageSnapshotStore,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

#[derive(Default)]
pub struct MockMemoryUsageSnapshotStore {
    snapshots: RwLock<Vec<MemoryUsageSnapshot>>,
}

impl MockMemoryUsageSnapshotStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn matches_query(snapshot: &MemoryUsageSnapshot, query: &MemoryUsageSnapshotQuery) -> bool {
    if snapshot.org_id != query.org_id {
        return false;
    }

    if let Some(created_after) = query.created_after
        && snapshot.captured_at < created_after
    {
        return false;
    }

    if let Some(created_before) = query.created_before
        && snapshot.captured_at >= created_before
    {
        return false;
    }

    if let Some(cursor) = &query.cursor {
        return is_after_cursor_in_desc_order(
            snapshot.captured_at,
            snapshot.id.to_string().as_str(),
            cursor,
        );
    }

    true
}

#[async_trait]
impl MemoryUsageSnapshotStore for MockMemoryUsageSnapshotStore {
    async fn insert_snapshot(
        &self,
        snapshot: MemoryUsageSnapshot,
    ) -> MemcoreResult<MemoryUsageSnapshot> {
        self.snapshots
            .write()
            .map_err(|error| storage_error("lock memory usage snapshots for insert", error))?
            .push(snapshot.clone());
        Ok(snapshot)
    }

    async fn query_snapshots(
        &self,
        query: MemoryUsageSnapshotQuery,
    ) -> MemcoreResult<MemoryUsageSnapshotQueryResult> {
        let mut snapshots: Vec<_> = self
            .snapshots
            .read()
            .map_err(|error| storage_error("lock memory usage snapshots for query", error))?
            .iter()
            .filter(|snapshot| matches_query(snapshot, &query))
            .cloned()
            .collect();

        snapshots.sort_by(|left, right| {
            right
                .captured_at
                .cmp(&left.captured_at)
                .then_with(|| right.id.cmp(&left.id))
        });

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
    use uuid::Uuid;

    fn snapshot(org_id: &str, captured_at: chrono::DateTime<Utc>) -> MemoryUsageSnapshot {
        MemoryUsageSnapshot {
            id: Uuid::new_v4(),
            org_id: org_id.to_string(),
            total_users: 2,
            total_memories: 3,
            active_memories: 3,
            deleted_memories: None,
            captured_at,
            metadata: Some(json!({ "source": "test" })),
        }
    }

    #[tokio::test]
    async fn insert_and_query_snapshots_are_org_scoped_and_ordered() {
        let store = MockMemoryUsageSnapshotStore::new();
        let base = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        let older = store
            .insert_snapshot(snapshot("org_a", base - Duration::days(1)))
            .await
            .expect("older");
        let newer = store
            .insert_snapshot(snapshot("org_a", base))
            .await
            .expect("newer");
        store
            .insert_snapshot(snapshot("org_b", base + Duration::days(1)))
            .await
            .expect("other org");

        let result = store
            .query_snapshots(MemoryUsageSnapshotQuery {
                org_id: "org_a".to_string(),
                created_after: None,
                created_before: None,
                limit: 10,
                cursor: None,
            })
            .await
            .expect("query");

        assert_eq!(result.snapshots.len(), 2);
        assert_eq!(result.snapshots[0].id, newer.id);
        assert_eq!(result.snapshots[1].id, older.id);
        assert_eq!(
            result.snapshots[0].metadata,
            Some(json!({ "source": "test" }))
        );
    }

    #[tokio::test]
    async fn date_filters_limit_and_cursor_work() {
        let store = MockMemoryUsageSnapshotStore::new();
        let base = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        store
            .insert_snapshot(snapshot("org_a", base - Duration::days(2)))
            .await
            .expect("one");
        let middle = store
            .insert_snapshot(snapshot("org_a", base - Duration::days(1)))
            .await
            .expect("two");
        let latest = store
            .insert_snapshot(snapshot("org_a", base))
            .await
            .expect("three");

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

        assert_eq!(first_page.snapshots, vec![latest]);
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

        assert_eq!(second_page.snapshots, vec![middle]);
        assert!(second_page.next_cursor.is_some());
    }
}
