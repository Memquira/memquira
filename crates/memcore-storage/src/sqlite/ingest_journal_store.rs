use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::TenantContext;
use memcore_core::ports::{
    IngestJournalStep, IngestJournalStore, IngestOperation, IngestOperationState,
    IngestOperationStatus, IngestReceipt,
};
use sqlx::sqlite::{SqliteConnection, SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};
use uuid::Uuid;

use crate::ingest_journal::{
    claimed_conflict, datetime_from_ms, now_ms, step_fact_columns, step_from_payload, step_payload,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

/// SQLite ingest journal (migration 0016).
#[derive(Clone, Debug)]
pub struct SqliteIngestJournalStore {
    pool: SqlitePool,
}

impl SqliteIngestJournalStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> MemcoreResult<Self> {
        let pool = crate::startup_checks::connect_sqlite_pool(database_url).await?;
        crate::migrations::sqlite::run_sqlite_migrations(&pool).await?;
        Ok(Self::new(pool))
    }

    async fn begin_tx(&self) -> MemcoreResult<Transaction<'_, Sqlite>> {
        self.pool
            .begin()
            .await
            .map_err(|error| storage_error("failed to begin ingest journal transaction", error))
    }
}

async fn insert_step(
    conn: &mut SqliteConnection,
    operation_id: Uuid,
    step: &IngestJournalStep,
) -> MemcoreResult<()> {
    let (fact_id, related) = step_fact_columns(step);
    sqlx::query(
        "INSERT INTO ingest_operation_steps (operation_id, seq, kind, fact_id, related_fact_id, payload) \
         SELECT ?, COALESCE(MAX(seq), 0) + 1, ?, ?, ?, ? FROM ingest_operation_steps WHERE operation_id = ?",
    )
    .bind(operation_id.to_string())
    .bind(step.kind())
    .bind(fact_id.to_string())
    .bind(related.map(|id| id.to_string()))
    .bind(step_payload(step)?)
    .bind(operation_id.to_string())
    .execute(conn)
    .await
    .map_err(|error| storage_error("failed to record ingest journal step", error))?;
    Ok(())
}

async fn insert_receipt(
    conn: &mut SqliteConnection,
    tenant: &TenantContext,
    receipt: &IngestReceipt,
) -> MemcoreResult<()> {
    sqlx::query(
        "INSERT INTO ingest_receipts (org_id, user_id, request_key, operation_id, committed_at_ms, result) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (org_id, user_id, request_key) DO NOTHING",
    )
    .bind(&tenant.org_id)
    .bind(&tenant.user_id)
    .bind(&receipt.request_key)
    .bind(receipt.operation_id.map(|id| id.to_string()))
    .bind(receipt.committed_at.timestamp_millis())
    .bind(&receipt.result)
    .execute(conn)
    .await
    .map_err(|error| storage_error("failed to record ingest receipt", error))?;
    Ok(())
}

async fn delete_operation(conn: &mut SqliteConnection, operation_id: Uuid) -> MemcoreResult<u64> {
    sqlx::query("DELETE FROM ingest_operation_steps WHERE operation_id = ?")
        .bind(operation_id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|error| storage_error("failed to delete ingest journal steps", error))?;
    let result = sqlx::query("DELETE FROM ingest_operations WHERE id = ?")
        .bind(operation_id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|error| storage_error("failed to delete ingest journal operation", error))?;
    Ok(result.rows_affected())
}

fn parse_operation(
    row: &SqliteRow,
    steps: Vec<IngestJournalStep>,
) -> MemcoreResult<IngestOperation> {
    let text = |name: &'static str| -> MemcoreResult<String> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("ingest operation {name}"), error))
    };
    let int = |name: &'static str| -> MemcoreResult<i64> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("ingest operation {name}"), error))
    };
    let status = text("status")?;
    Ok(IngestOperation {
        id: Uuid::parse_str(&text("id")?)
            .map_err(|error| storage_error("ingest operation id", error))?,
        org_id: text("org_id")?,
        user_id: text("user_id")?,
        status: IngestOperationStatus::parse(&status)
            .ok_or_else(|| storage_error("ingest operation status", status.clone()))?,
        attempts: int("attempts")?.max(0) as u32,
        created_at: datetime_from_ms(int("created_at_ms")?)?,
        updated_at: datetime_from_ms(int("updated_at_ms")?)?,
        steps,
    })
}

