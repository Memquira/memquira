use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::TenantContext;
use memcore_core::ports::{
    IngestJournalStep, IngestJournalStore, IngestOperation, IngestOperationState,
    IngestOperationStatus, IngestReceipt,
};
use sqlx::postgres::{PgConnection, PgPool, PgRow};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::ingest_journal::{
    claimed_conflict, datetime_from_ms, now_ms, step_fact_columns, step_from_payload, step_payload,
};

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

/// PostgreSQL ingest journal (migration 0016).
#[derive(Clone, Debug)]
pub struct PostgresIngestJournalStore {
    pool: PgPool,
}

impl PostgresIngestJournalStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn begin_tx(&self) -> MemcoreResult<Transaction<'_, Postgres>> {
        self.pool
            .begin()
            .await
            .map_err(|error| storage_error("failed to begin ingest journal transaction", error))
    }
}

async fn insert_step(
    conn: &mut PgConnection,
    operation_id: Uuid,
    step: &IngestJournalStep,
) -> MemcoreResult<()> {
    let (fact_id, related) = step_fact_columns(step);
    // The operation row is locked by the caller's insert or update, so the next seq is stable.
    sqlx::query(
        "INSERT INTO ingest_operation_steps (operation_id, seq, kind, fact_id, related_fact_id, payload) \
         SELECT $1, COALESCE(MAX(seq), 0) + 1, $2, $3, $4, $5 FROM ingest_operation_steps WHERE operation_id = $1",
    )
    .bind(operation_id)
    .bind(step.kind())
    .bind(fact_id)
    .bind(related)
    .bind(step_payload(step)?)
    .execute(conn)
    .await
    .map_err(|error| storage_error("failed to record ingest journal step", error))?;
    Ok(())
}

async fn insert_receipt(
    conn: &mut PgConnection,
    tenant: &TenantContext,
    receipt: &IngestReceipt,
) -> MemcoreResult<()> {
    sqlx::query(
        "INSERT INTO ingest_receipts (org_id, user_id, request_key, operation_id, committed_at_ms, result) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (org_id, user_id, request_key) DO NOTHING",
    )
    .bind(&tenant.org_id)
    .bind(&tenant.user_id)
    .bind(&receipt.request_key)
    .bind(receipt.operation_id)
    .bind(receipt.committed_at.timestamp_millis())
    .bind(&receipt.result)
    .execute(conn)
    .await
    .map_err(|error| storage_error("failed to record ingest receipt", error))?;
    Ok(())
}

async fn delete_operation(conn: &mut PgConnection, operation_id: Uuid) -> MemcoreResult<u64> {
    sqlx::query("DELETE FROM ingest_operation_steps WHERE operation_id = $1")
        .bind(operation_id)
        .execute(&mut *conn)
        .await
        .map_err(|error| storage_error("failed to delete ingest journal steps", error))?;
    let result = sqlx::query("DELETE FROM ingest_operations WHERE id = $1")
        .bind(operation_id)
        .execute(&mut *conn)
        .await
        .map_err(|error| storage_error("failed to delete ingest journal operation", error))?;
    Ok(result.rows_affected())
}

fn parse_operation(row: &PgRow, steps: Vec<IngestJournalStep>) -> MemcoreResult<IngestOperation> {
    let text = |name: &'static str| -> MemcoreResult<String> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("ingest operation {name}"), error))
    };
    let int = |name: &'static str| -> MemcoreResult<i64> {
        row.try_get(name)
            .map_err(|error| storage_error(format!("ingest operation {name}"), error))
    };
    let status = text("status")?;
    let attempts: i32 = row
        .try_get("attempts")
        .map_err(|error| storage_error("ingest operation attempts", error))?;
    Ok(IngestOperation {
        id: row
            .try_get("id")
            .map_err(|error| storage_error("ingest operation id", error))?,
        org_id: text("org_id")?,
        user_id: text("user_id")?,
        status: IngestOperationStatus::parse(&status)
            .ok_or_else(|| storage_error("ingest operation status", status.clone()))?,
        attempts: attempts.max(0) as u32,
        created_at: datetime_from_ms(int("created_at_ms")?)?,
        updated_at: datetime_from_ms(int("updated_at_ms")?)?,
        steps,
    })
}

