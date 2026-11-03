use std::path::Path;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{BackupBackend, BackupRequest, BackupResult, RestoreValidationResult};

use super::DatabaseBackupProvider;

/// Placeholder Postgres backup provider.
///
/// Production Postgres backups should use managed provider snapshots or `pg_dump`.
/// This type exists so feature-gated compiles can depend on a clear not-implemented path.
#[derive(Debug, Default, Clone)]
pub struct PostgresBackupProvider;

impl PostgresBackupProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DatabaseBackupProvider for PostgresBackupProvider {
    async fn create_backup(&self, _request: BackupRequest) -> MemcoreResult<BackupResult> {
        Err(MemcoreError::ValidationError(
            "postgres database backup is not implemented; use managed snapshots or pg_dump"
                .to_string(),
        ))
    }

    async fn validate_backup(&self, backup_path: &Path) -> MemcoreResult<RestoreValidationResult> {
        Ok(RestoreValidationResult {
            valid: false,
            backend: BackupBackend::Postgres,
            backup_path: backup_path.to_path_buf(),
            size_bytes: 0,
            checksum_sha256: String::new(),
            issues: vec![
                "postgres backup validation is not implemented; use managed snapshots or pg_dump"
                    .to_string(),
            ],
        })
    }
}
