use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use sqlx::Row;
use sqlx::sqlite::{SqlitePool, SqliteRow};

use super::types::{
    AppliedMigration, Migration, MigrationRunner, MigrationValidationReport, sorted_migrations,
    split_sql_statements, validate_applied_migrations,
};

fn migration_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::MigrationError(format!("{}: {error}", context.into()))
}

fn parse_applied_row(row: &SqliteRow) -> MemcoreResult<AppliedMigration> {
    let applied_at = row
        .try_get::<String, _>("applied_at")
        .map_err(|error| migration_error("read sqlite migration applied_at", error))?;
    let applied_at = DateTime::parse_from_rfc3339(&applied_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| migration_error("parse sqlite migration applied_at", error))?;

    Ok(AppliedMigration {
        version: row
            .try_get("version")
            .map_err(|error| migration_error("read sqlite migration version", error))?,
        name: row
            .try_get("name")
            .map_err(|error| migration_error("read sqlite migration name", error))?,
        checksum: row
            .try_get("checksum")
            .map_err(|error| migration_error("read sqlite migration checksum", error))?,
        applied_at,
    })
}

#[derive(Debug, Clone)]
pub struct SqliteMigrationRunner {
    pool: SqlitePool,
}

impl SqliteMigrationRunner {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    async fn table_exists(&self, table_name: &str) -> MemcoreResult<bool> {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(1) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| migration_error("check sqlite table existence", error))?;
        Ok(exists > 0)
    }

    async fn schema_migration_count(&self) -> MemcoreResult<i64> {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM schema_migrations")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| migration_error("count sqlite schema migrations", error))
    }

    async fn seed_from_sqlx_migrations(&self, migrations: &[Migration]) -> MemcoreResult<()> {
        if self.schema_migration_count().await? > 0
            || !self.table_exists("_sqlx_migrations").await?
        {
            return Ok(());
        }

        let legacy_versions = sqlx::query("SELECT version FROM _sqlx_migrations WHERE success = 1")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| migration_error("read sqlite legacy sqlx migrations", error))?
            .into_iter()
            .map(|row| row.try_get::<i64, _>("version"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| migration_error("read sqlite legacy migration version", error))?;

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
                "INSERT OR IGNORE INTO schema_migrations (version, name, checksum, applied_at) VALUES (?, ?, ?, ?)",
            )
            .bind(migration.version)
            .bind(migration.name)
            .bind(&migration.checksum)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(|error| migration_error("seed sqlite schema migrations", error))?;
        }

        Ok(())
    }
}

#[async_trait]
impl MigrationRunner for SqliteMigrationRunner {
    async fn ensure_migration_table(&self) -> MemcoreResult<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                checksum TEXT NOT NULL,
                applied_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|error| migration_error("create sqlite schema_migrations table", error))?;
        Ok(())
    }

    async fn applied_migrations(&self) -> MemcoreResult<Vec<AppliedMigration>> {
        self.ensure_migration_table().await?;
        let rows = sqlx::query(
            "SELECT version, name, checksum, applied_at FROM schema_migrations ORDER BY version ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| migration_error("read sqlite schema migrations", error))?;

        rows.iter().map(parse_applied_row).collect()
    }

    async fn validate_migrations(
        &self,
        migrations: &[Migration],
    ) -> MemcoreResult<MigrationValidationReport> {
        self.ensure_migration_table().await?;
        self.seed_from_sqlx_migrations(migrations).await?;
        validate_applied_migrations(migrations, &self.applied_migrations().await?)
    }

    async fn apply_pending_migrations(
        &self,
        migrations: &[Migration],
    ) -> MemcoreResult<MigrationValidationReport> {
        self.ensure_migration_table().await?;
        self.seed_from_sqlx_migrations(migrations).await?;

        let report = self.validate_migrations(migrations).await?;
        if report.has_blocking_issues() {
            return Err(MemcoreError::MigrationError(
                "migration validation failed before applying pending migrations".to_string(),
            ));
        }

        let applied = self.applied_migrations().await?;
        let applied_versions = applied
            .iter()
            .map(|migration| migration.version)
            .collect::<std::collections::HashSet<_>>();

        for migration in sorted_migrations(migrations)?
            .into_iter()
            .filter(|migration| !applied_versions.contains(&migration.version))
        {
            let mut tx =
                self.pool.begin().await.map_err(|error| {
                    migration_error("begin sqlite migration transaction", error)
                })?;

            for statement in split_sql_statements(migration.sql) {
                sqlx::query(&statement)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| migration_error("apply sqlite migration statement", error))?;
            }

            sqlx::query(
                "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES (?, ?, ?, ?)",
            )
            .bind(migration.version)
            .bind(migration.name)
            .bind(&migration.checksum)
            .bind(Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await
            .map_err(|error| migration_error("record sqlite migration", error))?;

            tx.commit()
                .await
                .map_err(|error| migration_error("commit sqlite migration transaction", error))?;
        }

        self.validate_migrations(migrations).await
    }
}

