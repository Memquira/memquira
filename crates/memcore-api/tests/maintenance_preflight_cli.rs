//! The real `memcore-maintenance rollout-preflight` binary
//! (cases RG4, RG9, RG16,
//! RG19 and RG21) end to end on SQLite files written through the project's own store, and (with
//! `MEMCORE_TEST_POSTGRES_URL`, `postgres` feature) on PostgreSQL. Invented data only.

use chrono::{TimeZone, Utc};
use memcore_core::ports::FactStore;
use memcore_core::{Fact, MemorySource, MemoryType, TenantContext};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memcore-maintenance");
const CONTENT: &str = "invented rollout-gate fixture text";

fn v4(n: u128) -> Uuid {
    Uuid::from_u128(0x7a00_0000_0000_4000_8000_0000_0000_0000 | n)
}
fn v8(n: u128) -> Uuid {
    Uuid::from_u128(0x7a00_0000_0000_8000_8000_0000_0000_0000 | n)
}

fn fact(id: Uuid) -> Fact {
    let at = Utc.with_ymd_and_hms(2026, 10, 1, 10, 0, 0).unwrap();
    let tenant = TenantContext::new("org_rg", "user_rg").unwrap();
    Fact::new(
        id,
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        MemoryType::Preference,
        CONTENT,
        None,
        MemorySource::ApiImport,
        0.9,
        0.6,
        None,
        None,
        at,
        at,
        json!({}),
    )
    .unwrap()
}

/// The fixture's pool: the product's SQLite options (WAL, synchronous, busy timeout) on a single
/// connection. With several pooled connections, `close` can return while one SQLite handle is
/// still closing on its worker thread; that late close then checkpoints or removes `-wal` /
/// `-shm` during the preflight run and breaks both the WAL guard and the read-only check.
async fn fixture_pool(url: &str) -> sqlx::SqlitePool {
    let options = memcore_storage::startup_checks::sqlite_connect_options(url).unwrap();
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

/// Checkpoints the WAL into the main file, verifies the checkpoint completed, then closes the
/// pool, so the store on disk is a quiescent snapshot instead of relying on SQLite's
/// checkpoint-on-close.
async fn checkpoint_and_close(pool: sqlx::SqlitePool, path: &Path) {
    let (busy, log, checkpointed): (i64, i64, i64) =
        sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&pool)
            .await
            .unwrap();
    // TRUNCATE reports (0, 0, 0) only when every frame was copied and the WAL truncated.
    assert_eq!(
        (busy, log, checkpointed),
        (0, 0, 0),
        "the WAL checkpoint did not complete"
    );
    pool.close().await;
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    assert!(
        wal.metadata().map(|m| m.len() == 0).unwrap_or(true),
        "the -wal file is not empty after a completed checkpoint"
    );
}

/// A SQLite store migrated and written by the project's own code; checkpointed and closed.
async fn store(dir: &Path, ids: &[Uuid], delete: Option<Uuid>) -> PathBuf {
    let path = dir.join("memcore.db");
    let url = format!(
        "sqlite://{}?mode=rwc",
        path.display().to_string().replace('\\', "/")
    );
    let pool = fixture_pool(&url).await;
    memcore_storage::check_sqlite_pool_startup(
        &pool,
        memcore_storage::StorageMigrationMode::Auto,
        true,
    )
    .await
    .unwrap();
    let facts = memcore_storage::SqliteFactStore::new(pool.clone());
    let tenant = TenantContext::new("org_rg", "user_rg").unwrap();
    for id in ids {
        facts.insert_fact(&tenant, fact(*id)).await.unwrap();
    }
    if let Some(id) = delete {
        facts.soft_delete_fact(&tenant, id).await.unwrap();
    }
    checkpoint_and_close(pool, &path).await;
    path
}

fn os_env() -> Vec<(String, String)> {
    [
        "SystemRoot",
        "SYSTEMROOT",
        "windir",
        "TEMP",
        "TMP",
        "PATH",
        "USERPROFILE",
        "HOME",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect()
}

fn run(vars: &[(&str, String)], args: &[&str]) -> (i32, Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .envs(os_env())
        .current_dir(dir.path())
        .arg("rollout-preflight")
        .args(args);
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run memcore-maintenance");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let text = format!("{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    (
        out.status.code().unwrap_or(-1),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        text,
    )
}

fn sqlite_env(path: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("MEMCORE_FACT_BACKEND", "sqlite".to_string()),
        ("MEMCORE_EVENT_BACKEND", "sqlite".to_string()),
        (
            "MEMCORE_DATABASE_URL",
            format!("sqlite://{}", path.display().to_string().replace('\\', "/")),
        ),
        ("MEMCORE_VECTOR_BACKEND", "mock".to_string()),
        ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled".to_string()),
    ]
}

fn bytes(path: &Path) -> Vec<Option<Vec<u8>>> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|s| std::fs::read(format!("{}{s}", path.display())).ok())
        .collect()
}

