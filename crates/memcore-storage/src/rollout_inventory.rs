//! A read-only inventory of a deployment's SQL fact store, for the
//! per-deployment rollout prerequisites (legacy UUIDv8 fact ids, schema, state fingerprint).
//!
//! Strictly read-only, and independent of the candidate's migrations, so it also works on a
//! schema older than the candidate:
//! - SQLite is opened `read_only` + `immutable` (no journal-mode pragma, no `-shm` / `-wal`
//!   creation, no lock). `immutable` ignores a WAL, so a non-empty `-wal` file is refused: inspect a
//!   checkpointed snapshot instead.
//! - PostgreSQL runs every query in one `REPEATABLE READ READ ONLY` transaction on a session with
//!   `default_transaction_read_only = on`, and rolls it back.
//! - Nothing is created, migrated, seeded or written; only the catalog and `SELECT`s are used.
//!
//! The report carries counts, booleans and SHA-256 digests only: never a fact id, memory text,
//! tenant id or credential.

use std::collections::{BTreeMap, BTreeSet};

use memcore_common::{MemcoreError, MemcoreResult};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Columns the inventory needs in `facts` (present since migration 1, 0001_create_facts).
pub const REQUIRED_FACT_COLUMNS: &[&str] = &[
    "id",
    "org_id",
    "user_id",
    "content",
    "deleted_at",
    "updated_at",
];

/// The class of one fact id, on its canonical text (no substring inference).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdClass {
    V4,
    V8,
    OtherRfc,
    Nil,
    Max,
    NonRfc,
    NonCanonical,
    Invalid,
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn canonical_shape(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => is_lower_hex(*b),
        })
}

