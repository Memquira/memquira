use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    BackupBackend, BackupRequest, BackupResult, RestoreValidationResult, file_sha256_hex,
    resolve_backup_path, rotate_memcore_sqlite_backups,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use super::DatabaseBackupProvider;

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

/// Extract a filesystem path from a SQLite database URL.
///
/// Returns `None` for in-memory databases.
pub fn sqlite_path_from_database_url(database_url: &str) -> MemcoreResult<Option<PathBuf>> {
    let normalized = if let Some(rest) = database_url.strip_prefix("sqlite://") {
        format!("sqlite:{rest}")
    } else {
        database_url.to_string()
    };

    let without_scheme = normalized
        .strip_prefix("sqlite:")
        .unwrap_or(normalized.as_str());

    if without_scheme.is_empty() || without_scheme.starts_with(":memory:") {
        return Ok(None);
    }

    let path_part = without_scheme
        .split('?')
        .next()
        .unwrap_or(without_scheme)
        .trim();
    if path_part.is_empty() || path_part == ":memory:" {
        return Ok(None);
    }

    Ok(Some(PathBuf::from(path_part)))
}

fn escape_sqlite_path_literal(path: &Path) -> MemcoreResult<String> {
    let path_str = path.to_str().ok_or_else(|| {
        MemcoreError::ValidationError("backup path contains invalid UTF-8".to_string())
    })?;
    Ok(path_str.replace('\'', "''"))
}

/// SQLite operational backup provider.
///
/// Prefers `VACUUM INTO` for a consistent snapshot. Falls back to filesystem copy
/// when the source path is known and `VACUUM INTO` is unavailable.
#[derive(Debug, Clone)]
pub struct SqliteBackupProvider {
    database_url: String,
    max_files: usize,
}

impl SqliteBackupProvider {
    pub fn new(database_url: impl Into<String>, max_files: usize) -> MemcoreResult<Self> {
        if max_files == 0 {
            return Err(MemcoreError::ValidationError(
                "backup_max_files must be greater than 0".to_string(),
            ));
        }
        Ok(Self {
            database_url: database_url.into(),
            max_files,
        })
    }

    async fn connect_source(&self) -> MemcoreResult<SqlitePool> {
        let options = if let Some(path) = sqlite_path_from_database_url(&self.database_url)? {
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
        } else {
            self.database_url
                .parse::<SqliteConnectOptions>()
                .map_err(|error| storage_error("parse sqlite database url", error))?
                .create_if_missing(true)
        };

        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|error| storage_error("connect sqlite database for backup", error))
    }

    async fn vacuum_into(pool: &SqlitePool, backup_path: &Path) -> MemcoreResult<()> {
        let escaped = escape_sqlite_path_literal(backup_path)?;
        let sql = format!("VACUUM INTO '{escaped}'");
        sqlx::query(&sql)
            .execute(pool)
            .await
            .map_err(|error| storage_error("sqlite vacuum into backup", error))?;
        Ok(())
    }

    fn copy_database_file(source: &Path, backup_path: &Path) -> MemcoreResult<()> {
        if !source.exists() {
            return Err(MemcoreError::NotFound(
                "sqlite database file was not found for backup".to_string(),
            ));
        }
        fs::copy(source, backup_path)
            .map_err(|error| storage_error("copy sqlite database file", error))?;
        Ok(())
    }

    async fn create_backup_file(&self, backup_path: &Path) -> MemcoreResult<()> {
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| storage_error("create backup directory", error))?;
        }

        if backup_path.exists() {
            return Err(MemcoreError::Conflict(
                "backup file already exists".to_string(),
            ));
        }

        let pool = self.connect_source().await?;
        match Self::vacuum_into(&pool, backup_path).await {
            Ok(()) => Ok(()),
            Err(vacuum_error) => {
                let source_path = sqlite_path_from_database_url(&self.database_url)?;
                match source_path {
                    Some(source) => {
                        tracing::warn!(
                            error_code = vacuum_error.code(),
                            "sqlite vacuum into failed; falling back to file copy"
                        );
                        if backup_path.exists() {
                            let _ = fs::remove_file(backup_path);
                        }
                        Self::copy_database_file(&source, backup_path)
                    }
                    None => Err(vacuum_error),
                }
            }
        }
    }

    async fn validate_sqlite_tables(backup_path: &Path) -> MemcoreResult<Vec<String>> {
        let mut issues = Vec::new();
        let options = SqliteConnectOptions::new()
            .filename(backup_path)
            .read_only(true);

        let pool = match SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
        {
            Ok(pool) => pool,
            Err(error) => {
                issues.push(format!("failed to open backup read-only: {error}"));
                return Ok(issues);
            }
        };

        let required = ["schema_migrations", "facts", "memory_events", "api_keys"];
        for table in required {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(1) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await;

            match exists {
                Ok(count) if count > 0 => {}
                Ok(_) => issues.push(format!("required table missing: {table}")),
                Err(error) => issues.push(format!("failed checking table {table}: {error}")),
            }
        }

        Ok(issues)
    }
}

