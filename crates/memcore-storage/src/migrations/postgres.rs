use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnection, PgPool, PgRow};
use sqlx::{Connection, Postgres, Row};

use super::types::{
    AppliedMigration, Migration, MigrationRunner, MigrationValidationReport, sorted_migrations,
    split_sql_statements, validate_applied_migrations,
};

/// closure: the session advisory lock every PostgreSQL migration run (and validation)
/// holds, so processes starting together on one database never create `schema_migrations` or apply
/// a migration at the same time (`CREATE TABLE IF NOT EXISTS` is not safe under concurrency in
/// PostgreSQL, and two runners would both see a migration as pending). A fixed key, distinct from
/// application advisory locks ("memcore" + 1).
pub const MIGRATION_LOCK_KEY: i64 = 0x6d65_6d63_6f72_6501;
/// Default for [`PostgresMigrationRunner::with_lock_wait`]: how long a runner waits for another
/// process's migration run before failing startup with a clear error.
pub const DEFAULT_MIGRATION_LOCK_WAIT: Duration = Duration::from_secs(300);
const LOCK_POLL: Duration = Duration::from_millis(100);

fn migration_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::MigrationError(format!("{}: {error}", context.into()))
}

fn parse_applied_row(row: &PgRow) -> MemcoreResult<AppliedMigration> {
    Ok(AppliedMigration {
        version: row
            .try_get("version")
            .map_err(|error| migration_error("read postgres migration version", error))?,
        name: row
            .try_get("name")
            .map_err(|error| migration_error("read postgres migration name", error))?,
        checksum: row
            .try_get("checksum")
            .map_err(|error| migration_error("read postgres migration checksum", error))?,
        applied_at: row
            .try_get("applied_at")
            .map_err(|error| migration_error("read postgres migration applied_at", error))?,
    })
}

#[derive(Debug, Clone)]
pub struct PostgresMigrationRunner {
    pool: PgPool,
    lock_wait: Duration,
}

/// One pooled connection holding [`MIGRATION_LOCK_KEY`], marked close-on-drop: it never returns to
/// the pool, so the lock cannot outlive the run on a pooled connection even when the run is
/// cancelled or fails (closing the session releases its advisory locks).
struct MigrationLock {
    conn: PoolConnection<Postgres>,
}

impl MigrationLock {
    /// Unlocks explicitly (best effort); dropping the connection closes the session either way.
    async fn release(mut self) {
        let _ = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .fetch_one(&mut *self.conn)
            .await;
    }
}

impl PostgresMigrationRunner {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            lock_wait: DEFAULT_MIGRATION_LOCK_WAIT,
        }
    }

    /// How long to wait for the migration lock held by another process (default 300 s).
    pub fn with_lock_wait(mut self, lock_wait: Duration) -> Self {
        self.lock_wait = lock_wait;
        self
    }

    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Acquires the migration lock on a dedicated connection, polling `pg_try_advisory_lock` until
    /// `lock_wait` elapses (no permanent wait; a holder that crashed releases it with its session).
    async fn lock(&self) -> MemcoreResult<MigrationLock> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| migration_error("acquire postgres migration connection", error))?;
        conn.close_on_drop();
        let started = Instant::now();
        loop {
            let locked = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
                .bind(MIGRATION_LOCK_KEY)
                .fetch_one(&mut *conn)
                .await
                .map_err(|error| migration_error("acquire postgres migration lock", error))?;
            if locked {
                return Ok(MigrationLock { conn });
            }
            if started.elapsed() >= self.lock_wait {
                return Err(MemcoreError::MigrationError(format!(
                    "timed out after {}s waiting for the postgres migration lock (another process is migrating this database)",
                    self.lock_wait.as_secs()
                )));
            }
            // Waits on the server (this crate has no async runtime dependency of its own).
            sqlx::query("SELECT pg_sleep($1)")
                .bind(LOCK_POLL.as_secs_f64())
                .execute(&mut *conn)
                .await
                .map_err(|error| migration_error("wait for postgres migration lock", error))?;
        }
    }

    async fn table_exists(&self, table_name: &str) -> MemcoreResult<bool> {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(1) FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1",
        )
        .bind(table_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| migration_error("check postgres table existence", error))?;
        Ok(exists > 0)
    }
}

async fn table_exists_on(conn: &mut PgConnection, table_name: &str) -> MemcoreResult<bool> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(1) FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1",
    )
    .bind(table_name)
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| migration_error("check postgres table existence", error))?;
    Ok(exists > 0)
}

async fn ensure_table_on(conn: &mut PgConnection) -> MemcoreResult<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version BIGINT PRIMARY KEY,
            name TEXT NOT NULL,
            checksum TEXT NOT NULL,
            applied_at TIMESTAMPTZ NOT NULL
        )
        "#,
    )
    .execute(&mut *conn)
    .await
    .map_err(|error| migration_error("create postgres schema_migrations table", error))?;
    Ok(())
}

async fn applied_on(conn: &mut PgConnection) -> MemcoreResult<Vec<AppliedMigration>> {
    let rows = sqlx::query(
        "SELECT version, name, checksum, applied_at FROM schema_migrations ORDER BY version ASC",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|error| migration_error("read postgres schema migrations", error))?;
    rows.iter().map(parse_applied_row).collect()
}

