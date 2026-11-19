#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteMigrationRunner;
pub use types::{
    AppliedMigration, Migration, MigrationIssue, MigrationRunner, MigrationStatus,
    MigrationValidationReport, migration_checksum, sorted_migrations, validate_applied_migrations,
};

#[cfg(feature = "postgres")]
pub use postgres::{DEFAULT_MIGRATION_LOCK_WAIT, MIGRATION_LOCK_KEY, PostgresMigrationRunner};

#[cfg(feature = "sqlite")]
pub fn sqlite_migrations() -> Vec<Migration> {
    vec![
        Migration::new(
            1,
            "create_facts",
            include_str!("../../migrations/sqlite/0001_create_facts.sql"),
        ),
        Migration::new(
            2,
            "create_memory_events",
            include_str!("../../migrations/sqlite/0002_create_memory_events.sql"),
        ),
        Migration::new(
            3,
            "create_api_keys",
            include_str!("../../migrations/sqlite/0003_create_api_keys.sql"),
        ),
        Migration::new(
            4,
            "create_provider_usage_events",
            include_str!("../../migrations/sqlite/0004_create_provider_usage_events.sql"),
        ),
        Migration::new(
            5,
            "create_org_plan_configs",
            include_str!("../../migrations/sqlite/0005_create_org_plan_configs.sql"),
        ),
        Migration::new(
            6,
            "create_memory_usage_snapshots",
            include_str!("../../migrations/sqlite/0006_create_memory_usage_snapshots.sql"),
        ),
        Migration::new(
            7,
            "create_background_job_runs",
            include_str!("../../migrations/sqlite/0007_create_background_job_runs.sql"),
        ),
        Migration::new(
            8,
            "create_background_job_locks",
            include_str!("../../migrations/sqlite/0008_create_background_job_locks.sql"),
        ),
        Migration::new(
            9,
            "add_background_job_retry_fields",
            include_str!("../../migrations/sqlite/0009_add_background_job_retry_fields.sql"),
        ),
        Migration::new(
            10,
            "fact_temporal_and_source",
            include_str!("../../migrations/sqlite/0010_fact_temporal_and_source.sql"),
        ),
        Migration::new(
            11,
            "fact_fulltext",
            include_str!("../../migrations/sqlite/0011_fact_fulltext.sql"),
        ),
        Migration::new(
            12,
            "episodes",
            include_str!("../../migrations/sqlite/0012_episodes.sql"),
        ),
        Migration::new(
            13,
            "fact_supersession",
            include_str!("../../migrations/sqlite/0013_fact_supersession.sql"),
        ),
        Migration::new(
            14,
            "entities",
            include_str!("../../migrations/sqlite/0014_entities.sql"),
        ),
        Migration::new(
            15,
            "fact_dedup_key",
            include_str!("../../migrations/sqlite/0015_fact_dedup_key.sql"),
        ),
        Migration::new(
            16,
            "ingest_journal",
            include_str!("../../migrations/sqlite/0016_ingest_journal.sql"),
        ),
        Migration::new(
            17,
            "ingest_receipts",
            include_str!("../../migrations/sqlite/0017_ingest_receipts.sql"),
        ),
    ]
}

#[cfg(feature = "postgres")]
pub fn postgres_migrations() -> Vec<Migration> {
    vec![
        Migration::new(
            1,
            "create_facts",
            include_str!("../../migrations/postgres/0001_create_facts.sql"),
        ),
        Migration::new(
            2,
            "create_memory_events",
            include_str!("../../migrations/postgres/0002_create_memory_events.sql"),
        ),
        Migration::new(
            3,
            "create_api_keys",
            include_str!("../../migrations/postgres/0003_create_api_keys.sql"),
        ),
        Migration::new(
            4,
            "create_provider_usage_events",
            include_str!("../../migrations/postgres/0004_create_provider_usage_events.sql"),
        ),
        Migration::new(
            5,
            "create_org_plan_configs",
            include_str!("../../migrations/postgres/0005_create_org_plan_configs.sql"),
        ),
        Migration::new(
            6,
            "create_memory_usage_snapshots",
            include_str!("../../migrations/postgres/0006_create_memory_usage_snapshots.sql"),
        ),
        Migration::new(
            7,
            "create_background_job_runs",
            include_str!("../../migrations/postgres/0007_create_background_job_runs.sql"),
        ),
        Migration::new(
            8,
            "create_background_job_locks",
            include_str!("../../migrations/postgres/0008_create_background_job_locks.sql"),
        ),
        Migration::new(
            9,
            "add_background_job_retry_fields",
            include_str!("../../migrations/postgres/0009_add_background_job_retry_fields.sql"),
        ),
        Migration::new(
            10,
            "fact_temporal_and_source",
            include_str!("../../migrations/postgres/0010_fact_temporal_and_source.sql"),
        ),
        Migration::new(
            11,
            "fact_fulltext",
            include_str!("../../migrations/postgres/0011_fact_fulltext.sql"),
        ),
        Migration::new(
            12,
            "episodes",
            include_str!("../../migrations/postgres/0012_episodes.sql"),
        ),
        Migration::new(
            13,
            "fact_supersession",
            include_str!("../../migrations/postgres/0013_fact_supersession.sql"),
        ),
        Migration::new(
            14,
            "entities",
            include_str!("../../migrations/postgres/0014_entities.sql"),
        ),
        Migration::new(
            15,
            "fact_dedup_key",
            include_str!("../../migrations/postgres/0015_fact_dedup_key.sql"),
        ),
        Migration::new(
            16,
            "ingest_journal",
            include_str!("../../migrations/postgres/0016_ingest_journal.sql"),
        ),
        Migration::new(
            17,
            "ingest_receipts",
            include_str!("../../migrations/postgres/0017_ingest_receipts.sql"),
        ),
    ]
}
