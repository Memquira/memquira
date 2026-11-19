pub mod backup;
pub mod context_cache;
pub mod dependency_probe;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod ingest_journal;
pub mod jobs;
pub mod keyword_search;
pub mod lazy_vector_store;
pub mod memory_usage;
pub mod migrations;
pub mod mocks;
pub mod org_plan;
pub mod pagination;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod provider_usage;
#[cfg(feature = "qdrant")]
pub mod qdrant;
pub mod queries;
pub mod rollout_inventory;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod startup_checks;
pub mod traits;
pub mod vector;

pub use backup::DatabaseBackupProvider;
#[cfg(feature = "postgres")]
pub use backup::PostgresBackupProvider;
#[cfg(feature = "sqlite")]
pub use backup::{SqliteBackupProvider, sqlite_path_from_database_url};
#[cfg(feature = "redis-cache")]
pub use context_cache::RedisContextCache;
pub use context_cache::{
    redis_context_cache_key, redis_context_index_key, sanitize_redis_url_for_display,
};
#[cfg(feature = "postgres")]
pub use dependency_probe::probe_postgres;
#[cfg(feature = "qdrant")]
pub use dependency_probe::probe_qdrant;
pub use dependency_probe::{DatabaseReadinessProbe, ProbeOutcome};
pub use jobs::{MockBackgroundJobLockStore, MockBackgroundJobRunStore};
#[cfg(feature = "postgres")]
pub use jobs::{PostgresBackgroundJobLockStore, PostgresBackgroundJobRunStore};
#[cfg(feature = "sqlite")]
pub use jobs::{SqliteBackgroundJobLockStore, SqliteBackgroundJobRunStore};
pub use lazy_vector_store::LazyVectorStore;
pub use memory_usage::MockMemoryUsageSnapshotStore;
#[cfg(feature = "postgres")]
pub use memory_usage::PostgresMemoryUsageSnapshotStore;
#[cfg(feature = "sqlite")]
pub use memory_usage::SqliteMemoryUsageSnapshotStore;
pub use migrations::{
    AppliedMigration, Migration, MigrationIssue, MigrationRunner, MigrationStatus,
    MigrationValidationReport, migration_checksum,
};
#[cfg(feature = "postgres")]
pub use migrations::{PostgresMigrationRunner, postgres_migrations};
#[cfg(feature = "sqlite")]
pub use migrations::{SqliteMigrationRunner, sqlite_migrations};
pub use mocks::{
    MockApiKeyStore, MockEntityStore, MockEpisodeStore, MockFactStore, MockIngestJournalStore,
    MockMemoryEventStore, MockVectorStore,
};
pub use org_plan::MockOrgPlanStore;
#[cfg(feature = "postgres")]
pub use org_plan::PostgresOrgPlanStore;
#[cfg(feature = "sqlite")]
pub use org_plan::SqliteOrgPlanStore;
#[cfg(feature = "postgres")]
pub use postgres::{
    PostgresApiKeyStore, PostgresEntityStore, PostgresEpisodeStore, PostgresFactStore,
    PostgresIngestJournalStore, PostgresMemoryEventStore,
};
pub use provider_usage::MockProviderUsageStore;
#[cfg(feature = "postgres")]
pub use provider_usage::PostgresProviderUsageStore;
#[cfg(feature = "sqlite")]
pub use provider_usage::SqliteProviderUsageStore;
#[cfg(feature = "qdrant")]
pub use qdrant::{
    EPISODE_PAYLOAD_INDEXES, FACT_PAYLOAD_INDEXES, PayloadIndexReport, QdrantVectorStore,
    QdrantWriteMode, SearchPath, SearchPolicyStats, identity_collection_name, search_path_for,
};
pub use queries::{FactSearchQuery, MemoryEventQuery};
#[cfg(feature = "sqlite")]
pub use sqlite::{
    SqliteApiKeyStore, SqliteEntityStore, SqliteEpisodeStore, SqliteFactStore,
    SqliteIngestJournalStore, SqliteMemoryEventStore,
};
pub use startup_checks::{StorageMigrationMode, StorageStartupCheckReport};
#[cfg(feature = "postgres")]
pub use startup_checks::{
    check_postgres_pool_startup, check_postgres_startup, connect_postgres_pool,
};
#[cfg(feature = "sqlite")]
pub use startup_checks::{check_sqlite_pool_startup, check_sqlite_startup, connect_sqlite_pool};
pub use traits::{
    ApiKeyStore, BackgroundJobLockStore, BackgroundJobRunStore, EpisodeStore, FactStore,
    MemoryEventStore, MemoryUsageSnapshotStore, OrgPlanStore, ProviderUsageStore, VectorStore,
};
pub use vector::{VectorRecord, VectorSearchQuery, VectorSearchResult};