async fn seed_from_sqlx_migrations_on(
    conn: &mut PgConnection,
    migrations: &[Migration],
) -> MemcoreResult<()> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM schema_migrations")
        .fetch_one(&mut *conn)
        .await
        .map_err(|error| migration_error("count postgres schema migrations", error))?;
    if count > 0 || !table_exists_on(conn, "_sqlx_migrations").await? {
        return Ok(());
    }

    let legacy_versions = sqlx::query("SELECT version FROM _sqlx_migrations WHERE success = true")
        .fetch_all(&mut *conn)
        .await
        .map_err(|error| migration_error("read postgres legacy sqlx migrations", error))?
        .into_iter()
        .map(|row| row.try_get::<i64, _>("version"))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| migration_error("read postgres legacy migration version", error))?;

    if legacy_versions.is_empty() {
        return Ok(());
    }

    let legacy_versions = legacy_versions
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    for migration in sorted_migrations(migrations)?
        .into_iter()
        .filter(|migration| legacy_versions.contains(&migration.version))
    {
        sqlx::query(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES ($1, $2, $3, $4) ON CONFLICT (version) DO NOTHING",
        )
        .bind(migration.version)
        .bind(migration.name)
        .bind(&migration.checksum)
        .bind(Utc::now())
        .execute(&mut *conn)
        .await
        .map_err(|error| migration_error("seed postgres schema migrations", error))?;
    }

    Ok(())
}

async fn validate_on(
    conn: &mut PgConnection,
    migrations: &[Migration],
) -> MemcoreResult<MigrationValidationReport> {
    ensure_table_on(conn).await?;
    seed_from_sqlx_migrations_on(conn, migrations).await?;
    validate_applied_migrations(migrations, &applied_on(conn).await?)
}

async fn apply_on(
    conn: &mut PgConnection,
    migrations: &[Migration],
) -> MemcoreResult<MigrationValidationReport> {
    let report = validate_on(conn, migrations).await?;
    if report.has_blocking_issues() {
        return Err(MemcoreError::MigrationError(
            "migration validation failed before applying pending migrations".to_string(),
        ));
    }

    let applied_versions = applied_on(conn)
        .await?
        .iter()
        .map(|migration| migration.version)
        .collect::<std::collections::HashSet<_>>();

    for migration in sorted_migrations(migrations)?
        .into_iter()
        .filter(|migration| !applied_versions.contains(&migration.version))
    {
        let mut tx = conn
            .begin()
            .await
            .map_err(|error| migration_error("begin postgres migration transaction", error))?;

        for statement in split_sql_statements(migration.sql) {
            sqlx::query(&statement)
                .execute(&mut *tx)
                .await
                .map_err(|error| migration_error("apply postgres migration statement", error))?;
        }

        sqlx::query(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(migration.version)
        .bind(migration.name)
        .bind(&migration.checksum)
        .bind(Utc::now())
        .execute(&mut *tx)
        .await
        .map_err(|error| migration_error("record postgres migration", error))?;

        tx.commit()
            .await
            .map_err(|error| migration_error("commit postgres migration transaction", error))?;
    }

    validate_on(conn, migrations).await
}

#[async_trait]
impl MigrationRunner for PostgresMigrationRunner {
    async fn ensure_migration_table(&self) -> MemcoreResult<()> {
        let mut lock = self.lock().await?;
        let result = ensure_table_on(&mut lock.conn).await;
        lock.release().await;
        result
    }

    async fn applied_migrations(&self) -> MemcoreResult<Vec<AppliedMigration>> {
        let mut lock = self.lock().await?;
        let result = match ensure_table_on(&mut lock.conn).await {
            Ok(()) => applied_on(&mut lock.conn).await,
            Err(error) => Err(error),
        };
        lock.release().await;
        result
    }

    async fn validate_migrations(
        &self,
        migrations: &[Migration],
    ) -> MemcoreResult<MigrationValidationReport> {
        let mut lock = self.lock().await?;
        let result = validate_on(&mut lock.conn, migrations).await;
        lock.release().await;
        result
    }

    /// Validates and applies pending migrations under [`MIGRATION_LOCK_KEY`]: a runner that waited
    /// for another process re-reads `schema_migrations` once it holds the lock, so it applies
    /// nothing twice. Each migration commits in its own transaction on the locked connection; a
    /// failure leaves the earlier ones applied and recorded, and the next start resumes.
    async fn apply_pending_migrations(
        &self,
        migrations: &[Migration],
    ) -> MemcoreResult<MigrationValidationReport> {
        let mut lock = self.lock().await?;
        let result = apply_on(&mut lock.conn, migrations).await;
        lock.release().await;
        result
    }
}

pub async fn required_postgres_tables_exist(pool: &PgPool) -> MemcoreResult<Vec<String>> {
    let required = [
        "facts",
        "memory_events",
        "api_keys",
        "provider_usage_events",
        "org_plan_configs",
        "memory_usage_snapshots",
        "background_job_runs",
        "background_job_locks",
        "schema_migrations",
    ];

    let runner = PostgresMigrationRunner::new(pool.clone());
    let mut missing = Vec::new();
    for table in required {
        if !runner.table_exists(table).await? {
            missing.push(table.to_string());
        }
    }
    Ok(missing)
}

pub async fn run_postgres_migrations(pool: &PgPool) -> MemcoreResult<MigrationValidationReport> {
    let runner = PostgresMigrationRunner::new(pool.clone());
    runner
        .apply_pending_migrations(&crate::migrations::postgres_migrations())
        .await
}