#[async_trait]
impl IngestJournalStore for PostgresIngestJournalStore {
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
             VALUES ($1, $2, $3, 'in_progress', 0, $4, $4)",
        )
        .bind(operation_id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
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
            "UPDATE ingest_operations SET updated_at_ms = $1 \
             WHERE id = $2 AND org_id = $3 AND user_id = $4 AND status = 'in_progress'",
        )
        .bind(now_ms())
        .bind(operation_id)
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
             WHERE id = $1 AND org_id = $2 AND user_id = $3 AND status = 'in_progress'",
        )
        .bind(operation_id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to commit ingest journal operation", error))?;
        if deleted.rows_affected() == 0 {
            return Err(claimed_conflict());
        }
        sqlx::query("DELETE FROM ingest_operation_steps WHERE operation_id = $1")
            .bind(operation_id)
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
             WHERE org_id = $1 AND user_id = $2 AND request_key = $3",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(request_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to read ingest receipt", error))?;
        row.map(|row| {
            let committed: i64 = row
                .try_get("committed_at_ms")
                .map_err(|error| storage_error("ingest receipt committed_at_ms", error))?;
            Ok(IngestReceipt {
                request_key: row
                    .try_get("request_key")
                    .map_err(|error| storage_error("ingest receipt request_key", error))?,
                operation_id: row
                    .try_get("operation_id")
                    .map_err(|error| storage_error("ingest receipt operation_id", error))?,
                committed_at: datetime_from_ms(committed)?,
                result: row
                    .try_get("result")
                    .map_err(|error| storage_error("ingest receipt result", error))?,
            })
        })
        .transpose()
    }

    async fn purge_receipts(&self, committed_before: DateTime<Utc>) -> MemcoreResult<usize> {
        let result = sqlx::query("DELETE FROM ingest_receipts WHERE committed_at_ms < $1")
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
                 updated_at_ms = $1 WHERE id = $2 AND status = 'in_progress' \
                 RETURNING id, org_id, user_id, status, attempts, created_at_ms, updated_at_ms",
            )
            .bind(now_ms())
            .bind(operation_id),
            Some(cutoff) => sqlx::query(
                "UPDATE ingest_operations SET status = 'rolling_back', attempts = attempts + 1, \
                 updated_at_ms = $1 WHERE id = $2 AND updated_at_ms < $3 \
                 RETURNING id, org_id, user_id, status, attempts, created_at_ms, updated_at_ms",
            )
            .bind(now_ms())
            .bind(operation_id)
            .bind(cutoff.timestamp_millis()),
        }
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| storage_error("failed to claim ingest journal operation", error))?;
        let Some(row) = claimed else {
            return Ok(None);
        };
        let step_rows = sqlx::query(
            "SELECT payload FROM ingest_operation_steps WHERE operation_id = $1 ORDER BY seq",
        )
        .bind(operation_id)
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
        sqlx::query("UPDATE ingest_operations SET updated_at_ms = $1 WHERE id = $2")
            .bind(now_ms())
            .bind(operation_id)
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
            "SELECT id FROM ingest_operations WHERE updated_at_ms < $1 ORDER BY updated_at_ms, id LIMIT $2",
        )
        .bind(stale_before.timestamp_millis())
        .bind(limit.min(i64::MAX as usize) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| storage_error("failed to list stale ingest operations", error))?;
        rows.iter()
            .map(|row| {
                row.try_get("id")
                    .map_err(|error| storage_error("ingest operation id", error))
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
             WHERE (s.fact_id = $1 OR s.related_fact_id = $1) AND o.org_id = $2 AND o.user_id = $3 \
             AND o.id <> $4 LIMIT 1",
        )
        .bind(fact_id)
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .bind(exclude)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| storage_error("failed to look up in-flight ingest operations", error))?;
        row.map(|row| {
            row.try_get("id")
                .map_err(|error| storage_error("ingest operation id", error))
        })
        .transpose()
    }

    async fn operation_state(
        &self,
        operation_id: Uuid,
    ) -> MemcoreResult<Option<IngestOperationState>> {
        let row = sqlx::query("SELECT status, updated_at_ms FROM ingest_operations WHERE id = $1")
            .bind(operation_id)
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
             COALESCE(SUM(CASE WHEN status = 'rolling_back' THEN 1 ELSE 0 END), 0)::BIGINT AS rolling_back, \
             MIN(updated_at_ms) AS oldest, COALESCE(MAX(attempts), 0)::BIGINT AS max_attempts \
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
             (SELECT id FROM ingest_operations WHERE org_id = $1 AND user_id = $2)",
        )
        .bind(&tenant.org_id)
        .bind(&tenant.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            storage_error("failed to delete the user's ingest journal steps", error)
        })?;
        sqlx::query("DELETE FROM ingest_receipts WHERE org_id = $1 AND user_id = $2")
            .bind(&tenant.org_id)
            .bind(&tenant.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| storage_error("failed to delete the user's ingest receipts", error))?;
        let deleted =
            sqlx::query("DELETE FROM ingest_operations WHERE org_id = $1 AND user_id = $2")
                .bind(&tenant.org_id)
                .bind(&tenant.user_id)
                .execute(&mut *tx)
                .await
                .map_err(|error| {
                    storage_error("failed to delete the user's ingest journal", error)
                })?;
        tx.commit().await.map_err(|error| {
            storage_error("failed to commit ingest journal user deletion", error)
        })?;
        Ok(deleted.rows_affected() as usize)
    }
}