#[async_trait]
impl IngestJournalStore for SqliteIngestJournalStore {
    async fn begin(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        step: &IngestJournalStep,
    ) -> MemcoreResult<()> {
        let now = now_ms();
        let mut tx = self.begin_tx().await?;
        sqlx::query(
            "INSERT INTO ingest_operations (id, org_id, user_id, status, attempts, created_at_ms, updated_at_ms) \
             VALUES (?, ?, ?, 'in_progress', 0, ?, ?)",
        )
        .bind(operation_id.to_string())
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to begin ingest journal operation", error))?;
        insert_step(&mut tx, operation_id, step).await?;
        tx.commit()
            .await
            .map_err(|error| storage_error("failed to commit ingest journal begin", error))
    }

    async fn append(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        step: &IngestJournalStep,
    ) -> MemcoreResult<()> {
        let mut tx = self.begin_tx().await?;
        let touched = sqlx::query(
            "UPDATE ingest_operations SET updated_at_ms = ? \
             WHERE id = ? AND org_id = ? AND user_id = ? AND status = 'in_progress'",
        )
        .bind(now_ms())
        .bind(operation_id.to_string())
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to refresh ingest journal operation", error))?;
        if touched.rows_affected() == 0 {
            return Err(claimed_conflict());
        }
        insert_step(&mut tx, operation_id, step).await?;
        tx.commit()
            .await
            .map_err(|error| storage_error("failed to commit ingest journal step", error))
    }

    async fn commit(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        receipt: Option<&IngestReceipt>,
    ) -> MemcoreResult<()> {
        let mut tx = self.begin_tx().await?;
        let deleted = sqlx::query(
            "DELETE FROM ingest_operations \
             WHERE id = ? AND org_id = ? AND user_id = ? AND status = 'in_progress'",
        )
        .bind(operation_id.to_string())
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to commit ingest journal operation", error))?;
        if deleted.rows_affected() == 0 {
            return Err(claimed_conflict());
        }
        sqlx::query("DELETE FROM ingest_operation_steps WHERE operation_id = ?")
            .bind(operation_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|error| storage_error("failed to delete ingest journal steps", error))?;
        if let Some(receipt) = receipt {
            insert_receipt(&mut tx, tenant, receipt).await?;
        }
        tx.commit()
            .await
            .map_err(|error| storage_error("failed to commit ingest journal commit", error))
    }

    async fn record_receipt(
        &self,
        tenant: &TenantContext,
        receipt: &IngestReceipt,
    ) -> MemcoreResult<()> {
        let mut conn =
            self.pool.acquire().await.map_err(|error| {
                storage_error("failed to acquire ingest receipt connection", error)
            })?;
        insert_receipt(&mut conn, tenant, receipt).await
    }