const FIRST: &[&str] = &["--assessment", "first-c-plus-rollout"];
const LATER: &[&str] = &["--assessment", "later-c-plus-upgrade"];

#[tokio::test]
async fn rg19_rg21_clean_store_passes_read_only_without_ids_text_or_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(dir.path(), &[v4(1), v4(2)], Some(v4(2))).await;
    let before = bytes(&path);
    let (code, report, text) = run(&sqlite_env(&path), FIRST);
    assert_eq!(code, 0, "{text}");
    assert_eq!(report["schema"], "memcore.ar023.rollout-preflight/v1");
    assert_eq!(report["outcome"], "PASS");
    assert_eq!(report["config"]["verdict"], "PASS");
    assert_eq!(report["config"]["context_cache_backend"], "disabled");
    let inv = &report["inventory"];
    assert_eq!(
        (
            inv["facts"]["total"].as_u64(),
            inv["facts"]["soft_deleted"].as_u64()
        ),
        (Some(2), Some(1))
    );
    assert_eq!(inv["facts"]["ids"]["v4"], 2);
    assert_eq!(inv["database"]["read_only_verified"], true);
    assert_eq!(inv["complete"], true);
    assert!(inv["fingerprint"]["digest"].as_str().unwrap().len() == 64);
    assert_eq!(
        bytes(&path),
        before,
        "RG19: the store's files are unchanged"
    );
    for leak in [
        v4(1).to_string(),
        CONTENT.to_string(),
        "org_rg".to_string(),
        "user_rg".to_string(),
    ] {
        assert!(!text.contains(&leak), "RG21: {leak}");
    }
    // Deterministic: a second run gives the same fingerprint.
    let (_, again, _) = run(&sqlite_env(&path), FIRST);
    assert_eq!(again["inventory"]["fingerprint"], inv["fingerprint"]);
}

/// Rollout preflight across migration 15: a store at schema 14
/// (holding a legacy row written by the plain insert) is inventoried
/// read-only, supported and **behind** the 15-migration candidate, which the gate turns into
/// "verify a migrated restore copy". After the product's own Auto migration it is current.
#[tokio::test]
async fn integration_schema_14_store_is_behind_the_migration_15_candidate() {
    use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
    let all = sqlite_migrations();
    let candidate_max = all.iter().map(|m| m.version).max().unwrap();
    // The candidate carries migration 15 (and the ingest journal, migration 16);
    // the store below stays at 14, so it is behind whatever the candidate's latest version is.
    assert!(
        all.iter().any(|m| m.version == 15) && candidate_max >= 15,
        "the candidate carries AR-086 migration 15"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memcore.db");
    let url = format!(
        "sqlite://{}?mode=rwc",
        path.display().to_string().replace('\\', "/")
    );
    let pool = fixture_pool(&url).await;
    let schema_14: Vec<_> = all.iter().filter(|m| m.version <= 14).cloned().collect();
    SqliteMigrationRunner::new(pool.clone())
        .apply_pending_migrations(&schema_14)
        .await
        .unwrap();
    // A plain insert keeps the pre-migration-15 statement, so the current store can write
    // the previous release's schema.
    let tenant = TenantContext::new("org_rg", "user_rg").unwrap();
    memcore_storage::SqliteFactStore::new(pool.clone())
        .insert_fact(&tenant, fact(v4(1)))
        .await
        .unwrap();
    checkpoint_and_close(pool, &path).await;

    let before = bytes(&path);
    let (code, report, text) = run(&sqlite_env(&path), FIRST);
    assert_eq!(code, 0, "{text}");
    assert_eq!(report["outcome"], "PASS", "{text}");
    let schema = &report["inventory"]["schema"];
    assert_eq!(schema["supported"], true, "{schema}");
    assert_eq!(schema["behind_candidate"], true, "{schema}");
    assert_eq!(schema["candidate_versions_max"], candidate_max, "{schema}");
    assert_eq!(
        schema["versions"].as_array().map(Vec::len),
        Some(14),
        "{schema}"
    );
    assert_eq!(report["inventory"]["facts"]["total"], 1);
    assert_eq!(
        bytes(&path),
        before,
        "read-only on a schema behind the candidate"
    );

    // The product's own startup migration brings it to the candidate's schema.
    let pool = fixture_pool(&url).await;
    memcore_storage::check_sqlite_pool_startup(
        &pool,
        memcore_storage::StorageMigrationMode::Auto,
        true,
    )
    .await
    .unwrap();
    checkpoint_and_close(pool, &path).await;
    let (code, after, text) = run(&sqlite_env(&path), FIRST);
    assert_eq!(
        (code, after["outcome"].as_str()),
        (0, Some("PASS")),
        "{text}"
    );
    let schema = &after["inventory"]["schema"];
    assert_eq!(schema["behind_candidate"], false, "{schema}");
    assert_eq!(
        schema["versions"].as_array().map(Vec::len),
        Some(all.len()),
        "{schema}"
    );
    assert_eq!(
        after["inventory"]["facts"]["total"], 1,
        "the legacy row survives"
    );
}

#[tokio::test]
async fn rg4_rg16_legacy_v8_blocks_a_first_rollout_but_not_a_later_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(dir.path(), &[v4(1), v8(3)], Some(v8(3))).await;
    let (code, report, _) = run(&sqlite_env(&path), FIRST);
    assert_eq!(code, 4);
    assert_eq!(report["outcome"], "BLOCKED");
    assert_eq!(report["inventory"]["facts"]["ids"]["v8"], 1);
    assert_eq!(report["inventory"]["facts"]["ids_soft_deleted"]["v8"], 1);
    let (code, later, _) = run(&sqlite_env(&path), LATER);
    assert_eq!((code, later["outcome"].as_str()), (0, Some("PASS")));
}

