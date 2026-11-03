use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use sha2::{Digest, Sha256};

use super::types::BackupBackend;

pub const BACKUP_FILE_PREFIX: &str = "memcore-sqlite-";
pub const BACKUP_FILE_SUFFIX: &str = ".backup.sqlite";

/// Sanitize an optional backup label for safe filenames.
///
/// Removes path separators and keeps only ASCII alphanumeric characters,
/// dashes, and underscores. Empty results become `None`.
pub fn sanitize_backup_label(label: Option<&str>) -> Option<String> {
    let raw = label.map(str::trim).filter(|value| !value.is_empty())?;

    let sanitized: String = raw
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect();

    let collapsed = sanitized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// Build a timestamped SQLite backup filename.
pub fn build_sqlite_backup_filename(created_at: DateTime<Utc>, label: Option<&str>) -> String {
    let timestamp = created_at.format("%Y%m%dT%H%M%SZ");
    match sanitize_backup_label(label) {
        Some(safe_label) => {
            format!("{BACKUP_FILE_PREFIX}{timestamp}-{safe_label}{BACKUP_FILE_SUFFIX}")
        }
        None => format!("{BACKUP_FILE_PREFIX}{timestamp}{BACKUP_FILE_SUFFIX}"),
    }
}

/// Ensure `output_dir` is non-empty and does not contain path traversal components.
pub fn validate_backup_output_dir(output_dir: &Path) -> MemcoreResult<PathBuf> {
    if output_dir.as_os_str().is_empty() {
        return Err(MemcoreError::ValidationError(
            "backup output directory cannot be empty".to_string(),
        ));
    }

    for component in output_dir.components() {
        if matches!(component, Component::ParentDir) {
            return Err(MemcoreError::ValidationError(
                "backup output directory cannot contain path traversal".to_string(),
            ));
        }
    }

    Ok(output_dir.to_path_buf())
}

/// Resolve the final backup path inside `output_dir`.
pub fn resolve_backup_path(
    output_dir: &Path,
    created_at: DateTime<Utc>,
    label: Option<&str>,
) -> MemcoreResult<PathBuf> {
    let output_dir = validate_backup_output_dir(output_dir)?;
    let filename = build_sqlite_backup_filename(created_at, label);
    let path = output_dir.join(&filename);

    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| name != filename)
    {
        return Err(MemcoreError::ValidationError(
            "backup filename escaped output directory".to_string(),
        ));
    }

    if !path.starts_with(&output_dir) {
        return Err(MemcoreError::ValidationError(
            "backup path must stay inside backup directory".to_string(),
        ));
    }

    Ok(path)
}

pub fn is_memcore_sqlite_backup_filename(name: &str) -> bool {
    name.starts_with(BACKUP_FILE_PREFIX) && name.ends_with(BACKUP_FILE_SUFFIX)
}

/// SHA-256 hex digest of a file's contents.
pub fn file_sha256_hex(path: &Path) -> MemcoreResult<String> {
    let mut file = fs::File::open(path).map_err(|error| {
        MemcoreError::StorageError(format!("failed to open backup file for checksum: {error}"))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            MemcoreError::StorageError(format!("failed to read backup file for checksum: {error}"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Remove oldest memcore SQLite backup files when count exceeds `max_files`.
///
/// Only deletes files matching the memcore backup filename pattern.
pub fn rotate_memcore_sqlite_backups(output_dir: &Path, max_files: usize) -> MemcoreResult<usize> {
    if max_files == 0 {
        return Err(MemcoreError::ValidationError(
            "backup_max_files must be greater than 0".to_string(),
        ));
    }

    if !output_dir.exists() {
        return Ok(0);
    }

    let mut backups = Vec::new();
    for entry in fs::read_dir(output_dir).map_err(|error| {
        MemcoreError::StorageError(format!("failed to read backup directory: {error}"))
    })? {
        let entry = entry.map_err(|error| {
            MemcoreError::StorageError(format!("failed to read backup directory entry: {error}"))
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_memcore_sqlite_backup_filename(name) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        backups.push((modified, path));
    }

    backups.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    if backups.len() <= max_files {
        return Ok(0);
    }

    let remove_count = backups.len() - max_files;
    let mut deleted = 0usize;
    for (_, path) in backups.into_iter().take(remove_count) {
        fs::remove_file(&path).map_err(|error| {
            MemcoreError::StorageError(format!("failed to rotate backup file: {error}"))
        })?;
        deleted += 1;
    }
    Ok(deleted)
}

pub fn backend_label(backend: BackupBackend) -> &'static str {
    backend.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sanitized_label_removes_unsafe_characters() {
        assert_eq!(
            sanitize_backup_label(Some("../before migration!!")),
            Some("before-migration".to_string())
        );
        assert_eq!(sanitize_backup_label(Some("///")), None);
        assert_eq!(
            sanitize_backup_label(Some("before-migration")),
            Some("before-migration".to_string())
        );
    }

    #[test]
    fn backup_filename_includes_timestamp_and_optional_label() {
        let created_at = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        assert_eq!(
            build_sqlite_backup_filename(created_at, None),
            "memcore-sqlite-20260618T100000Z.backup.sqlite"
        );
        assert_eq!(
            build_sqlite_backup_filename(created_at, Some("before-migration")),
            "memcore-sqlite-20260618T100000Z-before-migration.backup.sqlite"
        );
    }

    #[test]
    fn backup_path_stays_inside_backup_directory() {
        let created_at = Utc.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap();
        let path = resolve_backup_path(Path::new("./backups"), created_at, Some("ok")).unwrap();
        assert!(path.starts_with("./backups"));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("memcore-sqlite-20260618T100000Z-ok.backup.sqlite")
        );
    }

    #[test]
    fn path_traversal_in_output_dir_is_rejected() {
        let error = validate_backup_output_dir(Path::new("./backups/../outside"))
            .expect_err("traversal should fail");
        assert!(error.to_string().contains("path traversal"));
    }

    #[test]
    fn rotation_removes_oldest_memcore_backups_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unrelated = dir.path().join("notes.txt");
        fs::write(&unrelated, b"keep me").unwrap();

        for index in 0..3 {
            let path = dir.path().join(format!(
                "memcore-sqlite-20260618T10000{index}Z.backup.sqlite"
            ));
            fs::write(&path, format!("backup-{index}")).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let deleted = rotate_memcore_sqlite_backups(dir.path(), 2).unwrap();
        assert_eq!(deleted, 1);
        assert!(unrelated.exists());

        let remaining = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(is_memcore_sqlite_backup_filename)
            })
            .count();
        assert_eq!(remaining, 2);
    }
}