    async fn find_receipt(
        &self,
        tenant: &TenantContext,
        request_key: &str,
    ) -> MemcoreResult<Option<IngestReceipt>> {
        let row = sqlx::query(
            "SELECT request_key, operation_id, committed_at_ms, result FROM ingest_receipts \
             WHERE org_id = ? AND user_id = ? AND request_key = ?",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(request_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to read ingest receipt", error))?;
        row.map(|row| {
            let get = |name: &'static str| -> MemcoreResult<String> {
                row.try_get(name)
                    .map_err(|error| storage_error(format!("ingest receipt {name}"), error))
            };
            let operation_id: Option<String> = row
                .try_get("operation_id")
                .map_err(|error| storage_error("ingest receipt operation_id", error))?;
            let committed: i64 = row
                .try_get("committed_at_ms")
                .map_err(|error| storage_error("ingest receipt committed_at_ms", error))?;
            Ok(IngestReceipt {
                request_key: get("request_key")?,
                operation_id: operation_id
                    .map(|id| Uuid::parse_str(&id))
                    .transpose()
                    .map_err(|error| storage_error("ingest receipt operation_id", error))?,
                committed_at: datetime_from_ms(committed)?,
                result: get("result")?,
            })
        })
        .transpose()
    }

    async fn purge_receipts(&self, committed_before: DateTime<Utc>) -> MemcoreResult<usize> {
        let result = sqlx::query("DELETE FROM ingest_receipts WHERE committed_at_ms < ?")
            .bind(committed_before.timestamp_millis())
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to purge ingest receipts", error))?;
        Ok(result.rows_affected() as usize)
    }

    async fn claim(
        &self,
        operation_id: Uuid,
        stale_before: Option<DateTime<Utc>>,
    ) -> MemcoreResult<Option<IngestOperation>> {
        let mut tx = self.begin_tx().await?;
        let claimed = match stale_before {
            None => sqlx::query(
                "UPDATE ingest_operations SET status = 'rolling_back', attempts = attempts + 1, \
                 updated_at_ms = ? WHERE id = ? AND status = 'in_progress'",
            )
            .bind(now_ms())
            .bind(operation_id.to_string()),
            Some(cutoff) => sqlx::query(
                "UPDATE ingest_operations SET status = 'rolling_back', attempts = attempts + 1, \
                 updated_at_ms = ? WHERE id = ? AND updated_at_ms < ?",
            )
            .bind(now_ms())
            .bind(operation_id.to_string())
            .bind(cutoff.timestamp_millis()),
        }
        .execute(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to claim ingest journal operation", error))?;
        if claimed.rows_affected() == 0 {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT id, org_id, user_id, status, attempts, created_at_ms, updated_at_ms \
             FROM ingest_operations WHERE id = ?",
        )
        .bind(operation_id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to read ingest journal operation", error))?;
        let step_rows = sqlx::query(
            "SELECT payload FROM ingest_operation_steps WHERE operation_id = ? ORDER BY seq",
        )
        .bind(operation_id.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to read ingest journal steps", error))?;
        let steps = step_rows
            .iter()
            .map(|row| {
                row.try_get::<String, _>("payload")
                    .map_err(|error| storage_error("ingest journal step payload", error))
                    .and_then(|payload| step_from_payload(&payload))
            })
            .collect::<MemcoreResult<Vec<_>>>()?;
        let operation = parse_operation(&row, steps)?;
        tx.commit()
            .await
            .map_err(|error| storage_error("failed to commit ingest journal claim", error))?;
        Ok(Some(operation))
    }

    async fn finish(&self, operation_id: Uuid) -> MemcoreResult<()> {
        let mut tx = self.begin_tx().await?;
        delete_operation(&mut tx, operation_id).await?;
        tx.commit()
            .await
            .map_err(|error| storage_error("failed to commit ingest journal finish", error))
    }

    async fn release(&self, operation_id: Uuid) -> MemcoreResult<()> {
        sqlx::query("UPDATE ingest_operations SET updated_at_ms = ? WHERE id = ?")
            .bind(now_ms())
            .bind(operation_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|error| storage_error("failed to release ingest journal operation", error))?;
        Ok(())
    }

    async fn list_stale(
        &self,
        stale_before: DateTime<Utc>,
        limit: usize,
    ) -> MemcoreResult<Vec<Uuid>> {
        let rows = sqlx::query(
            "SELECT id FROM ingest_operations WHERE updated_at_ms < ? ORDER BY updated_at_ms, id LIMIT ?",
        )
        .bind(stale_before.timestamp_millis())
        .bind(limit.min(i64::MAX as usize) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list stale ingest operations", error))?;
        rows.iter()
            .map(|row| {
                let id: String = row
                    .try_get("id")
                    .map_err(|error| storage_error("ingest operation id", error))?;
                Uuid::parse_str(&id).map_err(|error| storage_error("ingest operation id", error))
            })
            .collect()
    }

    async fn find_in_flight(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
        exclude: Uuid,
    ) -> MemcoreResult<Option<Uuid>> {
        let row = sqlx::query(
            "SELECT o.id FROM ingest_operation_steps s JOIN ingest_operations o ON o.id = s.operation_id \
             WHERE (s.fact_id = ? OR s.related_fact_id = ?) AND o.org_id = ? AND o.user_id = ? \
             AND o.id <> ? LIMIT 1",
        )
        .bind(fact_id.to_string())
        .bind(fact_id.to_string())
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(exclude.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to look up in-flight ingest operations", error))?;
        row.map(|row| {
            let id: String = row
                .try_get("id")
                .map_err(|error| storage_error("ingest operation id", error))?;
            Uuid::parse_str(&id).map_err(|error| storage_error("ingest operation id", error))
        })
        .transpose()
    }

    async fn operation_state(
        &self,
        operation_id: Uuid,
    ) -> MemcoreResult<Option<IngestOperationState>> {
        let row = sqlx::query("SELECT status, updated_at_ms FROM ingest_operations WHERE id = ?")
            .bind(operation_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| storage_error("failed to read ingest operation state", error))?;
        row.map(|row| {
            let status: String = row
                .try_get("status")
                .map_err(|error| storage_error("ingest operation status", error))?;
            let updated: i64 = row
                .try_get("updated_at_ms")
                .map_err(|error| storage_error("ingest operation updated_at_ms", error))?;
            Ok(IngestOperationState {
                status: IngestOperationStatus::parse(&status)
                    .ok_or_else(|| storage_error("ingest operation status", status.clone()))?,
                updated_at: datetime_from_ms(updated)?,
            })
        })
        .transpose()
    }

    async fn count_operations(&self) -> MemcoreResult<usize> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(1) FROM ingest_operations")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| storage_error("failed to count ingest operations", error))?;
        Ok(count.max(0) as usize)
    }

    async fn backlog(&self) -> MemcoreResult<memcore_core::ports::IngestJournalBacklog> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS pending, \
             COALESCE(SUM(CASE WHEN status = 'rolling_back' THEN 1 ELSE 0 END), 0) AS rolling_back, \
             MIN(updated_at_ms) AS oldest, COALESCE(MAX(attempts), 0) AS max_attempts \
             FROM ingest_operations",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| storage_error("failed to read the ingest journal backlog", error))?;
        let get = |name: &'static str| -> MemcoreResult<i64> {
            row.try_get::<i64, _>(name)
                .map_err(|error| storage_error(format!("ingest journal backlog {name}"), error))
        };
        let oldest: Option<i64> = row
            .try_get("oldest")
            .map_err(|error| storage_error("ingest journal backlog oldest", error))?;
        Ok(memcore_core::ports::IngestJournalBacklog {
            pending: get("pending")?.max(0) as usize,
            rolling_back: get("rolling_back")?.max(0) as usize,
            oldest_updated_at: oldest.map(datetime_from_ms).transpose()?,
            max_attempts: get("max_attempts")?.max(0) as u32,
        })
    }

    async fn delete_user_operations(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        let mut tx = self.begin_tx().await?;
        sqlx::query(
            "DELETE FROM ingest_operation_steps WHERE operation_id IN \
             (SELECT id FROM ingest_operations WHERE org_id = ? AND user_id = ?)",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            storage_error("failed to delete the user's ingest journal steps", error)
        })?;
        sqlx::query("DELETE FROM ingest_receipts WHERE org_id = ? AND user_id = ?")
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| storage_error("failed to delete the user's ingest receipts", error))?;
        let deleted = sqlx::query("DELETE FROM ingest_operations WHERE org_id = ? AND user_id = ?")
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| storage_error("failed to delete the user's ingest journal", error))?;
        tx.commit().await.map_err(|error| {
            storage_error("failed to commit ingest journal user deletion", error)
        })?;
        Ok(deleted.rows_affected() as usize)
    }
}
