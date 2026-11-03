#[cfg(any(feature = "sqlite", feature = "postgres"))]
use memcore_common::{MemcoreError, MemcoreResult};
use serde::Serialize;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
use crate::migrations::MigrationRunner;
use crate::migrations::MigrationValidationReport;

#[cfg(feature = "sqlite")]
use crate::migrations::{SqliteMigrationRunner, sqlite_migrations};

#[cfg(feature = "postgres")]
use crate::migrations::{PostgresMigrationRunner, postgres_migrations};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMigrationMode {
    Auto,
    ValidateOnly,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageStartupCheckReport {
    pub database_connected: bool,
    pub migrations_clean: bool,
    pub migration_report: Option<MigrationValidationReport>,
    pub warnings: Vec<String>,
}

impl StorageStartupCheckReport {
    pub fn ready_without_database() -> Self {
        Self {
            database_connected: true,
            migrations_clean: true,
            migration_report: None,
            warnings: Vec::new(),
        }
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
fn migration_failure(report: &MigrationValidationReport) -> MemcoreError {
    let message = report
        .issues
        .first()
        .map(|issue| format!("{}: {}", issue.status.as_str(), issue.message))
        .unwrap_or_else(|| "migration validation failed".to_string());
    MemcoreError::MigrationError(message)
}

#[cfg(feature = "sqlite")]
fn normalize_sqlite_url(database_url: &str) -> String {
    if let Some(rest) = database_url.strip_prefix("sqlite://") {
        format!("sqlite:{rest}")
    } else {
        database_url.to_string()
    }
}

#[cfg(feature = "sqlite")]
pub async fn check_sqlite_startup(
    database_url: &str,
    mode: StorageMigrationMode,
    require_clean: bool,
) -> MemcoreResult<StorageStartupCheckReport> {
    let pool = connect_sqlite_pool(database_url).await?;
    check_sqlite_pool_startup(&pool, mode, require_clean).await
}

/// Opens a SQLite pool with the write settings memcore needs.
///
/// SQLite's defaults are a rollback journal with `synchronous=FULL`, which costs
/// a full fsync plus a journal file create/delete on every write transaction, and
/// blocks readers while a writer holds the lock. memcore never overrode them.
/// Measured on the baseline hardware, the same disk commits ~78 write
/// transactions/second that way, ~392 with WAL, and ~8,000 with WAL plus
/// `synchronous=NORMAL`.
///
/// WAL is durable across a crash; `synchronous=NORMAL` trades a small window of
/// the most recent commits on *power loss* (not process crash) for that
/// throughput, which is the usual production setting for WAL databases.
///
/// In-memory databases keep the previous single-connection setup: each pooled
/// connection would otherwise get its own empty database, and WAL does not apply
/// to them.
/// Connection options for a SQLite database, with memcore's write settings
/// applied to file-backed databases.
///
/// Callers that need their own pool sizing use this and build the pool
/// themselves; everything else should use [`connect_sqlite_pool`].
#[cfg(feature = "sqlite")]
pub fn sqlite_connect_options(
    database_url: &str,
) -> MemcoreResult<sqlx::sqlite::SqliteConnectOptions> {
    use std::str::FromStr;

    let url = normalize_sqlite_url(database_url);
    // Built from the URL so its parameters (e.g. `mode=rwc`) keep working exactly
    // as before; only the journal and fsync settings are added.
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&url).map_err(|error| {
        MemcoreError::StorageError(format!("invalid sqlite database url: {error}"))
    })?;

    if url.contains(":memory:") {
        // WAL does not apply to an in-memory database.
        return Ok(options);
    }

    Ok(options
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(5)))
}

/// Opens a SQLite pool. In-memory databases keep the single-connection setup —
/// each pooled connection would otherwise get its own empty database.
#[cfg(feature = "sqlite")]
pub async fn connect_sqlite_pool(database_url: &str) -> MemcoreResult<sqlx::sqlite::SqlitePool> {
    let url = normalize_sqlite_url(database_url);
    let options = sqlite_connect_options(database_url)?;

    if url.contains(":memory:") {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
    } else {
        sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(options)
            .await
    }
    .map_err(|error| {
        MemcoreError::StorageError(format!("failed to connect sqlite database: {error}"))
    })
}

#[cfg(feature = "sqlite")]
pub async fn check_sqlite_pool_startup(
    pool: &sqlx::sqlite::SqlitePool,
    mode: StorageMigrationMode,
    require_clean: bool,
) -> MemcoreResult<StorageStartupCheckReport> {
    let runner = SqliteMigrationRunner::new(pool.clone());
    let migrations = sqlite_migrations();
    let mut warnings = Vec::new();

    let migration_report = match mode {
        StorageMigrationMode::Disabled => {
            warnings.push("database migration validation is disabled".to_string());
            None
        }
        StorageMigrationMode::ValidateOnly => {
            let report = runner.validate_migrations(&migrations).await?;
            if require_clean && !report.clean {
                return Err(migration_failure(&report));
            }
            Some(report)
        }
        StorageMigrationMode::Auto => {
            let report = runner.apply_pending_migrations(&migrations).await?;
            if require_clean && !report.clean {
                return Err(migration_failure(&report));
            }
            Some(report)
        }
    };

    let mut migrations_clean = migration_report
        .as_ref()
        .map(|report| report.clean)
        .unwrap_or(true);

    if mode != StorageMigrationMode::Disabled {
        let missing = crate::migrations::sqlite::required_sqlite_tables_exist(pool).await?;
        if !missing.is_empty() {
            migrations_clean = false;
            warnings.push(format!("required tables missing: {}", missing.join(", ")));
            if require_clean {
                return Err(MemcoreError::MigrationError(
                    "required table missing after migration".to_string(),
                ));
            }
        }
    }

    Ok(StorageStartupCheckReport {
        database_connected: true,
        migrations_clean,
        migration_report,
        warnings,
    })
}

#[cfg(feature = "postgres")]
pub async fn check_postgres_startup(
    database_url: &str,
    mode: StorageMigrationMode,
    require_clean: bool,
) -> MemcoreResult<StorageStartupCheckReport> {
    let pool = connect_postgres_pool(database_url).await?;
    check_postgres_pool_startup(&pool, mode, require_clean).await
}

#[cfg(feature = "postgres")]
pub async fn connect_postgres_pool(database_url: &str) -> MemcoreResult<sqlx::postgres::PgPool> {
    sqlx::postgres::PgPool::connect(database_url)
        .await
        .map_err(|error| {
            MemcoreError::StorageError(format!("failed to connect postgres database: {error}"))
        })
}

#[cfg(feature = "postgres")]
pub async fn check_postgres_pool_startup(
    pool: &sqlx::postgres::PgPool,
    mode: StorageMigrationMode,
    require_clean: bool,
) -> MemcoreResult<StorageStartupCheckReport> {
    let runner = PostgresMigrationRunner::new(pool.clone());
    let migrations = postgres_migrations();
    let mut warnings = Vec::new();

    let migration_report = match mode {
        StorageMigrationMode::Disabled => {
            warnings.push("database migration validation is disabled".to_string());
            None
        }
        StorageMigrationMode::ValidateOnly => {
            let report = runner.validate_migrations(&migrations).await?;
            if require_clean && !report.clean {
                return Err(migration_failure(&report));
            }
            Some(report)
        }
        StorageMigrationMode::Auto => {
            let report = runner.apply_pending_migrations(&migrations).await?;
            if require_clean && !report.clean {
                return Err(migration_failure(&report));
            }
            Some(report)
        }
    };

    let mut migrations_clean = migration_report
        .as_ref()
        .map(|report| report.clean)
        .unwrap_or(true);

    if mode != StorageMigrationMode::Disabled {
        let missing = crate::migrations::postgres::required_postgres_tables_exist(pool).await?;
        if !missing.is_empty() {
            migrations_clean = false;
            warnings.push(format!("required tables missing: {}", missing.join(", ")));
            if require_clean {
                return Err(MemcoreError::MigrationError(
                    "required table missing after migration".to_string(),
                ));
            }
        }
    }

    Ok(StorageStartupCheckReport {
        database_connected: true,
        migrations_clean,
        migration_report,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    #[tokio::test]
    async fn database_connected_and_clean_after_auto_migrations() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect");

        let report = check_sqlite_pool_startup(&pool, StorageMigrationMode::Auto, true)
            .await
            .expect("startup");

        assert!(report.database_connected);
        assert!(report.migrations_clean);
        assert_eq!(
            report
                .migration_report
                .as_ref()
                .expect("migration report")
                .pending_count,
            0
        );
    }

    #[tokio::test]
    async fn validate_only_does_not_apply_pending_migrations() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect");

        let report = check_sqlite_pool_startup(&pool, StorageMigrationMode::ValidateOnly, false)
            .await
            .expect("startup");

        assert!(!report.migrations_clean);
        assert!(
            report
                .migration_report
                .as_ref()
                .expect("migration report")
                .pending_count
                > 0
        );
        assert!(
            !crate::migrations::sqlite::required_sqlite_tables_exist(&pool)
                .await
                .expect("tables")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn validate_only_with_clean_requirement_fails_on_pending_migrations() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect");

        let error = check_sqlite_pool_startup(&pool, StorageMigrationMode::ValidateOnly, true)
            .await
            .expect_err("pending migrations should fail");

        assert_eq!(error.code(), "migration_error");
    }

    #[tokio::test]
    async fn disabled_mode_skips_migration_validation() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect");

        let report = check_sqlite_pool_startup(&pool, StorageMigrationMode::Disabled, true)
            .await
            .expect("startup");

        assert!(report.database_connected);
        assert!(report.migrations_clean);
        assert!(report.migration_report.is_none());
        assert!(!report.warnings.is_empty());
    }
}