/// Classifies one id. `None` = not a text value (an integer or blob id in SQLite).
pub fn classify_fact_id(id: Option<&str>) -> IdClass {
    let Some(text) = id else {
        return IdClass::Invalid;
    };
    if canonical_shape(text) {
        if text == "00000000-0000-0000-0000-000000000000" {
            return IdClass::Nil;
        }
        if text == "ffffffff-ffff-ffff-ffff-ffffffffffff" {
            return IdClass::Max;
        }
        let (version, variant) = (text.as_bytes()[14], text.as_bytes()[19]);
        if !matches!(variant, b'8' | b'9' | b'a' | b'b') {
            return IdClass::NonRfc;
        }
        return match version {
            b'4' => IdClass::V4,
            b'8' => IdClass::V8,
            b'1'..=b'7' => IdClass::OtherRfc,
            _ => IdClass::NonRfc,
        };
    }
    if uuid::Uuid::parse_str(text.trim()).is_ok() {
        IdClass::NonCanonical
    } else {
        IdClass::Invalid
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct IdClassCounts {
    pub total: u64,
    pub v4: u64,
    pub v8: u64,
    pub other_rfc: u64,
    pub nil: u64,
    pub max: u64,
    pub nonrfc: u64,
    pub noncanonical: u64,
    pub invalid: u64,
}

impl IdClassCounts {
    fn add(&mut self, class: IdClass) {
        self.total += 1;
        match class {
            IdClass::V4 => self.v4 += 1,
            IdClass::V8 => self.v8 += 1,
            IdClass::OtherRfc => self.other_rfc += 1,
            IdClass::Nil => self.nil += 1,
            IdClass::Max => self.max += 1,
            IdClass::NonRfc => self.nonrfc += 1,
            IdClass::NonCanonical => self.noncanonical += 1,
            IdClass::Invalid => self.invalid += 1,
        }
    }

    /// Ids that are not a plainly classified RFC UUID (nil, max, non-RFC, non-canonical, invalid).
    pub fn unclassifiable(&self) -> u64 {
        self.nil + self.max + self.nonrfc + self.noncanonical + self.invalid
    }
}

/// One `facts` row as read for the inventory (content is only hashed).
#[derive(Debug, Clone)]
pub struct FactRow {
    pub id: Option<String>,
    pub org_id: String,
    pub user_id: String,
    pub updated_at: Option<String>,
    pub deleted: bool,
    pub superseded: bool,
    pub content: Option<String>,
}

/// One `episodes` row (hashed only).
#[derive(Debug, Clone)]
pub struct EpisodeRow {
    pub id: String,
    pub org_id: String,
    pub user_id: String,
    pub embedded: String,
    pub content: Option<String>,
}

/// What was read, before assessment.
#[derive(Debug, Clone, Default)]
pub struct RawInventory {
    pub backend: &'static str,
    pub tables: Vec<String>,
    pub fact_columns: Vec<String>,
    pub migration_table: Option<&'static str>,
    pub versions: Vec<i64>,
    pub table_row_counts: BTreeMap<String, i64>,
    pub facts: Vec<FactRow>,
    pub episodes: Option<Vec<EpisodeRow>>,
    pub incarnation: Option<String>,
    pub database_name: Option<String>,
    pub other_sessions: Option<i64>,
    pub read_only_verified: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DatabaseIdentity {
    pub backend: &'static str,
    /// PostgreSQL: `<system_identifier>:<database oid>` (changes when the cluster is re-created or
    /// the database is dropped and re-created). SQLite: none (a file has no incarnation).
    pub incarnation: Option<String>,
    /// SHA-256 of the database name (not the name itself).
    pub name_sha256: Option<String>,
    /// Other sessions connected to the same database (PostgreSQL), for the write freeze.
    pub other_sessions: Option<i64>,
    pub read_only_verified: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SchemaReport {
    pub migration_table: Option<&'static str>,
    pub versions: Vec<i64>,
    pub candidate_versions_max: i64,
    pub supported: bool,
    pub behind_candidate: bool,
    pub superseded_column: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FactInventory {
    pub total: u64,
    pub live: u64,
    pub soft_deleted: u64,
    /// `None` when the schema has no `superseded_at` column (before migration 13).
    pub superseded: Option<u64>,
    pub ids: IdClassCounts,
    pub ids_live: IdClassCounts,
    pub ids_soft_deleted: IdClassCounts,
    pub ids_superseded: Option<IdClassCounts>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Fingerprint {
    /// SHA-256 over the sorted per-row lines (id, tenant, `updated_at`, deletion and supersession
    /// flags, SHA-256 of the content) of every `facts` row.
    pub facts_sha256: String,
    /// The same over `episodes` (id, tenant, embedded flag, content hash), when present.
    pub episodes_sha256: Option<String>,
    /// SHA-256 over every table's name and row count.
    pub table_counts_sha256: String,
    /// SHA-256 over the schema versions.
    pub schema_sha256: String,
    /// The data and schema state only (no database identity): equal on a faithful restore copy.
    pub state_sha256: String,
    /// `state_sha256` bound to the backend and database incarnation: the value the deploy gate
    /// compares at cutover.
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TenantSet {
    pub count: u64,
    /// SHA-256 over the sorted `org_id \t user_id` lines (facts and episodes).
    pub sha256: String,
}

/// The assessment the preflight applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    /// First rollout of a C+ build onto pre-C+ data: legacy UUIDv8 fact ids must be absent.
    FirstCPlusRollout,
    /// A later upgrade of a deployment that already runs C+ (its own derived ids are v8): no
    /// v8 rule here; the gate requires the retained first-rollout record instead.
    LaterCPlusUpgrade,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InventoryReport {
    pub assessment: Assessment,
    pub database: DatabaseIdentity,
    pub schema: SchemaReport,
    pub facts: FactInventory,
    pub tenants: TenantSet,
    pub table_count: u64,
    pub fingerprint: Fingerprint,
    pub complete: bool,
    pub outcome: &'static str,
    pub reasons: Vec<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn digest_lines(mut lines: Vec<String>) -> String {
    lines.sort();
    sha256_hex(lines.join("\n").as_bytes())
}

/// Turns what was read into the report. Pure (unit-tested without a database).
pub fn assess(
    raw: RawInventory,
    candidate_versions: &[i64],
    assessment: Assessment,
) -> InventoryReport {
    let mut reasons: Vec<String> = raw.notes.clone();
    let candidate_max = candidate_versions.iter().copied().max().unwrap_or(0);

    // Schema: a known migration table whose versions are exactly 1..=k, k <= the candidate's.
    let missing: Vec<&str> = REQUIRED_FACT_COLUMNS
        .iter()
        .copied()
        .filter(|c| !raw.fact_columns.iter().any(|have| have == c))
        .collect();
    let contiguous = !raw.versions.is_empty()
        && raw
            .versions
            .iter()
            .enumerate()
            .all(|(i, v)| *v == i as i64 + 1)
        && raw.versions.iter().all(|v| candidate_versions.contains(v));
    let mut supported = true;
    if !raw.tables.iter().any(|t| t == "facts") {
        supported = false;
        reasons.push("schema: no facts table".to_string());
    } else if !missing.is_empty() {
        supported = false;
        reasons.push(format!(
            "schema: facts lacks required columns {}",
            missing.join(", ")
        ));
    }
    if raw.migration_table.is_none() {
        supported = false;
        reasons.push("schema: no migration table (schema version unknown)".to_string());
    } else if !contiguous {
        supported = false;
        reasons.push(
            "schema: migration versions are not a contiguous prefix of the candidate's".to_string(),
        );
    }
    let behind = supported && raw.versions.last().copied().unwrap_or(0) < candidate_max;
    let superseded_column = raw.fact_columns.iter().any(|c| c == "superseded_at");

    // Facts inventory.
    let (mut ids, mut ids_live, mut ids_deleted, mut ids_superseded) = (
        IdClassCounts::default(),
        IdClassCounts::default(),
        IdClassCounts::default(),
        IdClassCounts::default(),
    );
    let (mut live, mut deleted, mut superseded) = (0u64, 0u64, 0u64);
    let mut fact_lines = Vec::with_capacity(raw.facts.len());
    let mut tenants: BTreeSet<(String, String)> = BTreeSet::new();
    for row in &raw.facts {
        let class = classify_fact_id(row.id.as_deref());
        ids.add(class);
        if row.deleted {
            deleted += 1;
            ids_deleted.add(class);
        } else {
            live += 1;
            ids_live.add(class);
        }
        if row.superseded {
            superseded += 1;
            ids_superseded.add(class);
        }
        tenants.insert((row.org_id.clone(), row.user_id.clone()));
        fact_lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.id.as_deref().unwrap_or("<non-text>"),
            row.org_id,
            row.user_id,
            row.updated_at.as_deref().unwrap_or(""),
            row.deleted as u8,
            row.superseded as u8,
            sha256_hex(row.content.as_deref().unwrap_or("").as_bytes()),
        ));
    }
    let episodes_sha256 = raw.episodes.as_ref().map(|episodes| {
        let lines = episodes
            .iter()
            .map(|e| {
                tenants.insert((e.org_id.clone(), e.user_id.clone()));
                format!(
                    "{}\t{}\t{}\t{}\t{}",
                    e.id,
                    e.org_id,
                    e.user_id,
                    e.embedded,
                    sha256_hex(e.content.as_deref().unwrap_or("").as_bytes())
                )
            })
            .collect();
        digest_lines(lines)
    });
    let facts_sha256 = digest_lines(fact_lines);
    let table_counts_sha256 = digest_lines(
        raw.table_row_counts
            .iter()
            .map(|(t, n)| format!("{t}\t{n}"))
            .collect(),
    );
    let schema_sha256 =
        sha256_hex(format!("{:?}|{:?}", raw.migration_table, raw.versions).as_bytes());
    let state_sha256 = sha256_hex(
        format!(
            "{}|{}|{}|{}",
            schema_sha256,
            facts_sha256,
            episodes_sha256.as_deref().unwrap_or("-"),
            table_counts_sha256
        )
        .as_bytes(),
    );
    let digest = sha256_hex(
        format!(
            "{}|{}|{}",
            raw.backend,
            raw.incarnation.as_deref().unwrap_or("-"),
            state_sha256
        )
        .as_bytes(),
    );
    let tenant_lines: Vec<String> = tenants.iter().map(|(o, u)| format!("{o}\t{u}")).collect();

    // Completeness: every facts row was read (the row count matches the enumeration).
    let counted = raw.table_row_counts.get("facts").copied();
    let complete = supported && counted == Some(raw.facts.len() as i64);
    if supported && !complete {
        reasons.push("inventory: enumerated facts rows differ from the table count".to_string());
    }
    if !raw.read_only_verified {
        reasons.push("read-only access could not be verified".to_string());
    }
    if ids.unclassifiable() > 0 {
        reasons.push(format!(
            "ids: {} fact id(s) are invalid, non-canonical, non-RFC, nil or max",
            ids.unclassifiable()
        ));
    }
    if assessment == Assessment::FirstCPlusRollout && ids.v8 > 0 {
        reasons.push(format!(
            "ids: {} legacy UUIDv8 fact id(s) (live {}, soft-deleted {}, superseded {}): first C+ rollout BLOCKED pending remediation",
            ids.v8, ids_live.v8, ids_deleted.v8, ids_superseded.v8
        ));
    }

    let outcome = if reasons.is_empty() && complete {
        "PASS"
    } else {
        "BLOCKED"
    };
    InventoryReport {
        assessment,
        database: DatabaseIdentity {
            backend: raw.backend,
            incarnation: raw.incarnation.clone(),
            name_sha256: raw
                .database_name
                .as_deref()
                .map(|n| sha256_hex(n.as_bytes())),
            other_sessions: raw.other_sessions,
            read_only_verified: raw.read_only_verified,
        },
        schema: SchemaReport {
            migration_table: raw.migration_table,
            versions: raw.versions.clone(),
            candidate_versions_max: candidate_max,
            supported,
            behind_candidate: behind,
            superseded_column,
        },
        facts: FactInventory {
            total: ids.total,
            live,
            soft_deleted: deleted,
            superseded: superseded_column.then_some(superseded),
            ids,
            ids_live,
            ids_soft_deleted: ids_deleted,
            ids_superseded: superseded_column.then_some(ids_superseded),
        },
        tenants: TenantSet {
            count: tenant_lines.len() as u64,
            sha256: digest_lines(tenant_lines),
        },
        table_count: raw.tables.len() as u64,
        fingerprint: Fingerprint {
            facts_sha256,
            episodes_sha256,
            table_counts_sha256,
            schema_sha256,
            state_sha256,
            digest,
        },
        complete,
        outcome,
        reasons,
    }
}

fn read_error(what: &str, error: impl std::fmt::Display) -> MemcoreError {
    // sqlx errors do not echo the connection URL; keep the message bounded.
    MemcoreError::StorageError(format!(
        "rollout inventory: {what}: {}",
        truncate(&error.to_string())
    ))
}

fn truncate(text: &str) -> String {
    text.chars().take(200).collect()
}

fn quoted(name: &str) -> Option<String> {
    (!name.contains('"')).then(|| format!("\"{name}\""))
}

/// Reads a SQLite database file read-only (see the module docs).
#[cfg(feature = "sqlite")]
pub async fn read_sqlite(path: &std::path::Path) -> MemcoreResult<RawInventory> {
    use sqlx::Row;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let mut raw = RawInventory {
        backend: "sqlite",
        ..RawInventory::default()
    };
    if !path.is_file() {
        return Err(MemcoreError::ValidationError(
            "rollout inventory: the SQLite database file does not exist".to_string(),
        ));
    }
    let wal = path.with_file_name(format!(
        "{}-wal",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    ));
    if wal.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        raw.notes.push(
            "sqlite: the -wal file is not empty (live or not checkpointed); inspect a checkpointed snapshot"
                .to_string(),
        );
        return Ok(raw);
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| read_error("open", e))?;
    let result = async {
        let rows = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(&pool)
            .await
            .map_err(|e| read_error("list tables", e))?;
        raw.tables = rows.iter().map(|r| r.get::<String, _>(0)).collect();
        if raw.tables.iter().any(|t| t == "facts") {
            let rows = sqlx::query("SELECT name FROM pragma_table_info('facts')")
                .fetch_all(&pool)
                .await
                .map_err(|e| read_error("facts columns", e))?;
            raw.fact_columns = rows.iter().map(|r| r.get::<String, _>(0)).collect();
        }
        let version_query = if raw.tables.iter().any(|t| t == "schema_migrations") {
            raw.migration_table = Some("schema_migrations");
            Some("SELECT version FROM schema_migrations ORDER BY version")
        } else if raw.tables.iter().any(|t| t == "_sqlx_migrations") {
            raw.migration_table = Some("_sqlx_migrations");
            Some("SELECT version FROM _sqlx_migrations WHERE success = 1 ORDER BY version")
        } else {
            None
        };
        if let Some(query) = version_query {
            let rows = sqlx::query(query)
                .fetch_all(&pool)
                .await
                .map_err(|e| read_error("schema versions", e))?;
            raw.versions = rows.iter().map(|r| r.get::<i64, _>(0)).collect();
        }
        for table in raw.tables.clone() {
            let Some(name) = quoted(&table) else { continue };
            let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {name}"))
                .fetch_one(&pool)
                .await
                .map_err(|e| read_error("row count", e))?;
            raw.table_row_counts.insert(table, n);
        }
        if REQUIRED_FACT_COLUMNS
            .iter()
            .all(|c| raw.fact_columns.iter().any(|have| have == c))
        {
            let superseded = if raw.fact_columns.iter().any(|c| c == "superseded_at") {
                "superseded_at IS NOT NULL"
            } else {
                "0"
            };
            let rows = sqlx::query(&format!(
                "SELECT CASE WHEN typeof(id) = 'text' THEN id END, CAST(org_id AS TEXT), \
                 CAST(user_id AS TEXT), CAST(updated_at AS TEXT), deleted_at IS NOT NULL, \
                 {superseded}, CAST(content AS TEXT) FROM facts"
            ))
            .fetch_all(&pool)
            .await
            .map_err(|e| read_error("facts", e))?;
            raw.facts = rows
                .iter()
                .map(|r| FactRow {
                    id: r.get::<Option<String>, _>(0),
                    org_id: r.get::<Option<String>, _>(1).unwrap_or_default(),
                    user_id: r.get::<Option<String>, _>(2).unwrap_or_default(),
                    updated_at: r.get::<Option<String>, _>(3),
                    deleted: r.get::<i64, _>(4) != 0,
                    superseded: r.get::<i64, _>(5) != 0,
                    content: r.get::<Option<String>, _>(6),
                })
                .collect();
        }
        if raw.tables.iter().any(|t| t == "episodes") {
            let rows = sqlx::query(
                "SELECT CAST(id AS TEXT), CAST(org_id AS TEXT), CAST(user_id AS TEXT), \
                 CAST(embedded AS TEXT), CAST(content AS TEXT) FROM episodes",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| read_error("episodes", e))?;
            raw.episodes = Some(
                rows.iter()
                    .map(|r| EpisodeRow {
                        id: r.get::<Option<String>, _>(0).unwrap_or_default(),
                        org_id: r.get::<Option<String>, _>(1).unwrap_or_default(),
                        user_id: r.get::<Option<String>, _>(2).unwrap_or_default(),
                        embedded: r.get::<Option<String>, _>(3).unwrap_or_default(),
                        content: r.get::<Option<String>, _>(4),
                    })
                    .collect(),
            );
        }
        // `immutable` + `read_only` were requested; confirm the connection refuses writes.
        raw.read_only_verified = sqlx::query("CREATE TABLE ar023_rollout_probe (x INTEGER)")
            .execute(&pool)
            .await
            .is_err();
        Ok::<(), MemcoreError>(())
    }
    .await;
    pool.close().await;
    result.map(|()| raw)
}

/// Reads a PostgreSQL database in one read-only transaction (see the module docs).
#[cfg(feature = "postgres")]
pub async fn read_postgres(url: &str) -> MemcoreResult<RawInventory> {
    use sqlx::postgres::{PgConnectOptions, PgConnection};
    use sqlx::{Connection, Row};
    use std::str::FromStr;

    let options = PgConnectOptions::from_str(url)
        .map_err(|e| read_error("postgres url", e))?
        .options([("default_transaction_read_only", "on")]);
    let mut conn = PgConnection::connect_with(&options)
        .await
        .map_err(|e| read_error("connect", e))?;
    let mut raw = RawInventory {
        backend: "postgres",
        ..RawInventory::default()
    };
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut conn)
        .await
        .map_err(|e| read_error("begin read-only", e))?;
    let result = async {
        let ro: String = sqlx::query_scalar("SHOW transaction_read_only")
            .fetch_one(&mut conn)
            .await
            .map_err(|e| read_error("read-only check", e))?;
        raw.read_only_verified = ro == "on";
        raw.database_name = Some(
            sqlx::query_scalar("SELECT current_database()::text")
                .fetch_one(&mut conn)
                .await
                .map_err(|e| read_error("database", e))?,
        );
        let oid: String = sqlx::query_scalar(
            "SELECT oid::text FROM pg_database WHERE datname = current_database()",
        )
        .fetch_one(&mut conn)
        .await
        .map_err(|e| read_error("database oid", e))?;
        sqlx::query("SAVEPOINT ar023_sysid")
            .execute(&mut conn)
            .await
            .map_err(|e| read_error("savepoint", e))?;
        match sqlx::query_scalar::<_, String>("SELECT system_identifier::text FROM pg_control_system()")
            .fetch_one(&mut conn)
            .await
        {
            Ok(sysid) => raw.incarnation = Some(format!("{sysid}:{oid}")),
            Err(_) => {
                sqlx::query("ROLLBACK TO SAVEPOINT ar023_sysid")
                    .execute(&mut conn)
                    .await
                    .map_err(|e| read_error("savepoint rollback", e))?;
                raw.notes.push(
                    "postgres: pg_control_system() not readable; database incarnation unverifiable"
                        .to_string(),
                );
            }
        }
        raw.other_sessions = Some(
            sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
                 AND pid <> pg_backend_pid()",
            )
            .fetch_one(&mut conn)
            .await
            .map_err(|e| read_error("sessions", e))?,
        );
        let rows = sqlx::query(
            "SELECT table_name::text FROM information_schema.tables WHERE table_schema = current_schema() \
             AND table_type = 'BASE TABLE' ORDER BY 1",
        )
        .fetch_all(&mut conn)
        .await
        .map_err(|e| read_error("list tables", e))?;
        raw.tables = rows.iter().map(|r| r.get::<String, _>(0)).collect();
        let rows = sqlx::query(
            "SELECT column_name::text FROM information_schema.columns WHERE table_schema = current_schema() \
             AND table_name = 'facts' ORDER BY ordinal_position",
        )
        .fetch_all(&mut conn)
        .await
        .map_err(|e| read_error("facts columns", e))?;
        raw.fact_columns = rows.iter().map(|r| r.get::<String, _>(0)).collect();
        let version_query = if raw.tables.iter().any(|t| t == "schema_migrations") {
            raw.migration_table = Some("schema_migrations");
            Some("SELECT version::bigint FROM schema_migrations ORDER BY version")
        } else if raw.tables.iter().any(|t| t == "_sqlx_migrations") {
            raw.migration_table = Some("_sqlx_migrations");
            Some("SELECT version::bigint FROM _sqlx_migrations WHERE success ORDER BY version")
        } else {
            None
        };
        if let Some(query) = version_query {
            let rows = sqlx::query(query)
                .fetch_all(&mut conn)
                .await
                .map_err(|e| read_error("schema versions", e))?;
            raw.versions = rows.iter().map(|r| r.get::<i64, _>(0)).collect();
        }
        for table in raw.tables.clone() {
            let Some(name) = quoted(&table) else { continue };
            let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {name}"))
                .fetch_one(&mut conn)
                .await
                .map_err(|e| read_error("row count", e))?;
            raw.table_row_counts.insert(table, n);
        }
        if REQUIRED_FACT_COLUMNS
            .iter()
            .all(|c| raw.fact_columns.iter().any(|have| have == c))
        {
            let superseded = if raw.fact_columns.iter().any(|c| c == "superseded_at") {
                "superseded_at IS NOT NULL"
            } else {
                "false"
            };
            let rows = sqlx::query(&format!(
                "SELECT id::text, org_id::text, user_id::text, updated_at::text, \
                 deleted_at IS NOT NULL, {superseded}, content::text FROM facts"
            ))
            .fetch_all(&mut conn)
            .await
            .map_err(|e| read_error("facts", e))?;
            raw.facts = rows
                .iter()
                .map(|r| FactRow {
                    id: r.get::<Option<String>, _>(0),
                    org_id: r.get::<Option<String>, _>(1).unwrap_or_default(),
                    user_id: r.get::<Option<String>, _>(2).unwrap_or_default(),
                    updated_at: r.get::<Option<String>, _>(3),
                    deleted: r.get::<bool, _>(4),
                    superseded: r.get::<bool, _>(5),
                    content: r.get::<Option<String>, _>(6),
                })
                .collect();
        }
        if raw.tables.iter().any(|t| t == "episodes") {
            let rows = sqlx::query(
                "SELECT id::text, org_id::text, user_id::text, embedded::text, content::text FROM episodes",
            )
            .fetch_all(&mut conn)
            .await
            .map_err(|e| read_error("episodes", e))?;
            raw.episodes = Some(
                rows.iter()
                    .map(|r| EpisodeRow {
                        id: r.get::<Option<String>, _>(0).unwrap_or_default(),
                        org_id: r.get::<Option<String>, _>(1).unwrap_or_default(),
                        user_id: r.get::<Option<String>, _>(2).unwrap_or_default(),
                        embedded: r.get::<Option<String>, _>(3).unwrap_or_default(),
                        content: r.get::<Option<String>, _>(4),
                    })
                    .collect(),
            );
        }
        Ok::<(), MemcoreError>(())
    }
    .await;
    let _ = sqlx::query("ROLLBACK").execute(&mut conn).await;
    let _ = conn.close().await;
    result.map(|()| raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: Option<&str>, deleted: bool, superseded: bool) -> FactRow {
        FactRow {
            id: id.map(str::to_string),
            org_id: "org".to_string(),
            user_id: "user".to_string(),
            updated_at: Some("2026-10-01T00:00:00Z".to_string()),
            deleted,
            superseded,
            content: Some("text".to_string()),
        }
    }

    fn raw(facts: Vec<FactRow>, versions: Vec<i64>) -> RawInventory {
        let mut counts = BTreeMap::new();
        counts.insert("facts".to_string(), facts.len() as i64);
        RawInventory {
            backend: "sqlite",
            tables: vec!["facts".to_string(), "schema_migrations".to_string()],
            fact_columns: REQUIRED_FACT_COLUMNS
                .iter()
                .map(|c| c.to_string())
                .chain(std::iter::once("superseded_at".to_string()))
                .collect(),
            migration_table: Some("schema_migrations"),
            versions,
            table_row_counts: counts,
            facts,
            read_only_verified: true,
            ..RawInventory::default()
        }
    }

    const V4: &str = "3f2b8c1e-9d4a-4b7c-8e21-0a1b2c3d4e5f";
    const V8: &str = "3f2b8c1e-9d4a-8b7c-8e21-0a1b2c3d4e5f";
    const ALL: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

    #[test]
    fn classification_uses_the_canonical_text_only() {
        assert_eq!(classify_fact_id(Some(V4)), IdClass::V4);
        assert_eq!(classify_fact_id(Some(V8)), IdClass::V8);
        assert_eq!(
            classify_fact_id(Some("3f2b8c1e-9d4a-7b7c-8e21-0a1b2c3d4e5f")),
            IdClass::OtherRfc
        );
        // version 8 nibble with a non-RFC variant is not v8
        assert_eq!(
            classify_fact_id(Some("3f2b8c1e-9d4a-8b7c-ce21-0a1b2c3d4e5f")),
            IdClass::NonRfc
        );
        assert_eq!(
            classify_fact_id(Some(&V8.to_uppercase())),
            IdClass::NonCanonical
        );
        assert_eq!(
            classify_fact_id(Some("3f2b8c1e9d4a8b7c8e210a1b2c3d4e5f")),
            IdClass::NonCanonical
        );
        // an "8" substring at the version position of a non-UUID is not v8
        assert_eq!(
            classify_fact_id(Some("xxxxxxxx-xxxx-8xxx-8xxx-xxxxxxxxxxxx")),
            IdClass::Invalid
        );
        assert_eq!(classify_fact_id(Some("fact-1")), IdClass::Invalid);
        assert_eq!(classify_fact_id(None), IdClass::Invalid);
        assert_eq!(
            classify_fact_id(Some("00000000-0000-0000-0000-000000000000")),
            IdClass::Nil
        );
    }

    #[test]
    fn clean_v4_store_passes_and_legacy_v8_in_any_state_blocks_a_first_rollout() {
        let clean = assess(
            raw(vec![row(Some(V4), false, false)], ALL.to_vec()),
            ALL,
            Assessment::FirstCPlusRollout,
        );
        assert_eq!(clean.outcome, "PASS", "{:?}", clean.reasons);
        assert!(clean.complete && !clean.schema.behind_candidate);
        for (deleted, superseded) in [(false, false), (true, false), (false, true)] {
            let report = assess(
                raw(
                    vec![
                        row(Some(V4), false, false),
                        row(Some(V8), deleted, superseded),
                    ],
                    ALL.to_vec(),
                ),
                ALL,
                Assessment::FirstCPlusRollout,
            );
            assert_eq!(report.outcome, "BLOCKED");
            assert_eq!(report.facts.ids.v8, 1);
            assert!(report.reasons.iter().any(|r| r.contains("legacy UUIDv8")));
            // a later upgrade accepts the deployment's own C+ v8 ids
            let later = assess(
                raw(
                    vec![
                        row(Some(V4), false, false),
                        row(Some(V8), deleted, superseded),
                    ],
                    ALL.to_vec(),
                ),
                ALL,
                Assessment::LaterCPlusUpgrade,
            );
            assert_eq!(later.outcome, "PASS", "{:?}", later.reasons);
        }
    }

    #[test]
    fn invalid_or_noncanonical_ids_and_bad_schemas_block() {
        for id in [
            Some("fact-1"),
            Some("3F2B8C1E-9D4A-4B7C-8E21-0A1B2C3D4E5F"),
            None,
        ] {
            let r = assess(
                raw(vec![row(id, false, false)], ALL.to_vec()),
                ALL,
                Assessment::LaterCPlusUpgrade,
            );
            assert_eq!(r.outcome, "BLOCKED", "{id:?}");
        }
        // unknown version, gap, no migration table, missing column
        for versions in [vec![1, 2, 15], vec![1, 3], vec![]] {
            let r = assess(
                raw(vec![row(Some(V4), false, false)], versions.clone()),
                ALL,
                Assessment::FirstCPlusRollout,
            );
            assert_eq!(r.outcome, "BLOCKED", "{versions:?}");
            assert!(!r.schema.supported);
        }
        let mut no_table = raw(vec![], ALL.to_vec());
        no_table.migration_table = None;
        assert!(
            !assess(no_table, ALL, Assessment::FirstCPlusRollout)
                .schema
                .supported
        );
        let mut missing = raw(vec![row(Some(V4), false, false)], ALL.to_vec());
        missing.fact_columns.retain(|c| c != "deleted_at");
        assert_eq!(
            assess(missing, ALL, Assessment::FirstCPlusRollout).outcome,
            "BLOCKED"
        );
    }

    #[test]
    fn behind_schema_is_supported_but_flagged_and_incomplete_enumeration_blocks() {
        let mut old = raw(vec![row(Some(V4), true, false)], (1..=9).collect());
        old.fact_columns.retain(|c| c != "superseded_at");
        let r = assess(old, ALL, Assessment::FirstCPlusRollout);
        assert_eq!(r.outcome, "PASS", "{:?}", r.reasons);
        assert!(r.schema.behind_candidate && r.facts.superseded.is_none());
        assert_eq!(r.facts.soft_deleted, 1);
        let mut short = raw(vec![row(Some(V4), false, false)], ALL.to_vec());
        short.table_row_counts.insert("facts".to_string(), 2);
        let r = assess(short, ALL, Assessment::FirstCPlusRollout);
        assert_eq!(r.outcome, "BLOCKED");
        assert!(!r.complete);
        let mut unverified = raw(vec![], ALL.to_vec());
        unverified.read_only_verified = false;
        assert_eq!(
            assess(unverified, ALL, Assessment::FirstCPlusRollout).outcome,
            "BLOCKED"
        );
    }

    #[test]
    fn fingerprint_detects_every_fact_change_and_the_report_has_no_ids_or_text() {
        let base = || raw(vec![row(Some(V4), false, false)], ALL.to_vec());
        let a = assess(base(), ALL, Assessment::FirstCPlusRollout)
            .fingerprint
            .digest;
        assert_eq!(
            a,
            assess(base(), ALL, Assessment::FirstCPlusRollout)
                .fingerprint
                .digest
        );
        let mut changed_text = base();
        changed_text.facts[0].content = Some("other".to_string());
        let mut deleted = base();
        deleted.facts[0].deleted = true;
        let mut other_table = base();
        other_table
            .table_row_counts
            .insert("memory_events".to_string(), 1);
        let mut other_db = base();
        other_db.incarnation = Some("7".to_string());
        let base_state = assess(base(), ALL, Assessment::FirstCPlusRollout)
            .fingerprint
            .state_sha256;
        assert_eq!(
            assess(other_db.clone(), ALL, Assessment::FirstCPlusRollout)
                .fingerprint
                .state_sha256,
            base_state,
            "a faithful copy in another database has the same state"
        );
        for changed in [changed_text, deleted, other_table, other_db] {
            assert_ne!(
                assess(changed, ALL, Assessment::FirstCPlusRollout)
                    .fingerprint
                    .digest,
                a
            );
        }
        let json =
            serde_json::to_string(&assess(base(), ALL, Assessment::FirstCPlusRollout)).unwrap();
        assert!(!json.contains(V4) && !json.contains("\"text\"") && !json.contains("\"org\""));
    }
}