#[async_trait]
impl DatabaseBackupProvider for SqliteBackupProvider {
    async fn create_backup(&self, request: BackupRequest) -> MemcoreResult<BackupResult> {
        if request.backend != BackupBackend::Sqlite {
            return Err(MemcoreError::ValidationError(
                "SqliteBackupProvider only supports sqlite backups".to_string(),
            ));
        }

        let created_at = Utc::now();
        let backup_path =
            resolve_backup_path(&request.output_dir, created_at, request.label.as_deref())?;

        self.create_backup_file(&backup_path).await?;

        let metadata = fs::metadata(&backup_path)
            .map_err(|error| storage_error("read backup file metadata", error))?;
        let size_bytes = metadata.len();
        if size_bytes == 0 {
            let _ = fs::remove_file(&backup_path);
            return Err(MemcoreError::StorageError(
                "created backup file was empty".to_string(),
            ));
        }

        let checksum_sha256 = file_sha256_hex(&backup_path)?;
        rotate_memcore_sqlite_backups(&request.output_dir, self.max_files)?;

        tracing::info!(
            backend = "sqlite",
            size_bytes,
            "sqlite database backup created"
        );

        Ok(BackupResult {
            backend: BackupBackend::Sqlite,
            backup_path,
            size_bytes,
            created_at,
            checksum_sha256,
        })
    }

    async fn validate_backup(&self, backup_path: &Path) -> MemcoreResult<RestoreValidationResult> {
        let mut issues = Vec::new();
        let mut size_bytes = 0_u64;
        let mut checksum_sha256 = String::new();

        if !backup_path.exists() {
            issues.push("backup file does not exist".to_string());
        } else if !backup_path.is_file() {
            issues.push("backup path is not a file".to_string());
        } else {
            match fs::metadata(backup_path) {
                Ok(metadata) => {
                    size_bytes = metadata.len();
                    if size_bytes == 0 {
                        issues.push("backup file is empty".to_string());
                    }
                }
                Err(error) => issues.push(format!("failed to read backup metadata: {error}")),
            }

            match file_sha256_hex(backup_path) {
                Ok(checksum) => checksum_sha256 = checksum,
                Err(error) => issues.push(format!("failed to compute checksum: {error}")),
            }

            if issues.is_empty() {
                issues.extend(Self::validate_sqlite_tables(backup_path).await?);
            }
        }

        Ok(RestoreValidationResult {
            valid: issues.is_empty(),
            backend: BackupBackend::Sqlite,
            backup_path: backup_path.to_path_buf(),
            size_bytes,
            checksum_sha256,
            issues,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore_core::is_memcore_sqlite_backup_filename;
    use tempfile::tempdir;

    async fn seeded_sqlite_db(path: &Path) -> String {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("connect");
        crate::migrations::sqlite::run_sqlite_migrations(&pool)
            .await
            .expect("migrate");
        pool.close().await;
        // Prefer sqlite:// style used by Settings so path extraction remains covered.
        format!("sqlite://{}", path.display().to_string().replace('\\', "/"))
    }

    #[test]
    fn sqlite_path_extraction_handles_urls_and_memory() {
        assert_eq!(
            sqlite_path_from_database_url("sqlite://./data/memcore.db")
                .unwrap()
                .unwrap(),
            PathBuf::from("./data/memcore.db")
        );
        assert!(
            sqlite_path_from_database_url("sqlite::memory:")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn creates_non_empty_backup_with_checksum_and_validation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("source.db");
        let url = seeded_sqlite_db(&db_path).await;
        let backups = dir.path().join("backups");

        let provider = SqliteBackupProvider::new(&url, 10).unwrap();
        let result = provider
            .create_backup(BackupRequest {
                backend: BackupBackend::Sqlite,
                output_dir: backups.clone(),
                label: Some("before-migration".to_string()),
            })
            .await
            .expect("backup");

        assert!(result.backup_path.exists());
        assert!(result.size_bytes > 0);
        assert!(!result.checksum_sha256.is_empty());
        assert!(
            result
                .backup_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_memcore_sqlite_backup_filename)
        );

        let validation = provider
            .validate_backup(&result.backup_path)
            .await
            .expect("validate");
        assert!(validation.valid, "{:?}", validation.issues);
        assert_eq!(validation.checksum_sha256, result.checksum_sha256);
    }

    #[tokio::test]
    async fn validation_fails_for_missing_and_empty_backups() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("source.db");
        let url = seeded_sqlite_db(&db_path).await;
        let provider = SqliteBackupProvider::new(&url, 10).unwrap();

        let missing = provider
            .validate_backup(&dir.path().join("missing.backup.sqlite"))
            .await
            .unwrap();
        assert!(!missing.valid);
        assert!(
            missing
                .issues
                .iter()
                .any(|issue| issue.contains("does not exist"))
        );

        let empty_path = dir.path().join("empty.backup.sqlite");
        fs::write(&empty_path, b"").unwrap();
        let empty = provider.validate_backup(&empty_path).await.unwrap();
        assert!(!empty.valid);
        assert!(empty.issues.iter().any(|issue| issue.contains("empty")));
    }

    #[tokio::test]
    async fn rotation_keeps_max_files_and_preserves_unrelated() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("source.db");
        let url = seeded_sqlite_db(&db_path).await;
        let backups = dir.path().join("backups");
        fs::create_dir_all(&backups).unwrap();
        let unrelated = backups.join("notes.txt");
        fs::write(&unrelated, b"keep").unwrap();

        let provider = SqliteBackupProvider::new(&url, 2).unwrap();
        for label in ["a", "b", "c"] {
            provider
                .create_backup(BackupRequest {
                    backend: BackupBackend::Sqlite,
                    output_dir: backups.clone(),
                    label: Some(label.to_string()),
                })
                .await
                .expect("backup");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let memcore_count = fs::read_dir(&backups)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(is_memcore_sqlite_backup_filename)
            })
            .count();
        assert_eq!(memcore_count, 2);
        assert!(unrelated.exists());
    }
}
