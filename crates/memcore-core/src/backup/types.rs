use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Relational database backend targeted by an operational backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupBackend {
    Sqlite,
    Postgres,
}

impl BackupBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }
}

/// Request to create a local database backup file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRequest {
    pub backend: BackupBackend,
    pub output_dir: PathBuf,
    pub label: Option<String>,
}

/// Metadata returned after a successful backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupResult {
    pub backend: BackupBackend,
    pub backup_path: PathBuf,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
    pub checksum_sha256: String,
}

/// Result of validating a backup file without restoring it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreValidationResult {
    pub valid: bool,
    pub backend: BackupBackend,
    pub backup_path: PathBuf,
    pub size_bytes: u64,
    pub checksum_sha256: String,
    pub issues: Vec<String>,
}

/// Explicit confirmation required for any future destructive restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreConfirmation {
    pub confirmation: String,
}

impl RestoreConfirmation {
    pub const REQUIRED_PHRASE: &'static str = "I_UNDERSTAND_THIS_WILL_REPLACE_THE_DATABASE";

    pub fn is_valid(&self) -> bool {
        self.confirmation.trim() == Self::REQUIRED_PHRASE
    }
}

/// Placeholder result for a future guarded restore implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResult {
    pub backend: BackupBackend,
    pub backup_path: PathBuf,
    pub restored_at: DateTime<Utc>,
}
