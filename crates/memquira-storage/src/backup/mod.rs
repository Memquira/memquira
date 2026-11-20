use async_trait::async_trait;
use memquira_common::MemquiraResult;
use memquira_core::{BackupRequest, BackupResult, RestoreValidationResult};
use std::path::Path;

#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteBackupProvider, sqlite_path_from_database_url};

#[cfg(feature = "postgres")]
pub use postgres::PostgresBackupProvider;

/// Creates and validates local database backups.
#[async_trait]
pub trait DatabaseBackupProvider: Send + Sync {
    async fn create_backup(&self, request: BackupRequest) -> MemquiraResult<BackupResult>;

    async fn validate_backup(&self, backup_path: &Path) -> MemquiraResult<RestoreValidationResult>;
}
