use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
    pub checksum: String,
}

impl Migration {
    pub fn new(version: i64, name: &'static str, sql: &'static str) -> Self {
        Self {
            version,
            name,
            sql,
            checksum: migration_checksum(sql),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppliedMigration {
    pub version: i64,
    pub name: String,
    pub checksum: String,
    pub applied_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MigrationStatus {
    Applied,
    Pending,
    ChecksumMismatch,
    MissingFromCode,
}

impl MigrationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Pending => "pending",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::MissingFromCode => "missing_from_code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationIssue {
    pub version: i64,
    pub name: String,
    pub status: MigrationStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationValidationReport {
    pub clean: bool,
    pub applied_count: usize,
    pub pending_count: usize,
    pub issues: Vec<MigrationIssue>,
}

impl MigrationValidationReport {
    pub fn has_blocking_issues(&self) -> bool {
        self.issues.iter().any(|issue| {
            matches!(
                issue.status,
                MigrationStatus::ChecksumMismatch | MigrationStatus::MissingFromCode
            )
        })
    }
}

#[async_trait]
pub trait MigrationRunner: Send + Sync {
    async fn ensure_migration_table(&self) -> MemcoreResult<()>;

    async fn applied_migrations(&self) -> MemcoreResult<Vec<AppliedMigration>>;

    async fn validate_migrations(
        &self,
        migrations: &[Migration],
    ) -> MemcoreResult<MigrationValidationReport>;

    async fn apply_pending_migrations(
        &self,
        migrations: &[Migration],
    ) -> MemcoreResult<MigrationValidationReport>;
}

pub fn migration_checksum(sql: &str) -> String {
    let normalized = sql.replace("\r\n", "\n");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn sorted_migrations(migrations: &[Migration]) -> MemcoreResult<Vec<Migration>> {
    let mut seen = HashSet::new();
    let mut sorted = migrations.to_vec();
    sorted.sort_by_key(|migration| migration.version);

    for migration in &sorted {
        if !seen.insert(migration.version) {
            return Err(MemcoreError::MigrationError(format!(
                "duplicate migration version: {}",
                migration.version
            )));
        }
    }

    Ok(sorted)
}

pub fn validate_applied_migrations(
    migrations: &[Migration],
    applied: &[AppliedMigration],
) -> MemcoreResult<MigrationValidationReport> {
    let migrations = sorted_migrations(migrations)?;
    let coded_by_version = migrations
        .iter()
        .map(|migration| (migration.version, migration))
        .collect::<HashMap<_, _>>();
    let applied_by_version = applied
        .iter()
        .map(|migration| (migration.version, migration))
        .collect::<HashMap<_, _>>();

    let mut issues = Vec::new();
    let mut pending_count = 0usize;

    for migration in &migrations {
        match applied_by_version.get(&migration.version) {
            Some(applied) if applied.checksum != migration.checksum => {
                issues.push(MigrationIssue {
                    version: migration.version,
                    name: migration.name.to_string(),
                    status: MigrationStatus::ChecksumMismatch,
                    message: "migration checksum mismatch".to_string(),
                });
            }
            Some(_) => {}
            None => {
                pending_count += 1;
                issues.push(MigrationIssue {
                    version: migration.version,
                    name: migration.name.to_string(),
                    status: MigrationStatus::Pending,
                    message: "migration is pending".to_string(),
                });
            }
        }
    }

    for applied in applied {
        if !coded_by_version.contains_key(&applied.version) {
            issues.push(MigrationIssue {
                version: applied.version,
                name: applied.name.clone(),
                status: MigrationStatus::MissingFromCode,
                message: "applied migration is missing from code".to_string(),
            });
        }
    }

    Ok(MigrationValidationReport {
        clean: issues.is_empty(),
        applied_count: applied.len(),
        pending_count,
        issues,
    })
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
/// Splits a migration into statements on `;`. A `CREATE TRIGGER ... BEGIN ... END` block contains
/// semicolons of its own, so it is kept whole: it runs up to the first segment that ends in `END`.
pub(crate) fn split_sql_statements(sql: &str) -> impl Iterator<Item = String> + '_ {
    let mut statements = Vec::new();
    let mut pending: Option<String> = None;
    for segment in sql.split(';') {
        let segment = segment.trim();
        if let Some(mut trigger) = pending.take() {
            trigger.push_str(";\n");
            trigger.push_str(segment);
            if ends_trigger_body(segment) {
                statements.push(trigger);
            } else {
                pending = Some(trigger);
            }
            continue;
        }
        if segment.is_empty() {
            continue;
        }
        if starts_trigger(segment) && !ends_trigger_body(segment) {
            pending = Some(segment.to_string());
        } else {
            statements.push(segment.to_string());
        }
    }
    if let Some(trigger) = pending {
        statements.push(trigger);
    }
    statements.into_iter()
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
fn starts_trigger(statement: &str) -> bool {
    let words: Vec<String> = statement
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .flat_map(str::split_whitespace)
        .take(4)
        .map(str::to_ascii_uppercase)
        .collect();
    let rest = match words.first().map(String::as_str) {
        Some("CREATE") => &words[1..],
        _ => return false,
    };
    let rest = match rest.first().map(String::as_str) {
        Some("TEMP" | "TEMPORARY") => &rest[1..],
        _ => rest,
    };
    rest.first().is_some_and(|word| word == "TRIGGER")
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
fn ends_trigger_body(segment: &str) -> bool {
    segment
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("--"))
        .is_some_and(|line| line.eq_ignore_ascii_case("END"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration(version: i64, sql: &'static str) -> Migration {
        Migration::new(version, "test", sql)
    }

    #[test]
    fn checksum_is_stable_and_normalizes_line_endings() {
        assert_eq!(
            migration_checksum("SELECT 1;\n"),
            migration_checksum("SELECT 1;\r\n")
        );
        assert_ne!(
            migration_checksum("SELECT 1;"),
            migration_checksum("SELECT 2;")
        );
    }

    #[test]
    fn trigger_bodies_are_kept_whole() {
        let sql = "-- leading comment\nCREATE TABLE t (a TEXT);\n-- a trigger follows\n\
                   CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n    INSERT INTO u VALUES (new.a);\n\
                   \x20   INSERT INTO v VALUES (new.a);\nEND;\nINSERT INTO t VALUES ('x');\n";
        let statements: Vec<String> = split_sql_statements(sql).collect();
        assert_eq!(statements.len(), 3, "{statements:?}");
        assert!(statements[0].ends_with("CREATE TABLE t (a TEXT)"));
        assert!(statements[1].contains("CREATE TRIGGER tr"));
        assert!(statements[1].contains("INSERT INTO u VALUES (new.a);"));
        assert!(statements[1].ends_with("END"));
        assert_eq!(statements[2], "INSERT INTO t VALUES ('x')");
    }

    #[test]
    fn statements_without_triggers_split_as_before() {
        let statements: Vec<String> = split_sql_statements("SELECT 1; ;\n SELECT 2;").collect();
        assert_eq!(statements, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn migrations_sort_by_version() {
        let sorted = sorted_migrations(&[migration(2, "SELECT 2;"), migration(1, "SELECT 1;")])
            .expect("sort");
        assert_eq!(sorted[0].version, 1);
        assert_eq!(sorted[1].version, 2);
    }

    #[test]
    fn duplicate_migration_versions_are_detected() {
        let error = sorted_migrations(&[migration(1, "SELECT 1;"), migration(1, "SELECT 2;")])
            .expect_err("duplicate should fail");
        assert_eq!(error.code(), "migration_error");
    }

    #[test]
    fn validation_reports_pending_checksum_and_missing_issues() {
        let coded = vec![migration(1, "SELECT 1;"), migration(2, "SELECT 2;")];
        let applied = vec![
            AppliedMigration {
                version: 1,
                name: "test".to_string(),
                checksum: "different".to_string(),
                applied_at: Utc::now(),
            },
            AppliedMigration {
                version: 3,
                name: "removed".to_string(),
                checksum: migration_checksum("SELECT 3;"),
                applied_at: Utc::now(),
            },
        ];

        let report = validate_applied_migrations(&coded, &applied).expect("validate");
        assert!(!report.clean);
        assert_eq!(report.pending_count, 1);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.status == MigrationStatus::ChecksumMismatch)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.status == MigrationStatus::Pending)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.status == MigrationStatus::MissingFromCode)
        );
    }
}