pub async fn required_sqlite_tables_exist(pool: &SqlitePool) -> MemcoreResult<Vec<String>> {
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

    let runner = SqliteMigrationRunner::new(pool.clone());
    let mut missing = Vec::new();
    for table in required {
        if !runner.table_exists(table).await? {
            missing.push(table.to_string());
        }
    }
    Ok(missing)
}

pub async fn run_sqlite_migrations(pool: &SqlitePool) -> MemcoreResult<MigrationValidationReport> {
    let runner = SqliteMigrationRunner::new(pool.clone());
    runner
        .apply_pending_migrations(&crate::migrations::sqlite_migrations())
        .await
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::MigrationStatus;

    async fn test_runner() -> SqliteMigrationRunner {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        SqliteMigrationRunner::new(pool)
    }

    fn test_migration(version: i64, name: &'static str, sql: &'static str) -> Migration {
        Migration::new(version, name, sql)
    }

    #[tokio::test]
    async fn migration_table_is_created() {
        let runner = test_runner().await;
        runner.ensure_migration_table().await.expect("create table");
        assert!(
            runner
                .table_exists("schema_migrations")
                .await
                .expect("exists")
        );
    }

    #[tokio::test]
    async fn pending_migration_is_detected_and_applied() {
        let runner = test_runner().await;
        let migrations = vec![test_migration(
            1,
            "create_test",
            "CREATE TABLE IF NOT EXISTS test_table (id INTEGER PRIMARY KEY);",
        )];

        let report = runner
            .validate_migrations(&migrations)
            .await
            .expect("validate");
        assert!(!report.clean);
        assert_eq!(report.pending_count, 1);

        let report = runner
            .apply_pending_migrations(&migrations)
            .await
            .expect("apply");
        assert!(report.clean);
        assert_eq!(report.pending_count, 0);
        assert_eq!(runner.applied_migrations().await.expect("applied").len(), 1);
        assert!(runner.table_exists("test_table").await.expect("table"));
    }

    #[tokio::test]
    async fn second_run_is_idempotent() {
        let runner = test_runner().await;
        let migrations = vec![test_migration(
            1,
            "create_test",
            "CREATE TABLE IF NOT EXISTS test_table (id INTEGER PRIMARY KEY);",
        )];

        runner
            .apply_pending_migrations(&migrations)
            .await
            .expect("first");
        let report = runner
            .apply_pending_migrations(&migrations)
            .await
            .expect("second");

        assert!(report.clean);
        assert_eq!(runner.applied_migrations().await.expect("applied").len(), 1);
    }

    #[tokio::test]
    async fn checksum_mismatch_is_detected() {
        let runner = test_runner().await;
        let migrations = vec![test_migration(
            1,
            "create_test",
            "CREATE TABLE IF NOT EXISTS test_table (id INTEGER PRIMARY KEY);",
        )];
        runner
            .apply_pending_migrations(&migrations)
            .await
            .expect("apply");

        let changed = vec![test_migration(
            1,
            "create_test",
            "CREATE TABLE IF NOT EXISTS test_table (id INTEGER PRIMARY KEY, name TEXT);",
        )];
        let report = runner
            .validate_migrations(&changed)
            .await
            .expect("validate");
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.status == MigrationStatus::ChecksumMismatch)
        );
    }

    #[tokio::test]
    async fn missing_from_code_migration_is_reported() {
        let runner = test_runner().await;
        let migrations = vec![test_migration(
            1,
            "create_test",
            "CREATE TABLE IF NOT EXISTS test_table (id INTEGER PRIMARY KEY);",
        )];
        runner
            .apply_pending_migrations(&migrations)
            .await
            .expect("apply");

        let report = runner.validate_migrations(&[]).await.expect("validate");
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.status == MigrationStatus::MissingFromCode)
        );
    }

    #[tokio::test]
    async fn failed_migration_does_not_record_applied_row() {
        let runner = test_runner().await;
        let migrations = vec![test_migration(
            1,
            "bad",
            "CREATE TABLE broken (id INTEGER PRIMARY KEY); INSERT INTO missing_table VALUES (1);",
        )];

        let error = runner
            .apply_pending_migrations(&migrations)
            .await
            .expect_err("bad migration should fail");
        assert_eq!(error.code(), "migration_error");
        assert!(
            runner
                .applied_migrations()
                .await
                .expect("applied")
                .is_empty()
        );
    }
}