#[tokio::test]
async fn rg9_rg21_the_candidates_own_cache_verdict_blocks_unsafe_environments() {
    let dir = tempfile::tempdir().unwrap();
    let path = store(dir.path(), &[v4(1)], None).await;
    for (extra, expect) in [
        (
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "redis".to_string()),
                (
                    "MEMCORE_REDIS_URL",
                    "redis://:rg-s3cret@cache.internal:6379".to_string(),
                ),
            ],
            "redis is not supported",
        ),
        (
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "memory".to_string()),
                ("MEMCORE_ENV", "production".to_string()),
                (
                    "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS",
                    "true".to_string(),
                ),
            ],
            "process-local cache",
        ),
        (
            vec![("MEMCORE_CONTEXT_CACHE_ENABLED", "true".to_string())],
            "contradicts",
        ),
    ] {
        let mut vars = sqlite_env(&path);
        vars.retain(|(k, _)| !extra.iter().any(|(e, _)| e == k));
        vars.extend(extra);
        let (code, report, text) = run(&vars, FIRST);
        assert_eq!(code, 4, "{text}");
        assert_eq!(report["config"]["verdict"], "REFUSED");
        assert!(
            report["config"]["error"].as_str().unwrap().contains(expect),
            "{text}"
        );
        // the inventory itself is still produced
        assert_eq!(report["inventory"]["outcome"], "PASS");
        assert!(
            !text.contains("rg-s3cret") && !text.contains("cache.internal"),
            "RG21"
        );
    }
}

#[test]
fn argument_errors_are_refused() {
    for args in [
        &[][..],
        &["--assessment", "sometimes"][..],
        &["--bogus"][..],
    ] {
        let (code, _, _) = run(&[], args);
        assert_eq!(code, 2, "{args:?}");
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn rg19_rg21_postgres_preflight_is_read_only_and_redacts_the_url() {
    let Ok(url) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
        eprintln!("skipping postgres preflight: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    // The disposable server's own database (test-owned): migrated and written by the project.
    let db_url = url.clone();
    let facts = memcore_storage::PostgresFactStore::connect(&db_url)
        .await
        .unwrap();
    let tenant = TenantContext::new("org_rg", "user_rg").unwrap();
    facts
        .insert_fact(&tenant, fact(Uuid::new_v4()))
        .await
        .unwrap();
    facts.pool().close().await;
    let vars = vec![
        ("MEMCORE_FACT_BACKEND", "postgres".to_string()),
        ("MEMCORE_EVENT_BACKEND", "postgres".to_string()),
        ("MEMCORE_POSTGRES_URL", db_url.clone()),
        ("MEMCORE_VECTOR_BACKEND", "mock".to_string()),
        ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled".to_string()),
    ];
    let (code, report, text) = run(&vars, FIRST);
    let (code2, report2, _) = run(&vars, FIRST);
    assert_eq!(code, 0, "{text}");
    assert_eq!(report["inventory"]["database"]["read_only_verified"], true);
    assert!(report["inventory"]["database"]["incarnation"].is_string());
    assert!(report["inventory"]["facts"]["total"].as_u64().unwrap() >= 1);
    assert_eq!(
        (code2, &report2["inventory"]["fingerprint"]),
        (0, &report["inventory"]["fingerprint"])
    );
    let password = url
        .split_once("://")
        .and_then(|(_, r)| r.split_once('@'))
        .map(|(c, _)| c.to_string())
        .unwrap_or_default();
    assert!(password.is_empty() || !text.contains(&password), "RG21");
}
