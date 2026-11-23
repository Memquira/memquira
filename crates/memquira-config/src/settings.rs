use std::fmt;
use std::str::FromStr;

use memquira_common::env::var as env_var;
use memquira_common::{MemquiraError, MemquiraResult};

const MEMQUIRA_ENV: &str = "MEMQUIRA_ENV";
const MEMQUIRA_HOST: &str = "MEMQUIRA_HOST";
const MEMQUIRA_PORT: &str = "MEMQUIRA_PORT";
const MEMQUIRA_STORAGE_MODE: &str = "MEMQUIRA_STORAGE_MODE";
const MEMQUIRA_VECTOR_BACKEND: &str = "MEMQUIRA_VECTOR_BACKEND";
const MEMQUIRA_FACT_BACKEND: &str = "MEMQUIRA_FACT_BACKEND";
const MEMQUIRA_EVENT_BACKEND: &str = "MEMQUIRA_EVENT_BACKEND";
const MEMQUIRA_DATABASE_URL: &str = "MEMQUIRA_DATABASE_URL";
const MEMQUIRA_POSTGRES_URL: &str = "MEMQUIRA_POSTGRES_URL";
const MEMQUIRA_DATABASE_MIGRATIONS_ENABLED: &str = "MEMQUIRA_DATABASE_MIGRATIONS_ENABLED";
const MEMQUIRA_DATABASE_MIGRATION_MODE: &str = "MEMQUIRA_DATABASE_MIGRATION_MODE";
const MEMQUIRA_DATABASE_REQUIRE_CLEAN_MIGRATIONS: &str =
    "MEMQUIRA_DATABASE_REQUIRE_CLEAN_MIGRATIONS";
const MEMQUIRA_BACKUP_ENABLED: &str = "MEMQUIRA_BACKUP_ENABLED";
const MEMQUIRA_BACKUP_DIR: &str = "MEMQUIRA_BACKUP_DIR";
const MEMQUIRA_BACKUP_MAX_FILES: &str = "MEMQUIRA_BACKUP_MAX_FILES";
const MEMQUIRA_RESTORE_ENABLED: &str = "MEMQUIRA_RESTORE_ENABLED";
const MEMQUIRA_SECURITY_HEADERS_ENABLED: &str = "MEMQUIRA_SECURITY_HEADERS_ENABLED";
const MEMQUIRA_MAX_REQUEST_BODY_BYTES: &str = "MEMQUIRA_MAX_REQUEST_BODY_BYTES";
const MEMQUIRA_CORS_ENABLED: &str = "MEMQUIRA_CORS_ENABLED";
const MEMQUIRA_CORS_ALLOWED_ORIGINS: &str = "MEMQUIRA_CORS_ALLOWED_ORIGINS";
const MEMQUIRA_CORS_ALLOWED_METHODS: &str = "MEMQUIRA_CORS_ALLOWED_METHODS";
const MEMQUIRA_CORS_ALLOWED_HEADERS: &str = "MEMQUIRA_CORS_ALLOWED_HEADERS";
const MEMQUIRA_CORS_EXPOSED_HEADERS: &str = "MEMQUIRA_CORS_EXPOSED_HEADERS";
const MEMQUIRA_CORS_ALLOW_CREDENTIALS: &str = "MEMQUIRA_CORS_ALLOW_CREDENTIALS";
const MEMQUIRA_QDRANT_URL: &str = "MEMQUIRA_QDRANT_URL";
const MEMQUIRA_QDRANT_COLLECTION: &str = "MEMQUIRA_QDRANT_COLLECTION";
const MEMQUIRA_QDRANT_EPISODE_COLLECTION: &str = "MEMQUIRA_QDRANT_EPISODE_COLLECTION";
const MEMQUIRA_QDRANT_ENSURE_INDEXES: &str = "MEMQUIRA_QDRANT_ENSURE_INDEXES";
const MEMQUIRA_QDRANT_WRITE_MODE: &str = "MEMQUIRA_QDRANT_WRITE_MODE";
const MEMQUIRA_QDRANT_SEARCH_HNSW_EF: &str = "MEMQUIRA_QDRANT_SEARCH_HNSW_EF";
/// Upper bound for `MEMQUIRA_QDRANT_SEARCH_HNSW_EF` (a larger beam is a misconfiguration).
const MAX_QDRANT_SEARCH_HNSW_EF: u64 = 4096;
const MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS: &str =
    "MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS";
/// Upper bound for `MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS`: an exact (brute-force) search
/// scans every point of the tenant, so a larger bound is a misconfiguration.
const MAX_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS: u64 = 1_000_000;
const MEMQUIRA_EPISODES_ENABLED: &str = "MEMQUIRA_EPISODES_ENABLED";
const MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED: &str = "MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED";
const MEMQUIRA_QUERY_ANALYSIS_ENABLED: &str = "MEMQUIRA_QUERY_ANALYSIS_ENABLED";
const MEMQUIRA_TEMPORAL_RANKING_ENABLED: &str = "MEMQUIRA_TEMPORAL_RANKING_ENABLED";
const MEMQUIRA_TEMPORAL_BOOST: &str = "MEMQUIRA_TEMPORAL_BOOST";
const MEMQUIRA_MULTI_QUERY: &str = "MEMQUIRA_MULTI_QUERY";
const MEMQUIRA_RERANK_PROVIDER: &str = "MEMQUIRA_RERANK_PROVIDER";
const MEMQUIRA_RERANK_CANDIDATES: &str = "MEMQUIRA_RERANK_CANDIDATES";
const MEMQUIRA_RERANK_TIMEOUT_MS: &str = "MEMQUIRA_RERANK_TIMEOUT_MS";
const MEMQUIRA_RERANK_LOCAL_MODEL_PATH: &str = "MEMQUIRA_RERANK_LOCAL_MODEL_PATH";
const MEMQUIRA_RERANK_LOCAL_MODEL_SHA256: &str = "MEMQUIRA_RERANK_LOCAL_MODEL_SHA256";
const MEMQUIRA_RERANK_LOCAL_TOKENIZER_PATH: &str = "MEMQUIRA_RERANK_LOCAL_TOKENIZER_PATH";
const MEMQUIRA_RERANK_LOCAL_TOKENIZER_SHA256: &str = "MEMQUIRA_RERANK_LOCAL_TOKENIZER_SHA256";
const MEMQUIRA_RERANK_LOCAL_THREADS: &str = "MEMQUIRA_RERANK_LOCAL_THREADS";
const MEMQUIRA_RERANK_LOCAL_BATCH_SIZE: &str = "MEMQUIRA_RERANK_LOCAL_BATCH_SIZE";
const MEMQUIRA_RERANK_BEDROCK_REGION: &str = "MEMQUIRA_RERANK_BEDROCK_REGION";
const MEMQUIRA_RERANK_BEDROCK_MODEL_ARN: &str = "MEMQUIRA_RERANK_BEDROCK_MODEL_ARN";
const MEMQUIRA_RERANK_BEDROCK_PROFILE: &str = "MEMQUIRA_RERANK_BEDROCK_PROFILE";
const MEMQUIRA_QUERY_DECOMPOSITION: &str = "MEMQUIRA_QUERY_DECOMPOSITION";
const MEMQUIRA_QUERY_DECOMPOSITION_TIMEOUT_MS: &str = "MEMQUIRA_QUERY_DECOMPOSITION_TIMEOUT_MS";
const MEMQUIRA_LANCEDB_PATH: &str = "MEMQUIRA_LANCEDB_PATH";
const MEMQUIRA_LANCEDB_TABLE: &str = "MEMQUIRA_LANCEDB_TABLE";
const MEMQUIRA_LLM_PROVIDER: &str = "MEMQUIRA_LLM_PROVIDER";
const MEMQUIRA_LLM_MODEL: &str = "MEMQUIRA_LLM_MODEL";
const MEMQUIRA_EMBEDDING_PROVIDER: &str = "MEMQUIRA_EMBEDDING_PROVIDER";
const MEMQUIRA_EMBEDDING_MODEL: &str = "MEMQUIRA_EMBEDDING_MODEL";
const MEMQUIRA_ENABLE_PII_REDACTION: &str = "MEMQUIRA_ENABLE_PII_REDACTION";
const MEMQUIRA_STRUCTURED_EXTRACTION_ENABLED: &str = "MEMQUIRA_STRUCTURED_EXTRACTION_ENABLED";
const MEMQUIRA_STRUCTURED_EXTRACTION_POLICY: &str = "MEMQUIRA_STRUCTURED_EXTRACTION_POLICY";
const MEMQUIRA_CONSOLIDATION_MODE: &str = "MEMQUIRA_CONSOLIDATION_MODE";
const MEMQUIRA_RELATED_FACT_MATCHER: &str = "MEMQUIRA_RELATED_FACT_MATCHER";
const MEMQUIRA_CLASSIFICATION_PROMPT: &str = "MEMQUIRA_CLASSIFICATION_PROMPT";
const MEMQUIRA_IMPORTANCE_POLICY: &str = "MEMQUIRA_IMPORTANCE_POLICY";
const MEMQUIRA_TEMPORAL_REWRITE: &str = "MEMQUIRA_TEMPORAL_REWRITE";
const MEMQUIRA_UNKNOWN_MEMORY_TYPE: &str = "MEMQUIRA_UNKNOWN_MEMORY_TYPE";
const MEMQUIRA_EVENT_UPDATE_POLICY: &str = "MEMQUIRA_EVENT_UPDATE_POLICY";
const MEMQUIRA_DEDUP_VALUE_GUARD: &str = "MEMQUIRA_DEDUP_VALUE_GUARD";
const MEMQUIRA_RELATED_FACT_SIMILARITY_FLOOR: &str = "MEMQUIRA_RELATED_FACT_SIMILARITY_FLOOR";
const MEMQUIRA_CONTEXT_DEFAULT_PACKING: &str = "MEMQUIRA_CONTEXT_DEFAULT_PACKING";
const MEMQUIRA_CONSOLIDATION_CANDIDATES: &str = "MEMQUIRA_CONSOLIDATION_CANDIDATES";
const MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR: &str = "MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR";
const MEMQUIRA_ENTITIES_ENABLED: &str = "MEMQUIRA_ENTITIES_ENABLED";
const MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD: &str = "MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD";
const MEMQUIRA_ENTITY_LLM_CONFIRMATION_ENABLED: &str = "MEMQUIRA_ENTITY_LLM_CONFIRMATION_ENABLED";
const MEMQUIRA_SEARCH_STRATEGY: &str = "MEMQUIRA_SEARCH_STRATEGY";
const MEMQUIRA_RRF_K: &str = "MEMQUIRA_RRF_K";
const MEMQUIRA_SEARCH_MAX_LIMIT: &str = "MEMQUIRA_SEARCH_MAX_LIMIT";
const MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY: &str = "MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY";
const MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED: &str =
    "MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED";
const MEMQUIRA_INGEST_JOURNAL_ENABLED: &str = "MEMQUIRA_INGEST_JOURNAL_ENABLED";
const MEMQUIRA_INGEST_RECOVERY_STALE_SECS: &str = "MEMQUIRA_INGEST_RECOVERY_STALE_SECS";
const MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS: &str = "MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS";
const MEMQUIRA_INGEST_INFLIGHT_WAIT_MS: &str = "MEMQUIRA_INGEST_INFLIGHT_WAIT_MS";
const MEMQUIRA_INGEST_RECEIPT_TTL_SECS: &str = "MEMQUIRA_INGEST_RECEIPT_TTL_SECS";
const MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR: &str = "MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR";
const MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY: &str = "MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY";
const MEMQUIRA_RRF_WEIGHT_SEMANTIC: &str = "MEMQUIRA_RRF_WEIGHT_SEMANTIC";
const MEMQUIRA_RRF_WEIGHT_KEYWORD: &str = "MEMQUIRA_RRF_WEIGHT_KEYWORD";
const MEMQUIRA_EXTRACTION_WINDOW_TURNS: &str = "MEMQUIRA_EXTRACTION_WINDOW_TURNS";
const MEMQUIRA_EXTRACTION_WINDOW_OVERLAP: &str = "MEMQUIRA_EXTRACTION_WINDOW_OVERLAP";
const MEMQUIRA_EXTRACTION_CONCURRENCY: &str = "MEMQUIRA_EXTRACTION_CONCURRENCY";
const MEMQUIRA_MIN_IMPORTANCE: &str = "MEMQUIRA_MIN_IMPORTANCE";
const MEMQUIRA_AUTH_ENABLED: &str = "MEMQUIRA_AUTH_ENABLED";
const MEMQUIRA_AUTH_MODE: &str = "MEMQUIRA_AUTH_MODE";
const MEMQUIRA_DEV_API_KEY: &str = "MEMQUIRA_DEV_API_KEY";
const MEMQUIRA_API_KEY_PEPPER: &str = "MEMQUIRA_API_KEY_PEPPER";
const MEMQUIRA_RATE_LIMIT_ENABLED: &str = "MEMQUIRA_RATE_LIMIT_ENABLED";
const MEMQUIRA_ADMISSION_CONTROL: &str = "MEMQUIRA_ADMISSION_CONTROL";
const MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS: &str = "MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS";
const MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS: &str = "MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS";
const MEMQUIRA_PROVIDER_CHAT_MAX_CONCURRENCY: &str = "MEMQUIRA_PROVIDER_CHAT_MAX_CONCURRENCY";
const MEMQUIRA_PROVIDER_EMBEDDING_MAX_CONCURRENCY: &str =
    "MEMQUIRA_PROVIDER_EMBEDDING_MAX_CONCURRENCY";
const MEMQUIRA_PROVIDER_PERMIT_TIMEOUT_MS: &str = "MEMQUIRA_PROVIDER_PERMIT_TIMEOUT_MS";
const MEMQUIRA_ADMISSION_READ_MAX_IN_FLIGHT: &str = "MEMQUIRA_ADMISSION_READ_MAX_IN_FLIGHT";
const MEMQUIRA_ADMISSION_READ_MAX_QUEUE: &str = "MEMQUIRA_ADMISSION_READ_MAX_QUEUE";
const MEMQUIRA_ADMISSION_READ_DEADLINE_MS: &str = "MEMQUIRA_ADMISSION_READ_DEADLINE_MS";
const MEMQUIRA_ADMISSION_WRITE_MAX_IN_FLIGHT: &str = "MEMQUIRA_ADMISSION_WRITE_MAX_IN_FLIGHT";
const MEMQUIRA_ADMISSION_WRITE_MAX_QUEUE: &str = "MEMQUIRA_ADMISSION_WRITE_MAX_QUEUE";
const MEMQUIRA_ADMISSION_WRITE_DEADLINE_MS: &str = "MEMQUIRA_ADMISSION_WRITE_DEADLINE_MS";
const MEMQUIRA_ADMISSION_ADMIN_MAX_IN_FLIGHT: &str = "MEMQUIRA_ADMISSION_ADMIN_MAX_IN_FLIGHT";
const MEMQUIRA_ADMISSION_ADMIN_MAX_QUEUE: &str = "MEMQUIRA_ADMISSION_ADMIN_MAX_QUEUE";
const MEMQUIRA_ADMISSION_ADMIN_DEADLINE_MS: &str = "MEMQUIRA_ADMISSION_ADMIN_DEADLINE_MS";
const MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE: &str = "MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE";
const MEMQUIRA_LOG_FORMAT: &str = "MEMQUIRA_LOG_FORMAT";
const MEMQUIRA_LOG_LEVEL: &str = "MEMQUIRA_LOG_LEVEL";
const MEMQUIRA_REQUEST_ID_HEADER: &str = "MEMQUIRA_REQUEST_ID_HEADER";
const MEMQUIRA_METRICS_ENABLED: &str = "MEMQUIRA_METRICS_ENABLED";
const MEMQUIRA_METRICS_PATH: &str = "MEMQUIRA_METRICS_PATH";
const MEMQUIRA_METRICS_REQUIRE_AUTH: &str = "MEMQUIRA_METRICS_REQUIRE_AUTH";
const MEMQUIRA_METRICS_INCLUDE_PROCESS: &str = "MEMQUIRA_METRICS_INCLUDE_PROCESS";
const MEMQUIRA_RETENTION_ENABLED: &str = "MEMQUIRA_RETENTION_ENABLED";
const DEFAULT_METRICS_PATH: &str = "/metrics";
const MEMQUIRA_FACT_RETENTION_DAYS: &str = "MEMQUIRA_FACT_RETENTION_DAYS";
const MEMQUIRA_EVENT_RETENTION_DAYS: &str = "MEMQUIRA_EVENT_RETENTION_DAYS";
const MEMQUIRA_CONTEXT_CACHE_ENABLED: &str = "MEMQUIRA_CONTEXT_CACHE_ENABLED";
const MEMQUIRA_CONTEXT_CACHE_BACKEND: &str = "MEMQUIRA_CONTEXT_CACHE_BACKEND";
const MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS: &str = "MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS";
const MEMQUIRA_CONTEXT_CACHE_MAX_ENTRIES: &str = "MEMQUIRA_CONTEXT_CACHE_MAX_ENTRIES";
const MEMQUIRA_CONTEXT_CACHE_KEY_PREFIX: &str = "MEMQUIRA_CONTEXT_CACHE_KEY_PREFIX";
const MEMQUIRA_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED: &str =
    "MEMQUIRA_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED";
const MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS: &str =
    "MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS";
const MEMQUIRA_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED: &str =
    "MEMQUIRA_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED";
const MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS: &str = "MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS";
const MEMQUIRA_CONTEXT_CACHE_METRICS_ENABLED: &str = "MEMQUIRA_CONTEXT_CACHE_METRICS_ENABLED";
const MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS: &str =
    "MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS";
const MEMQUIRA_PROVIDER_TIMEOUT_SECONDS: &str = "MEMQUIRA_PROVIDER_TIMEOUT_SECONDS";
const MEMQUIRA_PROVIDER_MAX_RETRIES: &str = "MEMQUIRA_PROVIDER_MAX_RETRIES";
const MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS: &str = "MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS";
const MEMQUIRA_PROVIDER_MAX_BACKOFF_MS: &str = "MEMQUIRA_PROVIDER_MAX_BACKOFF_MS";
const MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER: &str = "MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER";
const MEMQUIRA_PROVIDER_RETRY_JITTER_ENABLED: &str = "MEMQUIRA_PROVIDER_RETRY_JITTER_ENABLED";
const MEMQUIRA_PROVIDER_FALLBACK_ENABLED: &str = "MEMQUIRA_PROVIDER_FALLBACK_ENABLED";
const MEMQUIRA_LLM_FALLBACK_ORDER: &str = "MEMQUIRA_LLM_FALLBACK_ORDER";
const MEMQUIRA_EMBEDDING_FALLBACK_ORDER: &str = "MEMQUIRA_EMBEDDING_FALLBACK_ORDER";
const MEMQUIRA_SUMMARIZER_FALLBACK_ORDER: &str = "MEMQUIRA_SUMMARIZER_FALLBACK_ORDER";
const MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_ENABLED: &str = "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_ENABLED";
const MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD: &str =
    "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD";
const MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS: &str =
    "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS";
const MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS: &str =
    "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS";
const MEMQUIRA_PROVIDER_USAGE_METRICS_ENABLED: &str = "MEMQUIRA_PROVIDER_USAGE_METRICS_ENABLED";
const MEMQUIRA_PROVIDER_COST_TRACKING_ENABLED: &str = "MEMQUIRA_PROVIDER_COST_TRACKING_ENABLED";
const MEMQUIRA_PROVIDER_USAGE_PERSISTENCE_ENABLED: &str =
    "MEMQUIRA_PROVIDER_USAGE_PERSISTENCE_ENABLED";
const MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS: &str = "MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS";
const MEMQUIRA_QUOTAS_ENABLED: &str = "MEMQUIRA_QUOTAS_ENABLED";
const MEMQUIRA_MAX_USERS_PER_ORG: &str = "MEMQUIRA_MAX_USERS_PER_ORG";
const MEMQUIRA_MAX_MEMORIES_PER_USER: &str = "MEMQUIRA_MAX_MEMORIES_PER_USER";
const MEMQUIRA_MAX_MEMORIES_PER_ORG: &str = "MEMQUIRA_MAX_MEMORIES_PER_ORG";
const MEMQUIRA_DAILY_PROVIDER_REQUEST_LIMIT: &str = "MEMQUIRA_DAILY_PROVIDER_REQUEST_LIMIT";
const MEMQUIRA_DAILY_PROVIDER_TOKEN_LIMIT: &str = "MEMQUIRA_DAILY_PROVIDER_TOKEN_LIMIT";
const MEMQUIRA_PROVIDER_GUARDRAILS_ENABLED: &str = "MEMQUIRA_PROVIDER_GUARDRAILS_ENABLED";
const MEMQUIRA_REAL_PROVIDER_CALLS_ENABLED: &str = "MEMQUIRA_REAL_PROVIDER_CALLS_ENABLED";
const MEMQUIRA_PROVIDER_TEST_MODE: &str = "MEMQUIRA_PROVIDER_TEST_MODE";
const MEMQUIRA_PROVIDER_MAX_CALLS_PER_RUN: &str = "MEMQUIRA_PROVIDER_MAX_CALLS_PER_RUN";
const MEMQUIRA_PROVIDER_MAX_INPUT_CHARS: &str = "MEMQUIRA_PROVIDER_MAX_INPUT_CHARS";
const MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED: &str =
    "MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED";
const MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS: &str = "MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS";
const MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS: &str = "MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS";
const MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS: &str = "MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS";
const MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS: &str = "MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS";
const MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL: &str = "MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL";
const MEMQUIRA_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS: &str =
    "MEMQUIRA_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS";
const MEMQUIRA_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS: &str =
    "MEMQUIRA_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS";
const MEMQUIRA_MULTI_PROVIDER_VALIDATION_ENABLED: &str =
    "MEMQUIRA_MULTI_PROVIDER_VALIDATION_ENABLED";
const MEMQUIRA_MULTI_PROVIDER_VALIDATION_CONFIRMATION: &str =
    "MEMQUIRA_MULTI_PROVIDER_VALIDATION_CONFIRMATION";
const MEMQUIRA_BACKGROUND_JOBS_ENABLED: &str = "MEMQUIRA_BACKGROUND_JOBS_ENABLED";
const MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS: &str =
    "MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS";
const MEMQUIRA_BACKGROUND_JOB_ORG_IDS: &str = "MEMQUIRA_BACKGROUND_JOB_ORG_IDS";
const MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED: &str =
    "MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED";
const MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS: &str =
    "MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS";
const MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_ENABLED: &str =
    "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_ENABLED";
const MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS: &str =
    "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS";
const MEMQUIRA_MEMORY_RETENTION_JOB_ENABLED: &str = "MEMQUIRA_MEMORY_RETENTION_JOB_ENABLED";
const MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS: &str =
    "MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS";
const MEMQUIRA_BACKGROUND_JOB_HISTORY_ENABLED: &str = "MEMQUIRA_BACKGROUND_JOB_HISTORY_ENABLED";
const MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS: &str =
    "MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS";
const MEMQUIRA_BACKGROUND_JOB_LOCK_ENABLED: &str = "MEMQUIRA_BACKGROUND_JOB_LOCK_ENABLED";
const MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND: &str = "MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND";
const MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS: &str = "MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS";
const MEMQUIRA_BACKGROUND_JOB_LOCK_OWNER_ID: &str = "MEMQUIRA_BACKGROUND_JOB_LOCK_OWNER_ID";
const MEMQUIRA_BACKGROUND_JOB_RETRIES_ENABLED: &str = "MEMQUIRA_BACKGROUND_JOB_RETRIES_ENABLED";
const MEMQUIRA_BACKGROUND_JOB_MAX_RETRIES: &str = "MEMQUIRA_BACKGROUND_JOB_MAX_RETRIES";
const MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS: &str =
    "MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS";
const MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS: &str = "MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS";
const MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER: &str =
    "MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER";
const MEMQUIRA_BACKGROUND_JOB_RETRY_JITTER_ENABLED: &str =
    "MEMQUIRA_BACKGROUND_JOB_RETRY_JITTER_ENABLED";
const MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS: &str =
    "MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS";
const MEMQUIRA_ALLOW_MOCK: &str = "MEMQUIRA_ALLOW_MOCK";
const MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS: &str =
    "MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS";
const MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS: &str =
    "MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS";
const MEMQUIRA_REDIS_URL: &str = "MEMQUIRA_REDIS_URL";
const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
const MEMQUIRA_LLM_BASE_URL: &str = "MEMQUIRA_LLM_BASE_URL";
const MEMQUIRA_LLM_API_KEY: &str = "MEMQUIRA_LLM_API_KEY";
const MEMQUIRA_LLM_ALLOW_NO_API_KEY: &str = "MEMQUIRA_LLM_ALLOW_NO_API_KEY";
const MEMQUIRA_EMBEDDING_BASE_URL: &str = "MEMQUIRA_EMBEDDING_BASE_URL";
const MEMQUIRA_EMBEDDING_API_KEY: &str = "MEMQUIRA_EMBEDDING_API_KEY";
const MEMQUIRA_EMBEDDING_ALLOW_NO_API_KEY: &str = "MEMQUIRA_EMBEDDING_ALLOW_NO_API_KEY";
const MEMQUIRA_EMBEDDING_DIMENSIONS: &str = "MEMQUIRA_EMBEDDING_DIMENSIONS";
const MEMQUIRA_EMBEDDING_SEND_DIMENSIONS: &str = "MEMQUIRA_EMBEDDING_SEND_DIMENSIONS";
const MEMQUIRA_EMBEDDING_NORMALIZE: &str = "MEMQUIRA_EMBEDDING_NORMALIZE";
const MEMQUIRA_EMBEDDING_QUERY_PREFIX: &str = "MEMQUIRA_EMBEDDING_QUERY_PREFIX";
const MEMQUIRA_EMBEDDING_DOCUMENT_PREFIX: &str = "MEMQUIRA_EMBEDDING_DOCUMENT_PREFIX";
const MEMQUIRA_EMBEDDING_MODEL_REVISION: &str = "MEMQUIRA_EMBEDDING_MODEL_REVISION";
const MEMQUIRA_EMBEDDING_SERVING_VARIANT: &str = "MEMQUIRA_EMBEDDING_SERVING_VARIANT";
const MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD: &str = "MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD";
const MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP: &str = "MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP";
const MEMQUIRA_LLM_JSON_MODE: &str = "MEMQUIRA_LLM_JSON_MODE";
const MEMQUIRA_PROVIDER_PROBE: &str = "MEMQUIRA_PROVIDER_PROBE";
const MEMQUIRA_EXPORT_MAX_RECORDS: &str = "MEMQUIRA_EXPORT_MAX_RECORDS";
const MEMQUIRA_REQUIRE_QUALITY_TIER: &str = "MEMQUIRA_REQUIRE_QUALITY_TIER";
const MEMQUIRA_LLM_STARTUP_PROBE: &str = "MEMQUIRA_LLM_STARTUP_PROBE";
const OPENAI_BASE_URL: &str = "OPENAI_BASE_URL";

pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_REQUEST_ID_HEADER: &str = "X-Request-ID";
pub const DEFAULT_CONTEXT_CACHE_KEY_PREFIX: &str = "memcore";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 1_048_576;

/// Default ceiling on any single LLM call's output (`MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS`), and the
/// default fact-extraction limit.
///
/// It was 300, and extraction hardcoded the same 300 regardless of this setting. Measured on 8,700
/// measured extraction responses: a fact costs 121–183 output tokens (p50–p99), a
/// complete response carried up to 6 facts, and responses cut off at 300 tokens had already listed
/// up to 4. At 300, a noticeable share of responses was truncated — each one
/// losing every fact in its batch. 2,048 fits about 11 facts at the p99 cost per fact. Output is
/// billed as generated, so the higher ceiling costs nothing on responses that were not truncated.
pub const DEFAULT_PROVIDER_MAX_OUTPUT_TOKENS: usize = 2_048;

/// Default fact-classification limit (`MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS`). Classification
/// returns one small JSON decision: across 9,222 measured responses the longest was 92 tokens and
/// none was truncated, so the previous 300 is kept.
pub const DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS: usize = 300;
pub const DEFAULT_CORS_ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "OPTIONS"];
pub const DEFAULT_CORS_ALLOWED_HEADERS: &[&str] = &[
    "Authorization",
    "Content-Type",
    "X-Organization-ID",
    "X-Memquira-Test-Source",
    "X-Memcore-Test-Source",
];

/// Exact confirmation required for `multi_real` provider test mode.
pub const MULTI_PROVIDER_VALIDATION_CONFIRMATION: &str =
    "I_UNDERSTAND_THIS_WILL_USE_PROVIDER_CREDITS";

/// Safe upper bound for `MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL`.
pub const MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTestMode {
    MockOnly,
    SingleReal,
    MultiReal,
    Production,
}

impl ProviderTestMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MockOnly => "mock_only",
            Self::SingleReal => "single_real",
            Self::MultiReal => "multi_real",
            Self::Production => "production",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCacheBackend {
    Disabled,
    Memory,
    Redis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundJobLockBackend {
    Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseMigrationMode {
    Auto,
    ValidateOnly,
    Disabled,
}

impl DatabaseMigrationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ValidateOnly => "validate_only",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Dev,
    Database,
}

/// Retrieval strategy for searches that name none (`MEMQUIRA_SEARCH_STRATEGY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStrategy {
    /// Original fused search: vector leg + substring keyword leg.
    Hybrid,
    /// Vector leg only.
    Semantic,
    /// Ranked full-text leg only.
    Fulltext,
    /// Vector leg + full-text leg, Reciprocal Rank Fusion over rank positions.
    Rrf,
}

impl FromStr for SearchStrategy {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hybrid" => Ok(Self::Hybrid),
            "semantic" | "vector" => Ok(Self::Semantic),
            "fulltext" => Ok(Self::Fulltext),
            "rrf" => Ok(Self::Rrf),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_SEARCH_STRATEGY value: {value}"
            ))),
        }
    }
}

/// Retention policy of the structured extraction prompt (`MEMQUIRA_STRUCTURED_EXTRACTION_POLICY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredExtractionPolicy {
    Atomic,
    Retentive,
}

/// How a new extracted fact is reconciled with the user's stored facts
/// (`MEMQUIRA_CONSOLIDATION_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationMode {
    /// The original path (text and embedding dedup, then the classifier), unchanged. Default.
    Legacy,
    /// Related-fact candidates, fast paths, one model decision
    /// (ADD / UPDATE / INVALIDATE / NOOP) validated in Rust, history kept on invalidation.
    LlmV2,
}

/// How the legacy ingest path finds the stored facts a candidate is classified against
/// (`MEMQUIRA_RELATED_FACT_MATCHER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelatedFactMatcher {
    /// The previous lookup (a stored fact containing the whole candidate text), unchanged. Default.
    Legacy,
    /// Experimental, opt-in, not validated with the real classifier: vector neighbours of the
    /// candidate's embedding at the consolidation similarity floor, same memory type, never a
    /// different subject (no full-text hits); a classifier Update / Delete applies only to an offered
    /// fact of the same subject that is not an event or a separate event time, and keeps history (the
    /// old fact is superseded, not overwritten or deleted). A reference-contract mismatch.
    Semantic,
    /// candidate, experimental and opt-in: `semantic` without the memory-type filter
    /// (an `event` fact is offered only to an `event` candidate) and with canonical subject names
    /// ("team" = "the team"); same floor, guard and history rule. Not validated with the real
    /// classifier. A reference-contract mismatch.
    SemanticV23,
}

/// How structured extraction writes the sentence of a fact with a settled event time
/// (`MEMQUIRA_TEMPORAL_REWRITE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalRewriteSetting {
    /// The settled date is prefixed and the model's time expression removed, whatever it is. Default.
    Legacy,
    /// Opt-in: only a time expression the resolver recognised is removed; a span with no time in it gives no event time; a date that is an attribute's value stays in the sentence. Not answer-validated.
    Preserving,
}

/// What extraction does with a fact whose memory type is outside the vocabulary
/// (`MEMQUIRA_UNKNOWN_MEMORY_TYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownMemoryTypeSetting {
    /// The fact is discarded and counted as `unknown_memory_type`. Default.
    Skip,
    /// Experimental, opt-in: the fact is kept with memory type `other` and counted as a fallback.
    Other,
}

/// Whether the semantic_v23 guard lets an Update supersede an `event` fact
/// (`MEMQUIRA_EVENT_UPDATE_POLICY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventUpdatePolicySetting {
    /// An event is history: an Update targeting it is stored as an Add. Default.
    History,
    /// Experimental, opt-in: an Update may supersede an event without an event time when the
    /// candidate has no event time either and states a change ("now", "no longer", "moved", ...).
    ChangeMarker,
}

/// Whether the near-duplicate dedup rules of the legacy ingest path compare values
/// (`MEMQUIRA_DEDUP_VALUE_GUARD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupValueGuardSetting {
    /// A near-duplicate by wording (token overlap >= 0.90 or embedding cosine >= the threshold) is
    /// discarded. Default.
    Off,
    /// Experimental, opt-in: a near-duplicate is discarded only when it states no value the matched
    /// fact lacks, has the same polarity and a compatible event time; otherwise it continues to the
    /// related-fact lookup and the classifier. Exact-text dedup is unchanged.
    On,
}

/// Which extracted candidates pass the importance threshold (`MEMQUIRA_IMPORTANCE_POLICY`, Memquira
/// V2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportancePolicySetting {
    /// A candidate is stored when its adjusted importance reaches `MEMQUIRA_MIN_IMPORTANCE`. Default.
    Legacy,
    /// Opt-in: also store a non-conversational, non-system fact the extractor rated at least medium (0.5). Not answer-validated.
    Durable,
}

/// Which instructions fact-classification requests carry (`MEMQUIRA_CLASSIFICATION_PROMPT`, Memquira
/// V2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationPromptSetting {
    /// The production fact-classification instructions, unchanged. Default.
    Legacy,
    /// Experimental and opt-in: the same JSON contract with Add / Update / Delete / NoOp defined and invented examples. Not measured with a real model.
    OperationsV1,
}

/// The packing `POST /api/v1/memories/context` uses when a request does not name one
/// (`MEMQUIRA_CONTEXT_DEFAULT_PACKING`). An explicit request `packing` always wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPacking {
    /// Default. The rank-order prefix the internal reference was measured with, and the
    /// only reference-contract value. The default after the
    /// paired internal answer evaluation, in which evidence packing scored lower.
    Prefix,
    /// Opt-in fill.
    Fill,
    /// Opt-in: fill over a fact-anchored order. Retrieval-validated on an internal long-conversation evaluation
    /// (better than fill) but answer-worse in the reference evaluation, so it is a reference-contract mismatch
    /// (`compatible-unvalidated`). Was the default from 2026-10-27 to 2026-10-28.
    Evidence,
}

impl ContextPacking {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prefix => "prefix",
            Self::Fill => "fill",
            Self::Evidence => "evidence",
        }
    }
}

/// How fact and episode vectors are written to Qdrant (`MEMQUIRA_QDRANT_WRITE_MODE`).
/// Unrelated to `MEMQUIRA_QDRANT_ENSURE_INDEXES`, which only controls index creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QdrantWriteModeSetting {
    /// Legacy: every write goes through the filtered pre-delete (the rollback).
    Legacy,
    /// Fresh facts and episodes skip the pre-delete; replacements make one filtered delete.
    Optimized,
}

/// One provider role's endpoint (deployment-level BYOK). Unset fields fall back to
/// `OPENAI_BASE_URL` / `OPENAI_API_KEY`, so a single-endpoint deployment is unchanged.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct RoleEndpoint {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    /// Accept a missing API key for a non-loopback endpoint (a server that needs no auth).
    pub allow_no_api_key: bool,
}

impl std::fmt::Debug for RoleEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleEndpoint")
            .field("base_url", &self.base_url.as_deref().map(redacted_url))
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("allow_no_api_key", &self.allow_no_api_key)
            .finish()
    }
}

/// An endpoint ready to use: URL plus optional key (never printed).
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub base_url: String,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ResolvedEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedEndpoint")
            .field("base_url", &redacted_url(&self.base_url))
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Embedding settings beyond the endpoint. Everything here that changes vectors is
/// part of the embedding identity; the dedup policy is part of it too (D-L3).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmbeddingRoleSettings {
    /// Expected vector size (`MEMQUIRA_EMBEDDING_DIMENSIONS`); default from the model name.
    pub dimensions: Option<usize>,
    /// Send the `dimensions` request field (`MEMQUIRA_EMBEDDING_SEND_DIMENSIONS`, default true;
    /// some servers reject it).
    pub send_dimensions: bool,
    /// L2-normalise returned vectors in Memquira (`MEMQUIRA_EMBEDDING_NORMALIZE`, default false).
    pub normalize: bool,
    pub query_prefix: Option<String>,
    pub document_prefix: Option<String>,
    pub model_revision: Option<String>,
    pub serving_variant: Option<String>,
    /// Explicit embedding-dedup cosine threshold. Unset: Titan v2 keeps its validated 0.92 and
    /// every other (uncalibrated) model gets exact-duplicate-only dedup (D-L3).
    pub dedup_threshold: Option<f32>,
}

/// How JSON output is requested from an OpenAI-compatible chat-completions LLM
/// (`MEMQUIRA_LLM_JSON_MODE`). The `/responses` transport is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LlmJsonMode {
    /// `response_format: {"type": "json_object"}` with the schema in the prompt. Default; the
    /// request bodies are exactly those of earlier releases.
    #[default]
    JsonObject,
    /// `response_format: {"type": "json_schema", ...}` with the same schema: schema-constrained
    /// output (OpenAI structured outputs; Ollama >= 0.5). For small local models.
    JsonSchema,
    /// No `response_format` field: the JSON-only instruction and the schema stay in the system
    /// prompt. For OpenAI-compatible servers that reject `response_format`.
    Instruction,
}

/// The quality tier a configuration may claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTier {
    /// The exact reference configuration (Nova Lite + Titan V2 through
    /// the reference gateway, the settings, every feature off). Historical
    /// evidence only: it says nothing about whether the live provider was verified today.
    ReferenceValidated,
    /// Any other real provider configuration (BYOM, local starter): works, quality not measured.
    CompatibleUnvalidated,
    /// Mock providers (`MEMQUIRA_ALLOW_MOCK=true`): tests and development only.
    DevelopmentMock,
}

impl QualityTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReferenceValidated => "reference-validated",
            Self::CompatibleUnvalidated => "compatible-unvalidated",
            Self::DevelopmentMock => "development-mock",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "reference-validated" => Some(Self::ReferenceValidated),
            "compatible-unvalidated" => Some(Self::CompatibleUnvalidated),
            "development-mock" => Some(Self::DevelopmentMock),
            _ => None,
        }
    }
}

/// The reference profile's model names (the gateway serves Nova Lite and Titan V2 under them).
pub const REFERENCE_LLM_MODEL: &str = "bedrock-nova-lite";
pub const REFERENCE_EMBEDDING_MODEL: &str = "bedrock-titan-embed-v2";
/// The reference store's vector size: memquira's default for `bedrock-titan-embed-v2` (the gateway
/// zero-pads Titan's 1,024-d output to it, as the validated proxy did).
pub const REFERENCE_EMBEDDING_DIMENSIONS: usize = 1536;
/// The reference profile's query-time Qdrant HNSW beam (`MEMQUIRA_QDRANT_SEARCH_HNSW_EF`): the value of
/// the maintainers' acceptance runs on SQLite and PostgreSQL.
/// The reference requires exactly this value; unset (the application default and the operational
/// rollback) and every other beam are `compatible-unvalidated`.
pub const REFERENCE_QDRANT_SEARCH_HNSW_EF: u64 = 256;

/// Deployment-level provider configuration per role. Keys come from the
/// deployment environment only, never from tenants or requests.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRoleSettings {
    pub llm: RoleEndpoint,
    /// `MEMQUIRA_LLM_JSON_MODE` (`json_object` default, or `json_schema`).
    pub llm_json_mode: LlmJsonMode,
    pub embedding: RoleEndpoint,
    pub embedding_options: EmbeddingRoleSettings,
    /// Allow plain `http://` to a non-loopback endpoint (`MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP`).
    pub allow_insecure_http: bool,
    /// Startup embedding probe and identity fingerprint (`MEMQUIRA_PROVIDER_PROBE`, default on).
    pub probe: bool,
    /// Refuse to start unless the configuration has this tier (`MEMQUIRA_REQUIRE_QUALITY_TIER`).
    pub require_quality_tier: Option<QualityTier>,
    /// One small chat call at startup that must return valid JSON (`MEMQUIRA_LLM_STARTUP_PROBE`,
    /// default off: on a paid endpoint every start costs one call).
    pub llm_startup_probe: bool,
}

impl Default for ProviderRoleSettings {
    fn default() -> Self {
        Self {
            llm: RoleEndpoint::default(),
            llm_json_mode: LlmJsonMode::default(),
            embedding: RoleEndpoint::default(),
            embedding_options: EmbeddingRoleSettings {
                send_dimensions: true,
                ..EmbeddingRoleSettings::default()
            },
            allow_insecure_http: false,
            probe: true,
            require_quality_tier: None,
            llm_startup_probe: false,
        }
    }
}

/// `scheme://host[:port]/path` without credentials, query or fragment, for messages and logs.
pub fn redacted_url(raw: &str) -> String {
    match url::Url::parse(raw.trim()) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("?");
            let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
            format!(
                "{}://{}{}{}",
                parsed.scheme(),
                host,
                port,
                parsed.path().trim_end_matches('/')
            )
        }
        Err(_) => "<invalid url>".to_string(),
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Validates one OpenAI-compatible endpoint. Errors name the setting and the redacted
/// URL, never a key.
pub fn validate_provider_endpoint(
    role: &str,
    endpoint: &ResolvedEndpoint,
    allow_no_api_key: bool,
    allow_insecure_http: bool,
) -> MemquiraResult<()> {
    let invalid = |message: String| Err(MemquiraError::ValidationError(message));
    let parsed = match url::Url::parse(endpoint.base_url.trim()) {
        Ok(parsed) => parsed,
        Err(_) => {
            return invalid(format!(
                "MEMQUIRA_{role}_BASE_URL (or OPENAI_BASE_URL) is not a valid URL"
            ));
        }
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return invalid(format!("MEMQUIRA_{role}_BASE_URL must use http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return invalid(format!(
            "MEMQUIRA_{role}_BASE_URL must not embed credentials; use MEMQUIRA_{role}_API_KEY"
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return invalid(format!(
            "MEMQUIRA_{role}_BASE_URL must not contain a query or fragment"
        ));
    }
    let Some(host) = parsed.host_str() else {
        return invalid(format!("MEMQUIRA_{role}_BASE_URL has no host"));
    };
    let loopback = is_loopback_host(host);
    let has_key = endpoint
        .api_key
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty());
    if !has_key && !loopback && !allow_no_api_key {
        return invalid(format!(
            "MEMQUIRA_{role}_API_KEY (or OPENAI_API_KEY) is required for {}; set \
             MEMQUIRA_{role}_ALLOW_NO_API_KEY=true only for a server that needs no authentication",
            redacted_url(&endpoint.base_url)
        ));
    }
    if parsed.scheme() == "http" && !loopback && has_key && !allow_insecure_http {
        return invalid(format!(
            "refusing to send the {role} API key over plain http to {}; use https or set \
             MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP=true",
            redacted_url(&endpoint.base_url)
        ));
    }
    Ok(())
}

/// Admission limits of one request class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionClassLimits {
    /// Requests of the class executing at once.
    pub max_in_flight: usize,
    /// Requests of the class allowed to wait for a slot; beyond it, an immediate 503.
    pub max_queue: usize,
    /// Deadline of an admitted request, queue wait included; the handler is cancelled after it.
    pub deadline_ms: u64,
}

/// Admission control and provider concurrency. Protected API routes only: health,
/// readiness, metrics and docs are never admitted, so they cannot queue behind work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionSettings {
    /// `MEMQUIRA_ADMISSION_CONTROL` (`on` default | `off`). `off` is a rollback switch only; a
    /// release profile keeps it on (D-L5).
    pub enabled: bool,
    pub read: AdmissionClassLimits,
    pub write: AdmissionClassLimits,
    pub admin: AdmissionClassLimits,
    /// Longest wait for a slot before a 503 (`MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS`).
    pub queue_timeout_ms: u64,
    /// `Retry-After` of an overload 503 (`MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS`).
    pub retry_after_seconds: u64,
    /// Chat / extraction / classification / summarisation calls at once, process-wide.
    pub provider_chat_max_concurrency: usize,
    /// Embedding calls at once, process-wide.
    pub provider_embedding_max_concurrency: usize,
    /// Longest wait for a provider slot (also bounded by the request deadline).
    pub provider_permit_timeout_ms: u64,
}

impl Default for AdmissionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            read: AdmissionClassLimits {
                max_in_flight: 16,
                max_queue: 64,
                deadline_ms: 30_000,
            },
            write: AdmissionClassLimits {
                max_in_flight: 8,
                max_queue: 32,
                deadline_ms: 120_000,
            },
            admin: AdmissionClassLimits {
                max_in_flight: 4,
                max_queue: 16,
                deadline_ms: 60_000,
            },
            queue_timeout_ms: 2_000,
            retry_after_seconds: 1,
            provider_chat_max_concurrency: 8,
            provider_embedding_max_concurrency: 16,
            provider_permit_timeout_ms: 10_000,
        }
    }
}

/// Multi-query mode for candidate lists (`MEMQUIRA_MULTI_QUERY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiQuerySetting {
    /// Default: the original query only.
    None,
    /// Clause sub-queries from the deterministic analyzer; no model call.
    Deterministic,
}

/// Reranker for candidate lists (`MEMQUIRA_RERANK_PROVIDER`). `local` and
/// `bedrock` also need a build with the `local-rerank` / `bedrock-rerank` feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankProviderSetting {
    None,
    /// bge-reranker-v2-m3 INT8 on ONNX Runtime, from local files verified by checksum.
    Local,
    /// Amazon Bedrock Rerank 1.0 (Agents Runtime `Rerank`, SigV4).
    Bedrock,
}

/// Query decomposition (`MEMQUIRA_QUERY_DECOMPOSITION`). Only generic plumbing exists:
/// `noop` exercises it without any model call. Model providers wait for the frozen contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryDecompositionSetting {
    None,
    Noop,
    /// The frozen `decompose-v1` contract through the configured LLM provider.
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_filter_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Environment {
    Development,
    Production,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageMode {
    Embedded,
    Production,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VectorBackend {
    Mock,
    LanceDb,
    Qdrant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactBackend {
    Mock,
    Sqlite,
    Postgres,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventBackend {
    Mock,
    Sqlite,
    Postgres,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmProviderKind {
    Mock,
    OpenAi,
    OpenRouter,
    Anthropic,
    Groq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingProviderKind {
    Mock,
    OpenAi,
}

impl EmbeddingProviderKind {
    /// Whether the provider runs a model inside Memquira (local embeddings). Local
    /// providers need `MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR` and the Qdrant backend, whose stores
    /// record and check their embedding identity.
    pub fn is_local(&self) -> bool {
        match self {
            Self::Mock | Self::OpenAi => false,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Settings {
    pub environment: Environment,
    pub host: String,
    pub port: u16,
    pub storage_mode: StorageMode,
    pub vector_backend: VectorBackend,
    pub fact_backend: FactBackend,
    pub event_backend: EventBackend,
    pub database_url: String,
    pub postgres_url: Option<String>,
    pub database_migrations_enabled: bool,
    pub database_migration_mode: DatabaseMigrationMode,
    pub database_require_clean_migrations: bool,
    /// Enable operational database backup helpers (disabled by default).
    pub backup_enabled: bool,
    /// Directory used for local database backup files.
    pub backup_dir: String,
    /// Maximum number of Memquira backup files retained in `backup_dir`.
    pub backup_max_files: usize,
    /// Explicitly allow destructive restore helpers. Dangerous; defaults to false.
    pub restore_enabled: bool,
    /// Apply default HTTP security headers on responses.
    pub security_headers_enabled: bool,
    /// Maximum accepted HTTP request body size in bytes.
    pub max_request_body_bytes: usize,
    /// Enable CORS responses (disabled by default for machine-to-machine APIs).
    pub cors_enabled: bool,
    /// Allowed CORS origins (`*` only when credentials are disabled).
    pub cors_allowed_origins: Vec<String>,
    /// Allowed CORS methods.
    pub cors_allowed_methods: Vec<String>,
    /// Allowed CORS request headers.
    pub cors_allowed_headers: Vec<String>,
    /// Headers exposed to browsers via CORS.
    pub cors_exposed_headers: Vec<String>,
    /// Whether CORS responses may include credentials.
    pub cors_allow_credentials: bool,
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub lancedb_path: String,
    pub lancedb_table: String,
    pub llm_provider: LlmProviderKind,
    pub llm_model: String,
    pub embedding_provider: EmbeddingProviderKind,
    pub embedding_model: String,
    pub enable_pii_redaction: bool,
    /// Structured extraction (subject, event time, source messages). Default `true`
    /// by default; `false` sends exactly the older unstructured
    /// extraction request.
    pub structured_extraction_enabled: bool,
    /// (`MEMQUIRA_EPISODES_ENABLED`, default `false`): store every added message as an
    /// episode (source-message-level memory) before extraction, and serve episode search. Off keeps
    /// every existing behaviour.
    pub episodes_enabled: bool,
    /// (`MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED`, default `false`): serve the
    /// EXPERIMENTAL `POST /api/v1/memories/candidates` (facts and episodes as one E-merge-ordered
    /// candidate list; needs episodes). Not a stable retrieval contract; off leaves every existing
    /// endpoint unchanged.
    pub experimental_candidates_enabled: bool,
    /// (`MEMQUIRA_QUERY_ANALYSIS_ENABLED`, default `false`): run the deterministic query
    /// analyzer on candidate requests. Needs the candidates endpoint.
    pub query_analysis_enabled: bool,
    /// (`MEMQUIRA_TEMPORAL_RANKING_ENABLED`, default `false`): event-time ranking of
    /// candidate lists. Needs query analysis.
    pub temporal_ranking_enabled: bool,
    /// (`MEMQUIRA_TEMPORAL_BOOST`, default 0.05, range (0, 1]): the boost added to
    /// the raw score of facts whose event time overlaps the question's date range.
    pub temporal_boost: f64,
    /// (`MEMQUIRA_MULTI_QUERY`, default `none`).
    pub multi_query: MultiQuerySetting,
    /// (`MEMQUIRA_RERANK_PROVIDER`, default `none`).
    pub rerank_provider: RerankProviderSetting,
    /// (`MEMQUIRA_RERANK_CANDIDATES`, default 60, range 1-100): the rerank / temporal
    /// window.
    pub rerank_candidates: usize,
    /// (`MEMQUIRA_RERANK_TIMEOUT_MS`, default 30000, range 100-600000): bound on one reranker
    /// call; on expiry the prior order is kept.
    pub rerank_timeout_ms: u64,
    /// Local reranker files and checksums (`MEMQUIRA_RERANK_LOCAL_*`); required for `local`.
    pub rerank_local_model_path: Option<String>,
    pub rerank_local_model_sha256: Option<String>,
    pub rerank_local_tokenizer_path: Option<String>,
    pub rerank_local_tokenizer_sha256: Option<String>,
    /// ONNX Runtime intra-op threads (default 4, range 1-64).
    pub rerank_local_threads: usize,
    /// Pairs per inference batch (default 16, range 1-64).
    pub rerank_local_batch_size: usize,
    /// Bedrock reranker region (no default; required for `bedrock`), model ARN (default Amazon
    /// Rerank 1.0 in that region; must name the same region) and an optional AWS profile (else the
    /// default credential chain).
    pub rerank_bedrock_region: String,
    pub rerank_bedrock_model_arn: String,
    pub rerank_bedrock_profile: Option<String>,
    /// (`MEMQUIRA_QUERY_DECOMPOSITION`, default `none`).
    pub query_decomposition: QueryDecompositionSetting,
    /// Bound on the one decomposition call (`MEMQUIRA_QUERY_DECOMPOSITION_TIMEOUT_MS`, default 10000).
    pub query_decomposition_timeout_ms: u64,
    /// Qdrant collection for episode vectors (`MEMQUIRA_QDRANT_EPISODE_COLLECTION`), default
    /// `<MEMQUIRA_QDRANT_COLLECTION>_episodes`. Must differ from the fact collection.
    pub qdrant_episode_collection: String,
    /// Ensure the Qdrant payload indexes (keyword `org_id`, `user_id`, `fact_id`; `memory_type` on
    /// the fact collection) when a collection is opened or created
    /// (`MEMQUIRA_QDRANT_ENSURE_INDEXES`, default true). Only missing indexes are created; points are
    /// never rewritten. `false` is the escape hatch: no index is created or checked.
    pub qdrant_ensure_indexes: bool,
    /// Qdrant write mode (`MEMQUIRA_QDRANT_WRITE_MODE`, `optimized` (default) | `legacy`).
    /// `legacy` is the rollback to the earlier filtered pre-delete on every write.
    pub qdrant_write_mode: QdrantWriteModeSetting,
    /// Query-time HNSW beam of every Qdrant vector search, facts and episodes
    /// (`MEMQUIRA_QDRANT_SEARCH_HNSW_EF`). Unset (the application default) sends no
    /// search parameter, so Qdrant uses its own default, exactly the earlier request. The validated
    /// reference profile sets `REFERENCE_QDRANT_SEARCH_HNSW_EF` (256); unsetting it is the operational
    /// rollback (a restart, no reindex) and makes the configuration `compatible-unvalidated`.
    /// Changes no index, write or payload; only how many graph candidates a search explores.
    pub qdrant_search_hnsw_ef: Option<u64>,
    /// EXPERIMENTAL bounded exact search (`MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS`, launch
    /// No count, the historical search request. Set to N: before
    /// every fact / episode vector search, the tenant's points in the queried collection are counted
    /// (exact count, the same `org_id` + `user_id` filter); a tenant with <= N points is searched with
    /// Qdrant `params.exact = true`, a larger one with the unchanged approximate request. Never part of
    /// the launch reference (any value is `compatible-unvalidated`); unsetting it is the rollback.
    pub qdrant_exact_search_max_tenant_points: Option<u64>,
    /// Retention policy of the structured extraction prompt. Default `Retentive`, adopted after
    /// acceptance; `Atomic` is the original Part B prompt.
    pub structured_extraction_policy: StructuredExtractionPolicy,
    /// Consolidation mode (`MEMQUIRA_CONSOLIDATION_MODE`, default `legacy`). `llm_v2` also
    /// makes search and context leave out invalidated facts unless a request asks for history.
    pub consolidation_mode: ConsolidationMode,
    /// Legacy-path related-fact matcher (`MEMQUIRA_RELATED_FACT_MATCHER`, default `legacy`).
    pub related_fact_matcher: RelatedFactMatcher,
    /// Fact-classification instructions (`MEMQUIRA_CLASSIFICATION_PROMPT`, default `legacy`).
    pub classification_prompt: ClassificationPromptSetting,
    /// Importance threshold policy (`MEMQUIRA_IMPORTANCE_POLICY`, default `legacy`).
    pub importance_policy: ImportancePolicySetting,
    /// Dated-fact sentence rewrite (`MEMQUIRA_TEMPORAL_REWRITE`, default `legacy`).
    pub temporal_rewrite: TemporalRewriteSetting,
    /// Out-of-vocabulary memory types at extraction (`MEMQUIRA_UNKNOWN_MEMORY_TYPE`, default `skip`).
    pub unknown_memory_type: UnknownMemoryTypeSetting,
    /// Event targets under the semantic_v23 guard (`MEMQUIRA_EVENT_UPDATE_POLICY`, default `history`).
    pub event_update_policy: EventUpdatePolicySetting,
    /// Value-aware near-duplicate dedup (`MEMQUIRA_DEDUP_VALUE_GUARD`, default `off`).
    pub dedup_value_guard: DedupValueGuardSetting,
    /// Similarity floor of the semantic related-fact matchers
    /// (`MEMQUIRA_RELATED_FACT_SIMILARITY_FLOOR`, default 0.5).
    pub related_fact_similarity_floor: f32,
    /// Default packing of the server-side context endpoint (`MEMQUIRA_CONTEXT_DEFAULT_PACKING`, default
    /// `prefix`, the reference packing; `fill` / `evidence` opt in without a migration or data change).
    pub context_default_packing: ContextPacking,
    /// Related-fact candidates a v2 decision sees (`MEMQUIRA_CONSOLIDATION_CANDIDATES`, default 10,
    /// range 1-50).
    pub consolidation_candidates: usize,
    /// v2 fast path: with no candidate at or above this vector similarity the new fact is added
    /// without a model call (`MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR`, default 0.5, range 0-1).
    pub consolidation_similarity_floor: f32,
    /// Entity layer (`MEMQUIRA_ENTITIES_ENABLED`, default false): a separate entity
    /// annotation request per extraction window, deterministic resolution and entity storage. Off:
    /// no entity rows, no annotation or confirmation requests.
    pub entities_enabled: bool,
    /// Name-embedding similarity at or above which a new mention is a candidate match for a stored
    /// entity of a compatible type (`MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD`, default 0.70, range
    /// 0-1). Predeclared before any entity measurement.
    pub entity_name_similarity_threshold: f32,
    /// Ask the model to confirm a candidate match (at most one request per new entity,
    /// `MEMQUIRA_ENTITY_LLM_CONFIRMATION_ENABLED`, default true). Off: a candidate match that is not
    /// an exact name or alias creates a new entity.
    pub entity_llm_confirmation_enabled: bool,
    /// Retrieval strategy a search uses when the request names none (`MEMQUIRA_SEARCH_STRATEGY`).
    /// Default `Hybrid`, the original fused search.
    pub search_strategy: SearchStrategy,
    /// Reciprocal Rank Fusion constant `k` (`MEMQUIRA_RRF_K`, default 60): a candidate at rank r in
    /// a list contributes 1 / (k + r).
    pub rrf_k: f32,
    /// Highest search `limit` accepted (`MEMQUIRA_SEARCH_MAX_LIMIT`). Default 50, the public
    /// limit; configurable within 50–200. Requests up to 50 behave exactly as before; above 50 each
    /// retrieval leg fetches exactly the requested limit.
    pub search_max_limit: usize,
    /// (`MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY`, default 0 = off): how many search-query
    /// embeddings to keep in process, keyed by the embedding provider's identity, the organisation
    /// and the exact query (least recently read evicted; at most 1,000,000).
    pub query_embedding_cache_capacity: usize,
    /// `MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED` (default false =
    /// the previous path): on the legacy ingest path, a candidate whose related-fact lookup returns
    /// no fact is added without the classification call (the call can only lead to an Add there);
    /// with related facts the classifier runs unchanged.
    pub classification_skip_when_no_related: bool,
    /// (`MEMQUIRA_INGEST_JOURNAL_ENABLED`, default true): every add request journals
    /// its fact operations before applying them and commits only when it succeeds, so a failed,
    /// cancelled or interrupted request is rolled back (SQL stores: migration 0016). `false` is a
    /// rollback switch to the V2 behaviour (a failed request's adds are compensated in-request).
    pub ingest_journal_enabled: bool,
    /// (`MEMQUIRA_INGEST_RECOVERY_STALE_SECS`, default 300): a journalled operation
    /// untouched this long is abandoned and the recovery sweep rolls it back. Must exceed the write
    /// admission deadline.
    pub ingest_recovery_stale_secs: u64,
    /// (`MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS`, default 60): how often the recovery
    /// sweep runs (also once at startup).
    pub ingest_recovery_interval_secs: u64,
    /// (`MEMQUIRA_INGEST_INFLIGHT_WAIT_MS`, default 10,000): longest a request waits for
    /// another request's uncommitted fact before answering `409 CONFLICT`.
    pub ingest_inflight_wait_ms: u64,
    /// closure (`MEMQUIRA_INGEST_RECEIPT_TTL_SECS`, default 86,400): how long the
    /// receipt of a successful add request with stable message ids is kept, so a retry of that exact
    /// request (e.g. after a lost response) returns the recorded result instead of running again.
    pub ingest_receipt_ttl_secs: u64,
    /// Records a complete export may hold (`MEMQUIRA_EXPORT_MAX_RECORDS`, default 100,000);
    /// a larger complete export is refused, never truncated.
    pub export_max_records: usize,
    /// Directory holding a local embedding model's pinned files
    /// (`MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR`); required by local providers. The files are verified
    /// against their SHA-256 at load; nothing is ever downloaded.
    pub local_embedding_model_dir: Option<String>,
    /// Pending local-inference jobs before requests are refused
    /// (`MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY`, default 16, 1-1024).
    pub local_embedding_queue_capacity: usize,
    /// RRF weight of the vector leg (`MEMQUIRA_RRF_WEIGHT_SEMANTIC`, default 1.0, >= 0).
    pub rrf_weight_semantic: f32,
    /// RRF weight of the full-text leg (`MEMQUIRA_RRF_WEIGHT_KEYWORD`, default 1.0, >= 0).
    pub rrf_weight_keyword: f32,
    /// Messages per structured extraction window.
    pub extraction_window_turns: usize,
    /// Messages repeated between consecutive windows.
    pub extraction_window_overlap: usize,
    /// Windows extracted concurrently per request.
    pub extraction_concurrency: usize,
    pub min_importance: f32,
    /// Temporary development auth toggle.
    pub auth_enabled: bool,
    /// Authentication mode: dev API key or database-backed hashed keys.
    pub auth_mode: AuthMode,
    /// Temporary plaintext dev API key. Do not log this value.
    pub dev_api_key: String,
    /// Pepper for HMAC hashing of database-backed API keys. Required in database auth mode.
    pub api_key_pepper: Option<String>,
    /// OpenAI API key. Required only when LLM or embedding provider is OpenAI.
    pub openai_api_key: Option<String>,
    /// OpenAI API base URL (supports OpenAI-compatible gateways).
    pub openai_base_url: String,
    /// In-memory rate limiting toggle for protected API routes.
    pub rate_limit_enabled: bool,
    /// Maximum protected-route requests per organization per minute.
    pub rate_limit_requests_per_minute: u32,
    /// Admission control and provider concurrency.
    pub admission: AdmissionSettings,
    /// Per-role provider endpoints and embedding options (BYOK).
    pub providers: ProviderRoleSettings,
    /// Structured log output format.
    pub log_format: LogFormat,
    /// Minimum tracing log level.
    pub log_level: LogLevel,
    /// HTTP header used for request correlation IDs.
    pub request_id_header: String,
    /// Expose in-process metrics at `GET /metrics`.
    pub metrics_enabled: bool,
    /// Prometheus scrape path (must start with `/`).
    pub metrics_path: String,
    /// When true, scrape requires a valid API key (AdminRead in database auth).
    pub metrics_require_auth: bool,
    /// When true, expose basic build info gauge (no env dump).
    pub metrics_include_process: bool,
    /// Global retention toggle for user-scoped retention apply endpoint.
    pub retention_enabled: bool,
    /// Default fact retention window in days (`0` disables fact retention).
    pub fact_retention_days: u32,
    /// Default event retention window in days (`0` disables event retention).
    pub event_retention_days: u32,
    /// In-memory context response cache toggle (derived from `context_cache_backend`).
    pub context_cache_enabled: bool,
    /// Context cache storage backend.
    pub context_cache_backend: ContextCacheBackend,
    /// Context cache entry TTL in seconds.
    pub context_cache_ttl_seconds: u64,
    /// Maximum in-memory context cache entries per process.
    pub context_cache_max_entries: usize,
    /// Redis URL for context cache when backend is `redis`.
    pub redis_url: Option<String>,
    /// Namespace prefix for Redis context cache keys.
    pub context_cache_key_prefix: String,
    /// Coalesce in-process concurrent context cache misses for the same key.
    pub context_cache_stampede_protection_enabled: bool,
    /// Maximum seconds to wait for an in-flight context cache computation.
    pub context_cache_lock_timeout_seconds: u64,
    /// Serve slightly expired context while refreshing in the background.
    pub context_cache_stale_while_revalidate_enabled: bool,
    /// Seconds after fresh TTL expires during which stale context may be served.
    pub context_cache_stale_ttl_seconds: u64,
    /// Record in-process context cache observability counters.
    pub context_cache_metrics_enabled: bool,
    /// The operator's explicit local-development opt-in for the
    /// process-local `memory` cache (`MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS`). A statement,
    /// not an enforcement: it is never accepted with a production marker
    /// (see [`Settings::check_context_cache_policy`]).
    pub context_cache_local_single_process: bool,
    /// Per-provider-call timeout in seconds.
    pub provider_timeout_seconds: u64,
    /// Maximum retries after the initial provider call attempt.
    pub provider_max_retries: usize,
    /// Initial retry backoff in milliseconds.
    pub provider_initial_backoff_ms: u64,
    /// Maximum retry backoff in milliseconds.
    pub provider_max_backoff_ms: u64,
    /// Exponential backoff multiplier between retries.
    pub provider_backoff_multiplier: f32,
    /// Apply simple jitter to retry backoff delays.
    pub provider_retry_jitter_enabled: bool,
    /// Try alternate providers after retry-exhausted primary provider failures.
    pub provider_fallback_enabled: bool,
    /// Comma-separated LLM provider fallback order when fallback is enabled.
    pub llm_fallback_order: Vec<String>,
    /// Comma-separated embedding provider fallback order when fallback is enabled.
    pub embedding_fallback_order: Vec<String>,
    /// Comma-separated summarizer provider fallback order when fallback is enabled.
    pub summarizer_fallback_order: Vec<String>,
    /// Process-local provider circuit breaker toggle.
    pub provider_circuit_breaker_enabled: bool,
    /// Retry-exhausted failures before opening a provider circuit.
    pub provider_circuit_breaker_failure_threshold: usize,
    /// Seconds before an open circuit transitions to half-open.
    pub provider_circuit_breaker_reset_timeout_seconds: u64,
    /// Maximum probe calls allowed while a circuit is half-open.
    pub provider_circuit_breaker_half_open_max_calls: usize,
    /// Record process-local provider usage counters.
    pub provider_usage_metrics_enabled: bool,
    /// Estimate provider cost from static pricing hints when token usage is available.
    pub provider_cost_tracking_enabled: bool,
    /// Persist provider usage events to the configured database store.
    pub provider_usage_persistence_enabled: bool,
    /// Forward a request's `X-Memquira-Operation` token on its OpenAI-compatible provider calls, for a trusted
    /// provider-side proxy that charges each call to the caller's operation. Off by default.
    pub provider_operation_header_enabled: bool,
    /// Hosts (`host` or `host:port`) of the trusted proxy that may receive the token
    /// (`MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS`); required when forwarding is enabled. No other provider URL ever
    /// receives it.
    pub provider_operation_header_hosts: Vec<String>,
    /// Default provider usage event retention window in days (`0` disables cleanup).
    pub provider_usage_retention_days: u32,
    /// Global organization quota enforcement toggle.
    pub quotas_enabled: bool,
    /// Maximum users with active memories per org (`0` means unlimited).
    pub max_users_per_org: u64,
    /// Maximum active memories per user (`0` means unlimited).
    pub max_memories_per_user: u64,
    /// Maximum active memories per org (`0` means unlimited).
    pub max_memories_per_org: u64,
    /// Maximum persisted provider requests per UTC day (`0` means unlimited).
    pub daily_provider_request_limit: u64,
    /// Maximum persisted provider tokens per UTC day (`0` means unlimited).
    pub daily_provider_token_limit: u64,
    /// Enforce provider test-mode / cost guardrails.
    pub provider_guardrails_enabled: bool,
    /// Allow real (non-mock) provider network calls when other mode rules pass.
    pub real_provider_calls_enabled: bool,
    /// Provider test mode (`mock_only` by default).
    pub provider_test_mode: ProviderTestMode,
    /// Max real provider calls per process run (in-memory budget).
    pub provider_max_calls_per_run: usize,
    /// Max input characters per guarded provider call.
    pub provider_max_input_chars: usize,
    /// Max completion/output tokens for guarded LLM calls — the ceiling every per-operation limit
    /// below is held to.
    pub provider_max_output_tokens: usize,
    /// Output-token limit for fact extraction. `None` means use `provider_max_output_tokens`; see
    /// `effective_extraction_max_output_tokens`.
    pub extraction_max_output_tokens: Option<usize>,
    /// Output-token limit for fact classification. `None` means
    /// `DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS`, capped at the ceiling.
    pub classification_max_output_tokens: Option<usize>,
    /// Cap on provider retries when guardrails are enabled.
    pub provider_max_retries_per_call: usize,
    /// Allow real providers when request source is load-test.
    pub allow_real_providers_during_load_tests: bool,
    /// Allow real providers from background jobs.
    pub background_jobs_allow_real_providers: bool,
    /// Enable multi-provider validation (requires confirmation for multi_real).
    pub multi_provider_validation_enabled: bool,
    /// Confirmation string for multi-provider credit-using validation.
    pub multi_provider_validation_confirmation: Option<String>,
    /// Global in-process background job runner toggle.
    pub background_jobs_enabled: bool,
    /// Runner polling interval in seconds.
    pub background_job_runner_interval_seconds: u64,
    /// Organization ids targeted by org-scoped background jobs.
    pub background_job_org_ids: Vec<String>,
    /// Enable scheduled memory usage snapshot capture.
    pub memory_usage_snapshot_job_enabled: bool,
    /// Memory usage snapshot job interval in seconds.
    pub memory_usage_snapshot_job_interval_seconds: u64,
    /// Enable scheduled provider usage retention cleanup.
    pub provider_usage_retention_job_enabled: bool,
    /// Provider usage retention job interval in seconds.
    pub provider_usage_retention_job_interval_seconds: u64,
    /// Enable scheduled memory retention job foundation.
    pub memory_retention_job_enabled: bool,
    /// Memory retention job interval in seconds.
    pub memory_retention_job_interval_seconds: u64,
    /// Persist background job run history to the configured storage backend.
    pub background_job_history_enabled: bool,
    /// Default background job run history retention window in days (`0` disables cleanup).
    pub background_job_history_retention_days: u32,
    /// Enable distributed background job locks.
    pub background_job_lock_enabled: bool,
    /// Distributed background job lock backend.
    pub background_job_lock_backend: BackgroundJobLockBackend,
    /// Distributed background job lock TTL in seconds.
    pub background_job_lock_ttl_seconds: u64,
    /// Optional lock owner id. Empty means API bootstrap generates one.
    pub background_job_lock_owner_id: Option<String>,
    /// Enable bounded in-process retries for failed background jobs.
    pub background_job_retries_enabled: bool,
    /// Maximum retry attempts after the initial background job attempt.
    pub background_job_max_retries: usize,
    /// Initial background job retry backoff in milliseconds.
    pub background_job_initial_backoff_ms: u64,
    /// Maximum background job retry backoff in milliseconds.
    pub background_job_max_backoff_ms: u64,
    /// Exponential backoff multiplier between background job retries.
    pub background_job_backoff_multiplier: f32,
    /// Apply simple jitter to background job retry backoff delays.
    pub background_job_retry_jitter_enabled: bool,
    /// Maximum time to wait for API server graceful shutdown after a signal.
    pub graceful_shutdown_timeout_seconds: u64,
    /// Explicit opt-in to mock LLM / embedding providers when the server starts (tests and local
    /// development only). Without it the server refuses to start on a mock provider.
    pub allow_mock: bool,
    /// How long server startup waits for Qdrant / Postgres to become reachable, with bounded
    /// backoff; `0` makes a single attempt.
    pub startup_dependency_timeout_seconds: u64,
    /// Maximum time to wait for a background job attempt to finish after shutdown starts.
    pub background_job_shutdown_timeout_seconds: u64,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("environment", &self.environment)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("storage_mode", &self.storage_mode)
            .field("vector_backend", &self.vector_backend)
            .field("fact_backend", &self.fact_backend)
            .field("event_backend", &self.event_backend)
            .field("database_url", &redacted_secret(&self.database_url))
            .field(
                "postgres_url",
                &self
                    .postgres_url
                    .as_ref()
                    .map(|value| redacted_secret(value)),
            )
            .field(
                "database_migrations_enabled",
                &self.database_migrations_enabled,
            )
            .field("database_migration_mode", &self.database_migration_mode)
            .field(
                "database_require_clean_migrations",
                &self.database_require_clean_migrations,
            )
            .field("backup_enabled", &self.backup_enabled)
            .field("backup_dir", &self.backup_dir)
            .field("backup_max_files", &self.backup_max_files)
            .field("restore_enabled", &self.restore_enabled)
            .field("security_headers_enabled", &self.security_headers_enabled)
            .field("max_request_body_bytes", &self.max_request_body_bytes)
            .field("cors_enabled", &self.cors_enabled)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("cors_allowed_methods", &self.cors_allowed_methods)
            .field("cors_allowed_headers", &self.cors_allowed_headers)
            .field("cors_exposed_headers", &self.cors_exposed_headers)
            .field("cors_allow_credentials", &self.cors_allow_credentials)
            .field("qdrant_url", &self.qdrant_url)
            .field("qdrant_collection", &self.qdrant_collection)
            .field("lancedb_path", &self.lancedb_path)
            .field("lancedb_table", &self.lancedb_table)
            .field("llm_provider", &self.llm_provider)
            .field("llm_model", &self.llm_model)
            .field("embedding_provider", &self.embedding_provider)
            .field("embedding_model", &self.embedding_model)
            .field("enable_pii_redaction", &self.enable_pii_redaction)
            .field(
                "structured_extraction_enabled",
                &self.structured_extraction_enabled,
            )
            .field("episodes_enabled", &self.episodes_enabled)
            .field(
                "experimental_candidates_enabled",
                &self.experimental_candidates_enabled,
            )
            .field("query_analysis_enabled", &self.query_analysis_enabled)
            .field("temporal_ranking_enabled", &self.temporal_ranking_enabled)
            .field("temporal_boost", &self.temporal_boost)
            .field("multi_query", &self.multi_query)
            .field("rerank_provider", &self.rerank_provider)
            .field("rerank_candidates", &self.rerank_candidates)
            .field("rerank_timeout_ms", &self.rerank_timeout_ms)
            .field("rerank_local_model_path", &self.rerank_local_model_path)
            .field("rerank_local_model_sha256", &self.rerank_local_model_sha256)
            .field(
                "rerank_local_tokenizer_path",
                &self.rerank_local_tokenizer_path,
            )
            .field(
                "rerank_local_tokenizer_sha256",
                &self.rerank_local_tokenizer_sha256,
            )
            .field("rerank_local_threads", &self.rerank_local_threads)
            .field("rerank_local_batch_size", &self.rerank_local_batch_size)
            .field("rerank_bedrock_region", &self.rerank_bedrock_region)
            .field("rerank_bedrock_model_arn", &self.rerank_bedrock_model_arn)
            .field("rerank_bedrock_profile", &self.rerank_bedrock_profile)
            .field("query_decomposition", &self.query_decomposition)
            .field(
                "query_decomposition_timeout_ms",
                &self.query_decomposition_timeout_ms,
            )
            .field("qdrant_episode_collection", &self.qdrant_episode_collection)
            .field("qdrant_ensure_indexes", &self.qdrant_ensure_indexes)
            .field("qdrant_write_mode", &self.qdrant_write_mode)
            .field("qdrant_search_hnsw_ef", &self.qdrant_search_hnsw_ef)
            .field(
                "qdrant_exact_search_max_tenant_points",
                &self.qdrant_exact_search_max_tenant_points,
            )
            .field(
                "structured_extraction_policy",
                &self.structured_extraction_policy,
            )
            .field("consolidation_mode", &self.consolidation_mode)
            .field("related_fact_matcher", &self.related_fact_matcher)
            .field("classification_prompt", &self.classification_prompt)
            .field("importance_policy", &self.importance_policy)
            .field("temporal_rewrite", &self.temporal_rewrite)
            .field("unknown_memory_type", &self.unknown_memory_type)
            .field("event_update_policy", &self.event_update_policy)
            .field("dedup_value_guard", &self.dedup_value_guard)
            .field(
                "related_fact_similarity_floor",
                &self.related_fact_similarity_floor,
            )
            .field("context_default_packing", &self.context_default_packing)
            .field("consolidation_candidates", &self.consolidation_candidates)
            .field(
                "consolidation_similarity_floor",
                &self.consolidation_similarity_floor,
            )
            .field("entities_enabled", &self.entities_enabled)
            .field(
                "entity_name_similarity_threshold",
                &self.entity_name_similarity_threshold,
            )
            .field(
                "entity_llm_confirmation_enabled",
                &self.entity_llm_confirmation_enabled,
            )
            .field("search_strategy", &self.search_strategy)
            .field("rrf_k", &self.rrf_k)
            .field("search_max_limit", &self.search_max_limit)
            .field(
                "query_embedding_cache_capacity",
                &self.query_embedding_cache_capacity,
            )
            .field(
                "classification_skip_when_no_related",
                &self.classification_skip_when_no_related,
            )
            .field("ingest_journal_enabled", &self.ingest_journal_enabled)
            .field(
                "ingest_recovery_stale_secs",
                &self.ingest_recovery_stale_secs,
            )
            .field(
                "ingest_recovery_interval_secs",
                &self.ingest_recovery_interval_secs,
            )
            .field("ingest_inflight_wait_ms", &self.ingest_inflight_wait_ms)
            .field("ingest_receipt_ttl_secs", &self.ingest_receipt_ttl_secs)
            .field("local_embedding_model_dir", &self.local_embedding_model_dir)
            .field(
                "local_embedding_queue_capacity",
                &self.local_embedding_queue_capacity,
            )
            .field("rrf_weight_semantic", &self.rrf_weight_semantic)
            .field("rrf_weight_keyword", &self.rrf_weight_keyword)
            .field("extraction_window_turns", &self.extraction_window_turns)
            .field("extraction_window_overlap", &self.extraction_window_overlap)
            .field("extraction_concurrency", &self.extraction_concurrency)
            .field("min_importance", &self.min_importance)
            .field("auth_enabled", &self.auth_enabled)
            .field("auth_mode", &self.auth_mode)
            .field("dev_api_key", &redacted_secret(&self.dev_api_key))
            .field(
                "api_key_pepper",
                &self
                    .api_key_pepper
                    .as_ref()
                    .map(|value| redacted_present(value)),
            )
            .field(
                "openai_api_key",
                &self
                    .openai_api_key
                    .as_ref()
                    .map(|value| redacted_present(value)),
            )
            .field("openai_base_url", &self.openai_base_url)
            .field("rate_limit_enabled", &self.rate_limit_enabled)
            .field(
                "rate_limit_requests_per_minute",
                &self.rate_limit_requests_per_minute,
            )
            .field("admission", &self.admission)
            .field("providers", &self.providers)
            .field("log_format", &self.log_format)
            .field("log_level", &self.log_level)
            .field("request_id_header", &self.request_id_header)
            .field("metrics_enabled", &self.metrics_enabled)
            .field("metrics_path", &self.metrics_path)
            .field("metrics_require_auth", &self.metrics_require_auth)
            .field("metrics_include_process", &self.metrics_include_process)
            .field("context_cache_backend", &self.context_cache_backend)
            .field(
                "context_cache_local_single_process",
                &self.context_cache_local_single_process,
            )
            .field(
                "redis_url",
                &self.redis_url.as_ref().map(|value| redacted_secret(value)),
            )
            .field("quotas_enabled", &self.quotas_enabled)
            .field("background_jobs_enabled", &self.background_jobs_enabled)
            .finish_non_exhaustive()
    }
}

fn redacted_secret(value: &str) -> &'static str {
    if value.trim().is_empty() {
        ""
    } else {
        "[REDACTED]"
    }
}

fn redacted_present(value: &str) -> &'static str {
    if value.trim().is_empty() {
        ""
    } else {
        "[REDACTED]"
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            environment: Environment::Development,
            host: "0.0.0.0".to_string(),
            port: 8080,
            storage_mode: StorageMode::Embedded,
            vector_backend: VectorBackend::Mock,
            fact_backend: FactBackend::Mock,
            event_backend: EventBackend::Mock,
            database_url: "sqlite://./data/memcore.db".to_string(),
            postgres_url: None,
            database_migrations_enabled: true,
            database_migration_mode: DatabaseMigrationMode::Auto,
            database_require_clean_migrations: true,
            backup_enabled: false,
            backup_dir: "./backups".to_string(),
            backup_max_files: 10,
            restore_enabled: false,
            security_headers_enabled: true,
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
            cors_enabled: false,
            cors_allowed_origins: Vec::new(),
            cors_allowed_methods: DEFAULT_CORS_ALLOWED_METHODS
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            cors_allowed_headers: DEFAULT_CORS_ALLOWED_HEADERS
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            cors_exposed_headers: Vec::new(),
            cors_allow_credentials: false,
            qdrant_url: "http://localhost:6334".to_string(),
            qdrant_collection: "memcore_vectors".to_string(),
            lancedb_path: "./data/lancedb".to_string(),
            lancedb_table: "memcore_vectors".to_string(),
            llm_provider: LlmProviderKind::Mock,
            llm_model: "mock-llm".to_string(),
            embedding_provider: EmbeddingProviderKind::Mock,
            embedding_model: "mock-embedding".to_string(),
            enable_pii_redaction: true,
            structured_extraction_enabled: true,
            episodes_enabled: false,
            experimental_candidates_enabled: false,
            query_analysis_enabled: false,
            temporal_ranking_enabled: false,
            temporal_boost: 0.05,
            multi_query: MultiQuerySetting::None,
            rerank_provider: RerankProviderSetting::None,
            rerank_candidates: 60,
            rerank_timeout_ms: 30_000,
            rerank_local_model_path: None,
            rerank_local_model_sha256: None,
            rerank_local_tokenizer_path: None,
            rerank_local_tokenizer_sha256: None,
            rerank_local_threads: 4,
            rerank_local_batch_size: 16,
            rerank_bedrock_region: String::new(),
            rerank_bedrock_model_arn: String::new(),
            rerank_bedrock_profile: None,
            query_decomposition: QueryDecompositionSetting::None,
            query_decomposition_timeout_ms: 10_000,
            qdrant_episode_collection: "memcore_vectors_episodes".to_string(),
            qdrant_ensure_indexes: true,
            qdrant_write_mode: QdrantWriteModeSetting::Optimized,
            qdrant_search_hnsw_ef: None,
            qdrant_exact_search_max_tenant_points: None,
            structured_extraction_policy: StructuredExtractionPolicy::Retentive,
            consolidation_mode: ConsolidationMode::Legacy,
            related_fact_matcher: RelatedFactMatcher::Legacy,
            temporal_rewrite: TemporalRewriteSetting::Legacy,
            unknown_memory_type: UnknownMemoryTypeSetting::Skip,
            event_update_policy: EventUpdatePolicySetting::History,
            dedup_value_guard: DedupValueGuardSetting::Off,
            related_fact_similarity_floor: 0.5,
            importance_policy: ImportancePolicySetting::Legacy,
            classification_prompt: ClassificationPromptSetting::Legacy,
            context_default_packing: ContextPacking::Prefix,
            consolidation_candidates: 10,
            consolidation_similarity_floor: 0.5,
            entities_enabled: false,
            entity_name_similarity_threshold: 0.70,
            entity_llm_confirmation_enabled: true,
            search_strategy: SearchStrategy::Hybrid,
            rrf_k: 60.0,
            search_max_limit: 50,
            query_embedding_cache_capacity: 0,
            classification_skip_when_no_related: false,
            ingest_journal_enabled: true,
            ingest_recovery_stale_secs: 300,
            ingest_recovery_interval_secs: 60,
            ingest_inflight_wait_ms: 10_000,
            ingest_receipt_ttl_secs: 86_400,
            export_max_records: 100_000,
            local_embedding_model_dir: None,
            local_embedding_queue_capacity: 16,
            rrf_weight_semantic: 1.0,
            rrf_weight_keyword: 1.0,
            extraction_window_turns: 20,
            extraction_window_overlap: 2,
            extraction_concurrency: 4,
            min_importance: 0.55,
            auth_enabled: true,
            auth_mode: AuthMode::Dev,
            dev_api_key: "memcore_dev_key".to_string(),
            api_key_pepper: None,
            openai_api_key: None,
            openai_base_url: DEFAULT_OPENAI_BASE_URL.to_string(),
            rate_limit_enabled: true,
            rate_limit_requests_per_minute: 60,
            admission: AdmissionSettings::default(),
            providers: ProviderRoleSettings::default(),
            log_format: LogFormat::Json,
            log_level: LogLevel::Info,
            request_id_header: DEFAULT_REQUEST_ID_HEADER.to_string(),
            metrics_enabled: false,
            metrics_path: DEFAULT_METRICS_PATH.to_string(),
            metrics_require_auth: true,
            metrics_include_process: false,
            retention_enabled: false,
            fact_retention_days: 0,
            event_retention_days: 0,
            context_cache_enabled: false,
            context_cache_backend: ContextCacheBackend::Disabled,
            context_cache_ttl_seconds: 300,
            context_cache_max_entries: 1000,
            redis_url: None,
            context_cache_key_prefix: DEFAULT_CONTEXT_CACHE_KEY_PREFIX.to_string(),
            context_cache_stampede_protection_enabled: true,
            context_cache_lock_timeout_seconds: 30,
            context_cache_stale_while_revalidate_enabled: false,
            context_cache_stale_ttl_seconds: 120,
            context_cache_metrics_enabled: true,
            context_cache_local_single_process: false,
            provider_timeout_seconds: 30,
            provider_max_retries: 2,
            provider_initial_backoff_ms: 250,
            provider_max_backoff_ms: 2000,
            provider_backoff_multiplier: 2.0,
            provider_retry_jitter_enabled: true,
            provider_fallback_enabled: false,
            llm_fallback_order: vec!["openai".to_string()],
            embedding_fallback_order: vec!["openai".to_string()],
            summarizer_fallback_order: vec!["openai".to_string()],
            provider_circuit_breaker_enabled: true,
            provider_circuit_breaker_failure_threshold: 5,
            provider_circuit_breaker_reset_timeout_seconds: 60,
            provider_circuit_breaker_half_open_max_calls: 1,
            provider_usage_metrics_enabled: true,
            provider_cost_tracking_enabled: false,
            provider_usage_persistence_enabled: false,
            provider_operation_header_enabled: false,
            provider_operation_header_hosts: Vec::new(),
            provider_usage_retention_days: 180,
            quotas_enabled: false,
            max_users_per_org: 0,
            max_memories_per_user: 0,
            max_memories_per_org: 0,
            daily_provider_request_limit: 0,
            daily_provider_token_limit: 0,
            provider_guardrails_enabled: true,
            real_provider_calls_enabled: false,
            provider_test_mode: ProviderTestMode::MockOnly,
            provider_max_calls_per_run: 10,
            provider_max_input_chars: 4000,
            provider_max_output_tokens: DEFAULT_PROVIDER_MAX_OUTPUT_TOKENS,
            extraction_max_output_tokens: None,
            classification_max_output_tokens: None,
            provider_max_retries_per_call: 1,
            allow_real_providers_during_load_tests: false,
            background_jobs_allow_real_providers: false,
            multi_provider_validation_enabled: false,
            multi_provider_validation_confirmation: None,
            background_jobs_enabled: false,
            background_job_runner_interval_seconds: 60,
            background_job_org_ids: Vec::new(),
            memory_usage_snapshot_job_enabled: false,
            memory_usage_snapshot_job_interval_seconds: 86_400,
            provider_usage_retention_job_enabled: false,
            provider_usage_retention_job_interval_seconds: 86_400,
            memory_retention_job_enabled: false,
            memory_retention_job_interval_seconds: 86_400,
            background_job_history_enabled: true,
            background_job_history_retention_days: 30,
            background_job_lock_enabled: false,
            background_job_lock_backend: BackgroundJobLockBackend::Database,
            background_job_lock_ttl_seconds: 300,
            background_job_lock_owner_id: None,
            background_job_retries_enabled: true,
            background_job_max_retries: 2,
            background_job_initial_backoff_ms: 500,
            background_job_max_backoff_ms: 5000,
            background_job_backoff_multiplier: 2.0,
            background_job_retry_jitter_enabled: true,
            graceful_shutdown_timeout_seconds: 30,
            allow_mock: false,
            startup_dependency_timeout_seconds: 60,
            background_job_shutdown_timeout_seconds: 30,
        }
    }
}

/// How the configured providers passed the server-startup provider check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStartupMode {
    /// Every provider role uses a real provider.
    Real,
    /// At least one role uses the mock provider, explicitly allowed by `MEMQUIRA_ALLOW_MOCK=true`.
    MockAllowed,
}

const MOCK_PROVIDER_REFUSAL: &str = "mock providers return fake extractions and fake embeddings, \
     so stored memories are silently wrong (distinct facts collapse as duplicates)";

impl Settings {
    /// Server-startup provider check. The settings default to the mock LLM and embedding providers
    /// so tests need no credentials; a server must never run on them by accident.
    ///
    /// - A mock provider in any role is refused unless `MEMQUIRA_ALLOW_MOCK=true`.
    /// - A fallback order that mixes `mock` with a real provider is always refused: a failing real
    ///   provider would otherwise be replaced silently by the mock model.
    pub fn check_startup_providers(&self) -> MemquiraResult<ProviderStartupMode> {
        let roles: Vec<(&str, Vec<String>)> = if self.provider_fallback_enabled {
            vec![
                (MEMQUIRA_LLM_FALLBACK_ORDER, self.llm_fallback_order.clone()),
                (
                    MEMQUIRA_SUMMARIZER_FALLBACK_ORDER,
                    self.summarizer_fallback_order.clone(),
                ),
                (
                    MEMQUIRA_EMBEDDING_FALLBACK_ORDER,
                    self.embedding_fallback_order.clone(),
                ),
            ]
        } else {
            let llm = if self.llm_provider == LlmProviderKind::Mock {
                "mock"
            } else {
                "real"
            };
            let embedding = if self.embedding_provider == EmbeddingProviderKind::Mock {
                "mock"
            } else {
                "real"
            };
            vec![
                (MEMQUIRA_LLM_PROVIDER, vec![llm.to_string()]),
                (MEMQUIRA_EMBEDDING_PROVIDER, vec![embedding.to_string()]),
            ]
        };

        let mut mock_roles = Vec::new();
        for (key, names) in &roles {
            let is_mock = |name: &String| name.trim().eq_ignore_ascii_case("mock");
            let has_mock = names.iter().any(is_mock);
            let has_real = names.iter().any(|name| !is_mock(name));
            if has_mock && has_real {
                return Err(MemquiraError::ValidationError(format!(
                    "{key} mixes `mock` with a real provider: a failing real provider would be \
                     replaced silently by the mock model. Remove `mock` from {key}."
                )));
            }
            if has_mock {
                mock_roles.push(*key);
            }
        }

        if mock_roles.is_empty() {
            return Ok(ProviderStartupMode::Real);
        }
        if self.allow_mock {
            return Ok(ProviderStartupMode::MockAllowed);
        }
        Err(MemquiraError::ValidationError(format!(
            "refusing to start: {} {} to the mock provider (the default when unset); \
             {MOCK_PROVIDER_REFUSAL}. Configure real providers (MEMQUIRA_LLM_PROVIDER / \
             MEMQUIRA_EMBEDDING_PROVIDER, OPENAI_BASE_URL, OPENAI_API_KEY), or set \
             MEMQUIRA_ALLOW_MOCK=true for tests and local development only.",
            mock_roles.join(" and "),
            if mock_roles.len() == 1 {
                "resolves"
            } else {
                "resolve"
            }
        )))
    }
}

impl Settings {
    /// Output-token limit for fact extraction: `MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS` when set,
    /// otherwise the provider ceiling `MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS`.
    pub fn effective_extraction_max_output_tokens(&self) -> usize {
        self.extraction_max_output_tokens
            .unwrap_or(self.provider_max_output_tokens)
    }

    /// Output-token limit for fact classification: `MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS` when
    /// set, otherwise `DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS`, never above the provider ceiling.
    pub fn effective_classification_max_output_tokens(&self) -> usize {
        self.classification_max_output_tokens
            .unwrap_or(DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS)
            .min(self.provider_max_output_tokens)
    }

    pub fn from_env() -> MemquiraResult<Self> {
        // Legacy MEMCORE_* names are accepted as aliases; a legacy and a current name set to
        // different values is ambiguous and refused before anything is read.
        if let Some(message) =
            memquira_common::env::conflict_error(&memquira_common::env::legacy_report())
        {
            return Err(MemquiraError::ValidationError(message));
        }
        let defaults = Self::default();

        let environment = Environment::from_str(&read_env_or(MEMQUIRA_ENV, "development"))?;
        let host = read_env_or(MEMQUIRA_HOST, &defaults.host);
        let port = parse_u16(MEMQUIRA_PORT, defaults.port)?;
        let storage_mode = StorageMode::from_str(&read_env_or(MEMQUIRA_STORAGE_MODE, "embedded"))?;
        // Qdrant is the default. The legacy `lancedb` cargo
        // feature: MEMQUIRA_VECTOR_BACKEND=lancedb still parses but is refused at
        // startup. It was never a safe default: Memquira upserts one vector per fact and LanceDB
        // is append-only, so every fact creates a new dataset version. Measured
        // in the baseline at 2,200 facts — 4,402 versions and 364 MB of
        // manifests against 19 MB of vectors, search p50 1,245 ms against
        // Qdrant's 19.4 ms on identical data, and an ingest rate that fell to 4.6%
        // of its starting value within a single load.
        let vector_backend =
            VectorBackend::from_str(&read_env_or(MEMQUIRA_VECTOR_BACKEND, "qdrant"))?;
        let fact_backend = FactBackend::from_str(&read_env_or(MEMQUIRA_FACT_BACKEND, "sqlite"))?;
        let event_backend = match read_env_optional(MEMQUIRA_EVENT_BACKEND) {
            Some(value) => EventBackend::from_str(&value)?,
            None => EventBackend::default_for_fact_backend(&fact_backend),
        };
        let database_url = read_env_or(MEMQUIRA_DATABASE_URL, &defaults.database_url);
        let postgres_url = read_env_optional(MEMQUIRA_POSTGRES_URL);
        let database_migrations_enabled = parse_bool(
            MEMQUIRA_DATABASE_MIGRATIONS_ENABLED,
            defaults.database_migrations_enabled,
        )?;
        let database_migration_mode = DatabaseMigrationMode::from_str(&read_env_or(
            MEMQUIRA_DATABASE_MIGRATION_MODE,
            defaults.database_migration_mode.as_str(),
        ))?;
        let database_require_clean_migrations = parse_bool(
            MEMQUIRA_DATABASE_REQUIRE_CLEAN_MIGRATIONS,
            defaults.database_require_clean_migrations,
        )?;
        let backup_enabled = parse_bool(MEMQUIRA_BACKUP_ENABLED, defaults.backup_enabled)?;
        let backup_dir = read_env_or(MEMQUIRA_BACKUP_DIR, &defaults.backup_dir);
        let backup_max_files = parse_usize(MEMQUIRA_BACKUP_MAX_FILES, defaults.backup_max_files)?;
        let restore_enabled = parse_bool(MEMQUIRA_RESTORE_ENABLED, defaults.restore_enabled)?;
        let security_headers_enabled = parse_bool(
            MEMQUIRA_SECURITY_HEADERS_ENABLED,
            defaults.security_headers_enabled,
        )?;
        let max_request_body_bytes = parse_usize(
            MEMQUIRA_MAX_REQUEST_BODY_BYTES,
            defaults.max_request_body_bytes,
        )?;
        let cors_enabled = parse_bool(MEMQUIRA_CORS_ENABLED, defaults.cors_enabled)?;
        let cors_allowed_origins = parse_string_list(
            MEMQUIRA_CORS_ALLOWED_ORIGINS,
            &defaults.cors_allowed_origins,
        );
        let cors_allowed_methods = parse_string_list(
            MEMQUIRA_CORS_ALLOWED_METHODS,
            &defaults.cors_allowed_methods,
        );
        let cors_allowed_headers = parse_string_list(
            MEMQUIRA_CORS_ALLOWED_HEADERS,
            &defaults.cors_allowed_headers,
        );
        let cors_exposed_headers = parse_string_list(
            MEMQUIRA_CORS_EXPOSED_HEADERS,
            &defaults.cors_exposed_headers,
        );
        let cors_allow_credentials = parse_bool(
            MEMQUIRA_CORS_ALLOW_CREDENTIALS,
            defaults.cors_allow_credentials,
        )?;
        let qdrant_url = read_env_or(MEMQUIRA_QDRANT_URL, &defaults.qdrant_url);
        let qdrant_collection =
            read_env_or(MEMQUIRA_QDRANT_COLLECTION, &defaults.qdrant_collection);
        let lancedb_path = read_env_or(MEMQUIRA_LANCEDB_PATH, &defaults.lancedb_path);
        let lancedb_table = read_env_or(MEMQUIRA_LANCEDB_TABLE, &defaults.lancedb_table);
        let llm_provider = LlmProviderKind::from_str(&read_env_or(MEMQUIRA_LLM_PROVIDER, "mock"))?;
        let llm_model = read_env_or(MEMQUIRA_LLM_MODEL, &defaults.llm_model);
        let embedding_provider =
            EmbeddingProviderKind::from_str(&read_env_or(MEMQUIRA_EMBEDDING_PROVIDER, "mock"))?;
        let embedding_model = read_env_or(MEMQUIRA_EMBEDDING_MODEL, &defaults.embedding_model);
        let enable_pii_redaction =
            parse_bool(MEMQUIRA_ENABLE_PII_REDACTION, defaults.enable_pii_redaction)?;
        let structured_extraction_enabled = parse_bool(
            MEMQUIRA_STRUCTURED_EXTRACTION_ENABLED,
            defaults.structured_extraction_enabled,
        )?;
        let episodes_enabled = parse_bool(MEMQUIRA_EPISODES_ENABLED, defaults.episodes_enabled)?;
        let experimental_candidates_enabled = parse_bool(
            MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED,
            defaults.experimental_candidates_enabled,
        )?;
        let query_analysis_enabled = parse_bool(
            MEMQUIRA_QUERY_ANALYSIS_ENABLED,
            defaults.query_analysis_enabled,
        )?;
        let temporal_ranking_enabled = parse_bool(
            MEMQUIRA_TEMPORAL_RANKING_ENABLED,
            defaults.temporal_ranking_enabled,
        )?;
        let temporal_boost = match env_var(MEMQUIRA_TEMPORAL_BOOST) {
            Ok(value) => value.trim().parse::<f64>().map_err(|_| {
                MemquiraError::ValidationError(format!(
                    "{MEMQUIRA_TEMPORAL_BOOST} must be a valid floating-point number"
                ))
            })?,
            Err(_) => defaults.temporal_boost,
        };
        let multi_query = match read_env_or(MEMQUIRA_MULTI_QUERY, "none")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => MultiQuerySetting::None,
            "deterministic" => MultiQuerySetting::Deterministic,
            "llm" => {
                return Err(MemquiraError::ValidationError(format!(
                    "{MEMQUIRA_MULTI_QUERY}=llm is not available in this build"
                )));
            }
            other => {
                return Err(MemquiraError::ValidationError(format!(
                    "{MEMQUIRA_MULTI_QUERY} must be none or deterministic, got {other}"
                )));
            }
        };
        let rerank_provider = match read_env_or(MEMQUIRA_RERANK_PROVIDER, "none")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => RerankProviderSetting::None,
            "local" => RerankProviderSetting::Local,
            "bedrock" => RerankProviderSetting::Bedrock,
            other => {
                return Err(MemquiraError::ValidationError(format!(
                    "{MEMQUIRA_RERANK_PROVIDER} must be none, local or bedrock, got {other}"
                )));
            }
        };
        let rerank_candidates =
            parse_usize(MEMQUIRA_RERANK_CANDIDATES, defaults.rerank_candidates)?;
        let rerank_timeout_ms = parse_u64(MEMQUIRA_RERANK_TIMEOUT_MS, defaults.rerank_timeout_ms)?;
        let rerank_local_model_path = read_env_optional(MEMQUIRA_RERANK_LOCAL_MODEL_PATH);
        let rerank_local_model_sha256 = read_env_optional(MEMQUIRA_RERANK_LOCAL_MODEL_SHA256);
        let rerank_local_tokenizer_path = read_env_optional(MEMQUIRA_RERANK_LOCAL_TOKENIZER_PATH);
        let rerank_local_tokenizer_sha256 =
            read_env_optional(MEMQUIRA_RERANK_LOCAL_TOKENIZER_SHA256);
        let rerank_local_threads =
            parse_usize(MEMQUIRA_RERANK_LOCAL_THREADS, defaults.rerank_local_threads)?;
        let rerank_local_batch_size = parse_usize(
            MEMQUIRA_RERANK_LOCAL_BATCH_SIZE,
            defaults.rerank_local_batch_size,
        )?;
        let rerank_bedrock_region = read_env_or(
            MEMQUIRA_RERANK_BEDROCK_REGION,
            &defaults.rerank_bedrock_region,
        )
        .trim()
        .to_string();
        let rerank_bedrock_model_arn = read_env_optional(MEMQUIRA_RERANK_BEDROCK_MODEL_ARN)
            .map(|arn| arn.trim().to_string())
            .unwrap_or_else(|| amazon_rerank_model_arn(&rerank_bedrock_region));
        let rerank_bedrock_profile = read_env_optional(MEMQUIRA_RERANK_BEDROCK_PROFILE);
        let query_decomposition = match read_env_or(MEMQUIRA_QUERY_DECOMPOSITION, "none")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => QueryDecompositionSetting::None,
            "noop" => QueryDecompositionSetting::Noop,
            "llm" => QueryDecompositionSetting::Llm,
            other => {
                return Err(MemquiraError::ValidationError(format!(
                    "{MEMQUIRA_QUERY_DECOMPOSITION} must be none, noop or llm, got {other}"
                )));
            }
        };
        let query_decomposition_timeout_ms = parse_u64(
            MEMQUIRA_QUERY_DECOMPOSITION_TIMEOUT_MS,
            defaults.query_decomposition_timeout_ms,
        )?;
        let qdrant_episode_collection = read_env_or(
            MEMQUIRA_QDRANT_EPISODE_COLLECTION,
            &format!("{qdrant_collection}_episodes"),
        );
        let qdrant_ensure_indexes = parse_bool(
            MEMQUIRA_QDRANT_ENSURE_INDEXES,
            defaults.qdrant_ensure_indexes,
        )?;
        let qdrant_write_mode = match env_var(MEMQUIRA_QDRANT_WRITE_MODE) {
            Ok(value) if !value.trim().is_empty() => QdrantWriteModeSetting::from_str(&value)?,
            _ => defaults.qdrant_write_mode,
        };
        let qdrant_search_hnsw_ef = match read_env_optional(MEMQUIRA_QDRANT_SEARCH_HNSW_EF) {
            None => defaults.qdrant_search_hnsw_ef,
            Some(value) => match value.trim().parse::<u64>() {
                Ok(ef) if (1..=MAX_QDRANT_SEARCH_HNSW_EF).contains(&ef) => Some(ef),
                _ => {
                    return Err(MemquiraError::ValidationError(format!(
                        "Invalid MEMQUIRA_QDRANT_SEARCH_HNSW_EF value: {value} (expected an integer \
                         1..={MAX_QDRANT_SEARCH_HNSW_EF}, or unset for Qdrant's default)"
                    )));
                }
            },
        };
        let qdrant_exact_search_max_tenant_points =
            match read_env_optional(MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS) {
                None => defaults.qdrant_exact_search_max_tenant_points,
                Some(value) => match value.trim().parse::<u64>() {
                    Ok(n) if (1..=MAX_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS).contains(&n) => {
                        Some(n)
                    }
                    _ => {
                        return Err(MemquiraError::ValidationError(format!(
                            "Invalid MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS value: {value} \
                             (expected an integer 1..={MAX_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS}, \
                             or unset for the historical approximate search)"
                        )));
                    }
                },
            };
        let structured_extraction_policy = StructuredExtractionPolicy::from_str(&read_env_or(
            MEMQUIRA_STRUCTURED_EXTRACTION_POLICY,
            "retentive",
        ))?;
        let consolidation_mode =
            ConsolidationMode::from_str(&read_env_or(MEMQUIRA_CONSOLIDATION_MODE, "legacy"))?;
        let related_fact_matcher =
            RelatedFactMatcher::from_str(&read_env_or(MEMQUIRA_RELATED_FACT_MATCHER, "legacy"))?;
        let classification_prompt = ClassificationPromptSetting::from_str(&read_env_or(
            MEMQUIRA_CLASSIFICATION_PROMPT,
            "legacy",
        ))?;
        let importance_policy =
            ImportancePolicySetting::from_str(&read_env_or(MEMQUIRA_IMPORTANCE_POLICY, "legacy"))?;
        let temporal_rewrite =
            TemporalRewriteSetting::from_str(&read_env_or(MEMQUIRA_TEMPORAL_REWRITE, "legacy"))?;
        let unknown_memory_type =
            UnknownMemoryTypeSetting::from_str(&read_env_or(MEMQUIRA_UNKNOWN_MEMORY_TYPE, "skip"))?;
        let event_update_policy = EventUpdatePolicySetting::from_str(&read_env_or(
            MEMQUIRA_EVENT_UPDATE_POLICY,
            "history",
        ))?;
        let dedup_value_guard =
            DedupValueGuardSetting::from_str(&read_env_or(MEMQUIRA_DEDUP_VALUE_GUARD, "off"))?;
        let related_fact_similarity_floor = parse_f32(
            MEMQUIRA_RELATED_FACT_SIMILARITY_FLOOR,
            defaults.related_fact_similarity_floor,
        )?;
        if !(0.0..=1.0).contains(&related_fact_similarity_floor) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_RELATED_FACT_SIMILARITY_FLOOR must be between 0.0 and 1.0".to_string(),
            ));
        }
        let context_default_packing =
            ContextPacking::from_str(&read_env_or(MEMQUIRA_CONTEXT_DEFAULT_PACKING, "prefix"))?;
        let consolidation_candidates = parse_usize(
            MEMQUIRA_CONSOLIDATION_CANDIDATES,
            defaults.consolidation_candidates,
        )?;
        if !(1..=50).contains(&consolidation_candidates) {
            return Err(MemquiraError::ValidationError(format!(
                "MEMQUIRA_CONSOLIDATION_CANDIDATES must be between 1 and 50, got {consolidation_candidates}"
            )));
        }
        let consolidation_similarity_floor = parse_f32(
            MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR,
            defaults.consolidation_similarity_floor,
        )?;
        if !(0.0..=1.0).contains(&consolidation_similarity_floor) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR must be between 0.0 and 1.0".to_string(),
            ));
        }
        let entities_enabled = parse_bool(MEMQUIRA_ENTITIES_ENABLED, defaults.entities_enabled)?;
        let entity_name_similarity_threshold = parse_f32(
            MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD,
            defaults.entity_name_similarity_threshold,
        )?;
        if !(0.0..=1.0).contains(&entity_name_similarity_threshold) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD must be between 0.0 and 1.0".to_string(),
            ));
        }
        let entity_llm_confirmation_enabled = parse_bool(
            MEMQUIRA_ENTITY_LLM_CONFIRMATION_ENABLED,
            defaults.entity_llm_confirmation_enabled,
        )?;
        let search_strategy =
            SearchStrategy::from_str(&read_env_or(MEMQUIRA_SEARCH_STRATEGY, "hybrid"))?;
        let rrf_k = parse_f32(MEMQUIRA_RRF_K, defaults.rrf_k)?;
        let search_max_limit = parse_usize(MEMQUIRA_SEARCH_MAX_LIMIT, defaults.search_max_limit)?;
        let export_max_records =
            parse_usize(MEMQUIRA_EXPORT_MAX_RECORDS, defaults.export_max_records)?;
        let query_embedding_cache_capacity = parse_usize(
            MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY,
            defaults.query_embedding_cache_capacity,
        )?;
        let classification_skip_when_no_related = parse_bool(
            MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED,
            defaults.classification_skip_when_no_related,
        )?;
        let ingest_journal_enabled = parse_bool(
            MEMQUIRA_INGEST_JOURNAL_ENABLED,
            defaults.ingest_journal_enabled,
        )?;
        let ingest_recovery_stale_secs = parse_u64(
            MEMQUIRA_INGEST_RECOVERY_STALE_SECS,
            defaults.ingest_recovery_stale_secs,
        )?;
        let ingest_recovery_interval_secs = parse_u64(
            MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS,
            defaults.ingest_recovery_interval_secs,
        )?;
        let ingest_inflight_wait_ms = parse_u64(
            MEMQUIRA_INGEST_INFLIGHT_WAIT_MS,
            defaults.ingest_inflight_wait_ms,
        )?;
        let ingest_receipt_ttl_secs = parse_u64(
            MEMQUIRA_INGEST_RECEIPT_TTL_SECS,
            defaults.ingest_receipt_ttl_secs,
        )?;
        let local_embedding_model_dir = read_env_optional(MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR);
        let local_embedding_queue_capacity = parse_usize(
            MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY,
            defaults.local_embedding_queue_capacity,
        )?;
        let rrf_weight_semantic =
            parse_f32(MEMQUIRA_RRF_WEIGHT_SEMANTIC, defaults.rrf_weight_semantic)?;
        let rrf_weight_keyword =
            parse_f32(MEMQUIRA_RRF_WEIGHT_KEYWORD, defaults.rrf_weight_keyword)?;
        let extraction_window_turns = parse_usize(
            MEMQUIRA_EXTRACTION_WINDOW_TURNS,
            defaults.extraction_window_turns,
        )?;
        let extraction_window_overlap = parse_usize(
            MEMQUIRA_EXTRACTION_WINDOW_OVERLAP,
            defaults.extraction_window_overlap,
        )?;
        let extraction_concurrency = parse_usize(
            MEMQUIRA_EXTRACTION_CONCURRENCY,
            defaults.extraction_concurrency,
        )?;
        let min_importance = parse_f32(MEMQUIRA_MIN_IMPORTANCE, defaults.min_importance)?;
        let auth_enabled = parse_bool(MEMQUIRA_AUTH_ENABLED, defaults.auth_enabled)?;
        let auth_mode = AuthMode::from_str(&read_env_or(MEMQUIRA_AUTH_MODE, "dev"))?;
        let dev_api_key = read_env_or(MEMQUIRA_DEV_API_KEY, &defaults.dev_api_key);
        let api_key_pepper = read_env_optional(MEMQUIRA_API_KEY_PEPPER);
        let openai_api_key = read_env_optional(OPENAI_API_KEY);
        let openai_base_url = read_env_or(OPENAI_BASE_URL, &defaults.openai_base_url);
        let rate_limit_enabled =
            parse_bool(MEMQUIRA_RATE_LIMIT_ENABLED, defaults.rate_limit_enabled)?;
        let rate_limit_requests_per_minute = parse_u32(
            MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE,
            defaults.rate_limit_requests_per_minute,
        )?;
        let admission = parse_admission(&defaults.admission)?;
        let providers = parse_provider_roles(&defaults.providers)?;
        let log_format = LogFormat::from_str(&read_env_or(MEMQUIRA_LOG_FORMAT, "json"))?;
        let log_level = LogLevel::from_str(&read_env_or(MEMQUIRA_LOG_LEVEL, "info"))?;
        let request_id_header =
            read_env_or(MEMQUIRA_REQUEST_ID_HEADER, &defaults.request_id_header);
        let metrics_enabled = parse_bool(MEMQUIRA_METRICS_ENABLED, defaults.metrics_enabled)?;
        let metrics_path = read_env_or(MEMQUIRA_METRICS_PATH, &defaults.metrics_path);
        let metrics_require_auth =
            parse_bool(MEMQUIRA_METRICS_REQUIRE_AUTH, defaults.metrics_require_auth)?;
        let metrics_include_process = parse_bool(
            MEMQUIRA_METRICS_INCLUDE_PROCESS,
            defaults.metrics_include_process,
        )?;
        let retention_enabled = parse_bool(MEMQUIRA_RETENTION_ENABLED, defaults.retention_enabled)?;
        let fact_retention_days =
            parse_u32(MEMQUIRA_FACT_RETENTION_DAYS, defaults.fact_retention_days)?;
        let event_retention_days =
            parse_u32(MEMQUIRA_EVENT_RETENTION_DAYS, defaults.event_retention_days)?;
        // The legacy flag is still parsed (a malformed value fails); when both are set they must
        // agree (no silent precedence between contradictory settings).
        let legacy_cache_enabled = read_env_optional(MEMQUIRA_CONTEXT_CACHE_ENABLED)
            .map(|_| {
                parse_bool(
                    MEMQUIRA_CONTEXT_CACHE_ENABLED,
                    defaults.context_cache_enabled,
                )
            })
            .transpose()?;
        let context_cache_backend = match read_env_optional(MEMQUIRA_CONTEXT_CACHE_BACKEND) {
            Some(value) => {
                let backend = ContextCacheBackend::from_str(&value)?;
                if let Some(legacy) = legacy_cache_enabled
                    && legacy != (backend != ContextCacheBackend::Disabled)
                {
                    return Err(MemquiraError::ValidationError(format!(
                        "{MEMQUIRA_CONTEXT_CACHE_ENABLED} contradicts {MEMQUIRA_CONTEXT_CACHE_BACKEND}; \
                         remove {MEMQUIRA_CONTEXT_CACHE_ENABLED} or make them agree \
                         (the safe setting is {MEMQUIRA_CONTEXT_CACHE_BACKEND}=disabled)"
                    )));
                }
                backend
            }
            None if legacy_cache_enabled == Some(true) => ContextCacheBackend::Memory,
            None => ContextCacheBackend::Disabled,
        };
        // An unset MEMQUIRA_ENV defaults to development, which is not evidence of a local
        // single-process deployment: the memory cache needs it stated explicitly.
        if context_cache_backend == ContextCacheBackend::Memory
            && read_env_optional(MEMQUIRA_ENV).is_none()
        {
            return Err(MemquiraError::ValidationError(format!(
                "context cache refused: {MEMQUIRA_CONTEXT_CACHE_BACKEND}=memory needs {MEMQUIRA_ENV} \
                 set explicitly to development (local single-process development only); \
                 otherwise set {MEMQUIRA_CONTEXT_CACHE_BACKEND}=disabled"
            )));
        }
        let context_cache_enabled = context_cache_backend != ContextCacheBackend::Disabled;
        let context_cache_local_single_process = parse_bool(
            MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS,
            defaults.context_cache_local_single_process,
        )?;
        let context_cache_ttl_seconds = parse_u64(
            MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS,
            defaults.context_cache_ttl_seconds,
        )?;
        let context_cache_max_entries = parse_usize(
            MEMQUIRA_CONTEXT_CACHE_MAX_ENTRIES,
            defaults.context_cache_max_entries,
        )?;
        let redis_url = read_env_optional(MEMQUIRA_REDIS_URL);
        let context_cache_key_prefix = read_env_or(
            MEMQUIRA_CONTEXT_CACHE_KEY_PREFIX,
            &defaults.context_cache_key_prefix,
        );
        let context_cache_stampede_protection_enabled = parse_bool(
            MEMQUIRA_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED,
            defaults.context_cache_stampede_protection_enabled,
        )?;
        let context_cache_lock_timeout_seconds = parse_u64(
            MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS,
            defaults.context_cache_lock_timeout_seconds,
        )?;
        let context_cache_stale_while_revalidate_enabled = parse_bool(
            MEMQUIRA_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED,
            defaults.context_cache_stale_while_revalidate_enabled,
        )?;
        let context_cache_stale_ttl_seconds = parse_u64(
            MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS,
            defaults.context_cache_stale_ttl_seconds,
        )?;
        let context_cache_metrics_enabled = parse_bool(
            MEMQUIRA_CONTEXT_CACHE_METRICS_ENABLED,
            defaults.context_cache_metrics_enabled,
        )?;
        let provider_timeout_seconds = parse_u64(
            MEMQUIRA_PROVIDER_TIMEOUT_SECONDS,
            defaults.provider_timeout_seconds,
        )?;
        let provider_max_retries =
            parse_usize(MEMQUIRA_PROVIDER_MAX_RETRIES, defaults.provider_max_retries)?;
        let provider_initial_backoff_ms = parse_u64(
            MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS,
            defaults.provider_initial_backoff_ms,
        )?;
        let provider_max_backoff_ms = parse_u64(
            MEMQUIRA_PROVIDER_MAX_BACKOFF_MS,
            defaults.provider_max_backoff_ms,
        )?;
        let provider_backoff_multiplier = parse_f32(
            MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER,
            defaults.provider_backoff_multiplier,
        )?;
        let provider_retry_jitter_enabled = parse_bool(
            MEMQUIRA_PROVIDER_RETRY_JITTER_ENABLED,
            defaults.provider_retry_jitter_enabled,
        )?;
        let provider_fallback_enabled = parse_bool(
            MEMQUIRA_PROVIDER_FALLBACK_ENABLED,
            defaults.provider_fallback_enabled,
        )?;
        let llm_fallback_order = parse_provider_fallback_order(
            MEMQUIRA_LLM_FALLBACK_ORDER,
            &defaults.llm_fallback_order,
        )?;
        let embedding_fallback_order = parse_provider_fallback_order(
            MEMQUIRA_EMBEDDING_FALLBACK_ORDER,
            &defaults.embedding_fallback_order,
        )?;
        let summarizer_fallback_order = parse_provider_fallback_order(
            MEMQUIRA_SUMMARIZER_FALLBACK_ORDER,
            &defaults.summarizer_fallback_order,
        )?;
        let provider_circuit_breaker_enabled = parse_bool(
            MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_ENABLED,
            defaults.provider_circuit_breaker_enabled,
        )?;
        let provider_circuit_breaker_failure_threshold = parse_usize(
            MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD,
            defaults.provider_circuit_breaker_failure_threshold,
        )?;
        let provider_circuit_breaker_reset_timeout_seconds = parse_u64(
            MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS,
            defaults.provider_circuit_breaker_reset_timeout_seconds,
        )?;
        let provider_circuit_breaker_half_open_max_calls = parse_usize(
            MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS,
            defaults.provider_circuit_breaker_half_open_max_calls,
        )?;
        let provider_usage_metrics_enabled = parse_bool(
            MEMQUIRA_PROVIDER_USAGE_METRICS_ENABLED,
            defaults.provider_usage_metrics_enabled,
        )?;
        let provider_cost_tracking_enabled = parse_bool(
            MEMQUIRA_PROVIDER_COST_TRACKING_ENABLED,
            defaults.provider_cost_tracking_enabled,
        )?;
        let provider_usage_persistence_enabled = parse_bool(
            MEMQUIRA_PROVIDER_USAGE_PERSISTENCE_ENABLED,
            defaults.provider_usage_persistence_enabled,
        )?;
        let provider_operation_header_enabled = parse_bool(
            MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED,
            defaults.provider_operation_header_enabled,
        )?;
        let provider_operation_header_hosts = parse_string_list(
            MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS,
            &defaults.provider_operation_header_hosts,
        );
        let provider_usage_retention_days = parse_u32(
            MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS,
            defaults.provider_usage_retention_days,
        )?;
        let quotas_enabled = parse_bool(MEMQUIRA_QUOTAS_ENABLED, defaults.quotas_enabled)?;
        let max_users_per_org = parse_u64(MEMQUIRA_MAX_USERS_PER_ORG, defaults.max_users_per_org)?;
        let max_memories_per_user = parse_u64(
            MEMQUIRA_MAX_MEMORIES_PER_USER,
            defaults.max_memories_per_user,
        )?;
        let max_memories_per_org =
            parse_u64(MEMQUIRA_MAX_MEMORIES_PER_ORG, defaults.max_memories_per_org)?;
        let daily_provider_request_limit = parse_u64(
            MEMQUIRA_DAILY_PROVIDER_REQUEST_LIMIT,
            defaults.daily_provider_request_limit,
        )?;
        let daily_provider_token_limit = parse_u64(
            MEMQUIRA_DAILY_PROVIDER_TOKEN_LIMIT,
            defaults.daily_provider_token_limit,
        )?;
        let provider_guardrails_enabled = parse_bool(
            MEMQUIRA_PROVIDER_GUARDRAILS_ENABLED,
            defaults.provider_guardrails_enabled,
        )?;
        let real_provider_calls_enabled = parse_bool(
            MEMQUIRA_REAL_PROVIDER_CALLS_ENABLED,
            defaults.real_provider_calls_enabled,
        )?;
        let provider_test_mode = ProviderTestMode::from_str(&read_env_or(
            MEMQUIRA_PROVIDER_TEST_MODE,
            defaults.provider_test_mode.as_str(),
        ))?;
        let provider_max_calls_per_run = parse_usize(
            MEMQUIRA_PROVIDER_MAX_CALLS_PER_RUN,
            defaults.provider_max_calls_per_run,
        )?;
        let provider_max_input_chars = parse_usize(
            MEMQUIRA_PROVIDER_MAX_INPUT_CHARS,
            defaults.provider_max_input_chars,
        )?;
        let provider_max_output_tokens = parse_usize(
            MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS,
            defaults.provider_max_output_tokens,
        )?;
        let extraction_max_output_tokens =
            parse_optional_usize(MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS)?;
        let classification_max_output_tokens =
            parse_optional_usize(MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS)?;
        let provider_max_retries_per_call = parse_usize(
            MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL,
            defaults.provider_max_retries_per_call,
        )?;
        let allow_real_providers_during_load_tests = parse_bool(
            MEMQUIRA_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS,
            defaults.allow_real_providers_during_load_tests,
        )?;
        let background_jobs_allow_real_providers = parse_bool(
            MEMQUIRA_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS,
            defaults.background_jobs_allow_real_providers,
        )?;
        let multi_provider_validation_enabled = parse_bool(
            MEMQUIRA_MULTI_PROVIDER_VALIDATION_ENABLED,
            defaults.multi_provider_validation_enabled,
        )?;
        let multi_provider_validation_confirmation =
            read_env_optional(MEMQUIRA_MULTI_PROVIDER_VALIDATION_CONFIRMATION);
        let background_jobs_enabled = parse_bool(
            MEMQUIRA_BACKGROUND_JOBS_ENABLED,
            defaults.background_jobs_enabled,
        )?;
        let background_job_runner_interval_seconds = parse_u64(
            MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS,
            defaults.background_job_runner_interval_seconds,
        )?;
        let background_job_org_ids = parse_string_list(
            MEMQUIRA_BACKGROUND_JOB_ORG_IDS,
            &defaults.background_job_org_ids,
        );
        let memory_usage_snapshot_job_enabled = parse_bool(
            MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED,
            defaults.memory_usage_snapshot_job_enabled,
        )?;
        let memory_usage_snapshot_job_interval_seconds = parse_u64(
            MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS,
            defaults.memory_usage_snapshot_job_interval_seconds,
        )?;
        let provider_usage_retention_job_enabled = parse_bool(
            MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_ENABLED,
            defaults.provider_usage_retention_job_enabled,
        )?;
        let provider_usage_retention_job_interval_seconds = parse_u64(
            MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS,
            defaults.provider_usage_retention_job_interval_seconds,
        )?;
        let memory_retention_job_enabled = parse_bool(
            MEMQUIRA_MEMORY_RETENTION_JOB_ENABLED,
            defaults.memory_retention_job_enabled,
        )?;
        let memory_retention_job_interval_seconds = parse_u64(
            MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS,
            defaults.memory_retention_job_interval_seconds,
        )?;
        let background_job_history_enabled = parse_bool(
            MEMQUIRA_BACKGROUND_JOB_HISTORY_ENABLED,
            defaults.background_job_history_enabled,
        )?;
        let background_job_history_retention_days = parse_u32(
            MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS,
            defaults.background_job_history_retention_days,
        )?;
        let background_job_lock_enabled = parse_bool(
            MEMQUIRA_BACKGROUND_JOB_LOCK_ENABLED,
            defaults.background_job_lock_enabled,
        )?;
        let background_job_lock_backend = BackgroundJobLockBackend::from_str(&read_env_or(
            MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND,
            "database",
        ))?;
        let background_job_lock_ttl_seconds = parse_u64(
            MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS,
            defaults.background_job_lock_ttl_seconds,
        )?;
        let background_job_lock_owner_id = read_env_optional(MEMQUIRA_BACKGROUND_JOB_LOCK_OWNER_ID)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let background_job_retries_enabled = parse_bool(
            MEMQUIRA_BACKGROUND_JOB_RETRIES_ENABLED,
            defaults.background_job_retries_enabled,
        )?;
        let background_job_max_retries = parse_usize(
            MEMQUIRA_BACKGROUND_JOB_MAX_RETRIES,
            defaults.background_job_max_retries,
        )?;
        let background_job_initial_backoff_ms = parse_u64(
            MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS,
            defaults.background_job_initial_backoff_ms,
        )?;
        let background_job_max_backoff_ms = parse_u64(
            MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS,
            defaults.background_job_max_backoff_ms,
        )?;
        let background_job_backoff_multiplier = parse_f32(
            MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER,
            defaults.background_job_backoff_multiplier,
        )?;
        let background_job_retry_jitter_enabled = parse_bool(
            MEMQUIRA_BACKGROUND_JOB_RETRY_JITTER_ENABLED,
            defaults.background_job_retry_jitter_enabled,
        )?;
        let graceful_shutdown_timeout_seconds = parse_u64(
            MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS,
            defaults.graceful_shutdown_timeout_seconds,
        )?;
        let allow_mock = parse_bool(MEMQUIRA_ALLOW_MOCK, defaults.allow_mock)?;
        let startup_dependency_timeout_seconds = parse_u64(
            MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS,
            defaults.startup_dependency_timeout_seconds,
        )?;
        let background_job_shutdown_timeout_seconds = parse_u64(
            MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS,
            defaults.background_job_shutdown_timeout_seconds,
        )?;

        if !(0.0..=1.0).contains(&min_importance) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_MIN_IMPORTANCE must be between 0.0 and 1.0".to_string(),
            ));
        }

        let mut settings = Self {
            environment,
            host,
            port,
            storage_mode,
            vector_backend,
            fact_backend,
            event_backend,
            database_url,
            postgres_url,
            database_migrations_enabled,
            database_migration_mode,
            database_require_clean_migrations,
            backup_enabled,
            backup_dir,
            backup_max_files,
            restore_enabled,
            security_headers_enabled,
            max_request_body_bytes,
            cors_enabled,
            cors_allowed_origins,
            cors_allowed_methods,
            cors_allowed_headers,
            cors_exposed_headers,
            cors_allow_credentials,
            qdrant_url,
            qdrant_collection,
            lancedb_path,
            lancedb_table,
            llm_provider,
            llm_model,
            embedding_provider,
            embedding_model,
            enable_pii_redaction,
            structured_extraction_enabled,
            episodes_enabled,
            experimental_candidates_enabled,
            query_analysis_enabled,
            temporal_ranking_enabled,
            temporal_boost,
            multi_query,
            rerank_provider,
            rerank_candidates,
            rerank_timeout_ms,
            rerank_local_model_path,
            rerank_local_model_sha256,
            rerank_local_tokenizer_path,
            rerank_local_tokenizer_sha256,
            rerank_local_threads,
            rerank_local_batch_size,
            rerank_bedrock_region,
            rerank_bedrock_model_arn,
            rerank_bedrock_profile,
            query_decomposition,
            query_decomposition_timeout_ms,
            qdrant_episode_collection,
            qdrant_ensure_indexes,
            qdrant_write_mode,
            qdrant_search_hnsw_ef,
            qdrant_exact_search_max_tenant_points,
            structured_extraction_policy,
            consolidation_mode,
            related_fact_matcher,
            temporal_rewrite,
            unknown_memory_type,
            event_update_policy,
            dedup_value_guard,
            related_fact_similarity_floor,
            importance_policy,
            classification_prompt,
            context_default_packing,
            consolidation_candidates,
            consolidation_similarity_floor,
            entities_enabled,
            entity_name_similarity_threshold,
            entity_llm_confirmation_enabled,
            search_strategy,
            rrf_k,
            search_max_limit,
            query_embedding_cache_capacity,
            classification_skip_when_no_related,
            ingest_journal_enabled,
            ingest_recovery_stale_secs,
            ingest_recovery_interval_secs,
            ingest_inflight_wait_ms,
            ingest_receipt_ttl_secs,
            export_max_records,
            local_embedding_model_dir,
            local_embedding_queue_capacity,
            rrf_weight_semantic,
            rrf_weight_keyword,
            extraction_window_turns,
            extraction_window_overlap,
            extraction_concurrency,
            min_importance,
            auth_enabled,
            auth_mode,
            dev_api_key,
            api_key_pepper,
            openai_api_key,
            openai_base_url,
            rate_limit_enabled,
            rate_limit_requests_per_minute,
            admission,
            providers,
            log_format,
            log_level,
            request_id_header,
            metrics_enabled,
            metrics_path,
            metrics_require_auth,
            metrics_include_process,
            retention_enabled,
            fact_retention_days,
            event_retention_days,
            context_cache_enabled,
            context_cache_backend,
            context_cache_ttl_seconds,
            context_cache_max_entries,
            redis_url,
            context_cache_key_prefix,
            context_cache_stampede_protection_enabled,
            context_cache_lock_timeout_seconds,
            context_cache_stale_while_revalidate_enabled,
            context_cache_stale_ttl_seconds,
            context_cache_metrics_enabled,
            context_cache_local_single_process,
            provider_timeout_seconds,
            provider_max_retries,
            provider_initial_backoff_ms,
            provider_max_backoff_ms,
            provider_backoff_multiplier,
            provider_retry_jitter_enabled,
            provider_fallback_enabled,
            llm_fallback_order,
            embedding_fallback_order,
            summarizer_fallback_order,
            provider_circuit_breaker_enabled,
            provider_circuit_breaker_failure_threshold,
            provider_circuit_breaker_reset_timeout_seconds,
            provider_circuit_breaker_half_open_max_calls,
            provider_usage_metrics_enabled,
            provider_cost_tracking_enabled,
            provider_usage_persistence_enabled,
            provider_operation_header_enabled,
            provider_operation_header_hosts,
            provider_usage_retention_days,
            quotas_enabled,
            max_users_per_org,
            max_memories_per_user,
            max_memories_per_org,
            daily_provider_request_limit,
            daily_provider_token_limit,
            provider_guardrails_enabled,
            real_provider_calls_enabled,
            provider_test_mode,
            provider_max_calls_per_run,
            provider_max_input_chars,
            provider_max_output_tokens,
            extraction_max_output_tokens,
            classification_max_output_tokens,
            provider_max_retries_per_call,
            allow_real_providers_during_load_tests,
            background_jobs_allow_real_providers,
            multi_provider_validation_enabled,
            multi_provider_validation_confirmation,
            background_jobs_enabled,
            background_job_runner_interval_seconds,
            background_job_org_ids,
            memory_usage_snapshot_job_enabled,
            memory_usage_snapshot_job_interval_seconds,
            provider_usage_retention_job_enabled,
            provider_usage_retention_job_interval_seconds,
            memory_retention_job_enabled,
            memory_retention_job_interval_seconds,
            background_job_history_enabled,
            background_job_history_retention_days,
            background_job_lock_enabled,
            background_job_lock_backend,
            background_job_lock_ttl_seconds,
            background_job_lock_owner_id,
            background_job_retries_enabled,
            background_job_max_retries,
            background_job_initial_backoff_ms,
            background_job_max_backoff_ms,
            background_job_backoff_multiplier,
            background_job_retry_jitter_enabled,
            graceful_shutdown_timeout_seconds,
            allow_mock,
            startup_dependency_timeout_seconds,
            background_job_shutdown_timeout_seconds,
        };
        // Without an explicit stale window, keep it clear of the write deadline (a
        // profile for slow local models raises the deadline well past the 300 s default).
        if read_env_optional(MEMQUIRA_INGEST_RECOVERY_STALE_SECS).is_none() {
            settings.ingest_recovery_stale_secs =
                default_ingest_recovery_stale_secs(&settings.admission);
        }

        settings.validate()?;
        Ok(settings)
    }

    /// In-memory SQLite settings for integration tests.
    pub fn sqlite_memory() -> Self {
        Self {
            fact_backend: FactBackend::Sqlite,
            event_backend: EventBackend::Sqlite,
            database_url: "sqlite::memory:?cache=shared".to_string(),
            ..Self::default()
        }
    }

    /// LanceDB vector store with mock facts (API integration tests).
    pub fn lancedb_with_path(lancedb_path: impl Into<String>) -> Self {
        Self {
            vector_backend: VectorBackend::LanceDb,
            lancedb_path: lancedb_path.into(),
            ..Self::default()
        }
    }

    /// Qdrant vector store with mock facts (API integration tests).
    pub fn qdrant_with_url(qdrant_url: impl Into<String>) -> Self {
        Self {
            vector_backend: VectorBackend::Qdrant,
            qdrant_url: qdrant_url.into(),
            ..Self::default()
        }
    }

    /// Qdrant vector store with explicit collection name (API integration tests).
    /// Every way this configuration differs from the validated reference contract.
    /// Empty = the reference configuration. Setting names only, never values.
    pub fn reference_contract_mismatches(&self) -> Vec<&'static str> {
        let e = &self.providers.embedding_options;
        let mut out = Vec::new();
        let mut need = |ok: bool, name: &'static str| {
            if !ok {
                out.push(name);
            }
        };
        need(self.llm_provider == LlmProviderKind::OpenAi, "llm_provider");
        need(self.llm_model == REFERENCE_LLM_MODEL, "llm_model");
        need(
            self.embedding_provider == EmbeddingProviderKind::OpenAi,
            "embedding_provider",
        );
        need(
            self.embedding_model == REFERENCE_EMBEDDING_MODEL,
            "embedding_model",
        );
        need(
            e.dimensions.unwrap_or(REFERENCE_EMBEDDING_DIMENSIONS)
                == REFERENCE_EMBEDDING_DIMENSIONS,
            "embedding_dimensions",
        );
        need(e.send_dimensions, "embedding_send_dimensions");
        need(!e.normalize, "embedding_normalize");
        need(
            e.query_prefix.is_none() && e.document_prefix.is_none(),
            "embedding_prefixes",
        );
        need(
            e.model_revision.is_none() && e.serving_variant.is_none(),
            "embedding_revision",
        );
        need(e.dedup_threshold.is_none(), "embedding_dedup_threshold");
        need(
            self.providers.llm_json_mode == LlmJsonMode::JsonObject,
            "llm_json_mode",
        );
        need(!self.provider_fallback_enabled, "provider_fallback_enabled");
        need(
            self.structured_extraction_enabled,
            "structured_extraction_enabled",
        );
        need(
            self.structured_extraction_policy == StructuredExtractionPolicy::Retentive,
            "structured_extraction_policy",
        );
        need(
            self.extraction_max_output_tokens.is_none(),
            "extraction_max_output_tokens",
        );
        need(
            self.extraction_window_turns == 20 && self.extraction_window_overlap == 2,
            "extraction_window",
        );
        need(
            (self.min_importance - 0.55).abs() < f32::EPSILON,
            "min_importance",
        );
        need(self.enable_pii_redaction, "pii_redaction");
        need(self.episodes_enabled, "episodes_enabled");
        // The reference searches with exactly the beam of the
        // maintainers' acceptance (256). Unset (the earlier request) and any other beam are
        // reference mismatches (`compatible-unvalidated`).
        need(
            self.qdrant_search_hnsw_ef == Some(REFERENCE_QDRANT_SEARCH_HNSW_EF),
            "qdrant_search_hnsw_ef",
        );
        // Experimental; the launch reference is measured without
        // it, so any explicit value is a reference mismatch (`compatible-unvalidated`).
        need(
            self.qdrant_exact_search_max_tenant_points.is_none(),
            "qdrant_exact_search_max_tenant_points",
        );
        need(self.search_max_limit == 100, "search_max_limit");
        need(
            self.search_strategy == SearchStrategy::Hybrid,
            "search_strategy",
        );
        need(
            (self.rrf_k - 60.0).abs() < f32::EPSILON
                && (self.rrf_weight_semantic - 1.0).abs() < f32::EPSILON
                && (self.rrf_weight_keyword - 1.0).abs() < f32::EPSILON,
            "rrf",
        );
        // optional features: all off in the reference.
        need(
            self.consolidation_mode == ConsolidationMode::Legacy,
            "consolidation_mode",
        );
        need(
            self.related_fact_matcher == RelatedFactMatcher::Legacy,
            "related_fact_matcher",
        );
        need(
            self.classification_prompt == ClassificationPromptSetting::Legacy,
            "classification_prompt",
        );
        need(
            self.importance_policy == ImportancePolicySetting::Legacy,
            "importance_policy",
        );
        need(
            self.temporal_rewrite == TemporalRewriteSetting::Legacy,
            "temporal_rewrite",
        );
        need(
            self.unknown_memory_type == UnknownMemoryTypeSetting::Skip,
            "unknown_memory_type",
        );
        need(
            self.event_update_policy == EventUpdatePolicySetting::History,
            "event_update_policy",
        );
        need(
            self.dedup_value_guard == DedupValueGuardSetting::Off,
            "dedup_value_guard",
        );
        need(
            self.related_fact_similarity_floor == 0.5,
            "related_fact_similarity_floor",
        );
        // The reference contexts were measured with prefix (the default again since 2026-10-28);
        // fill and evidence are not answer-validated (evidence: answer-worse in the reference evaluation).
        need(
            self.context_default_packing == ContextPacking::Prefix,
            "context_default_packing",
        );
        need(!self.entities_enabled, "entities_enabled");
        need(!self.query_analysis_enabled, "query_analysis_enabled");
        need(!self.temporal_ranking_enabled, "temporal_ranking_enabled");
        need(self.multi_query == MultiQuerySetting::None, "multi_query");
        need(
            self.rerank_provider == RerankProviderSetting::None,
            "rerank_provider",
        );
        need(
            self.query_decomposition == QueryDecompositionSetting::None,
            "query_decomposition",
        );
        need(!self.context_cache_enabled, "context_cache_enabled");
        out
    }

    /// The tier this configuration may claim.
    pub fn quality_tier(&self) -> QualityTier {
        if self.llm_provider == LlmProviderKind::Mock
            || self.embedding_provider == EmbeddingProviderKind::Mock
        {
            QualityTier::DevelopmentMock
        } else if self.reference_contract_mismatches().is_empty() {
            QualityTier::ReferenceValidated
        } else {
            QualityTier::CompatibleUnvalidated
        }
    }

    /// `MEMQUIRA_REQUIRE_QUALITY_TIER`: refuses a configuration whose tier differs (fail closed).
    pub fn check_required_quality_tier(&self) -> MemquiraResult<QualityTier> {
        let tier = self.quality_tier();
        match self.providers.require_quality_tier {
            Some(required) if required != tier => Err(MemquiraError::ValidationError(format!(
                "MEMQUIRA_REQUIRE_QUALITY_TIER={} but this configuration is {}{}",
                required.as_str(),
                tier.as_str(),
                if required == QualityTier::ReferenceValidated {
                    format!(
                        " (differs from the reference contract in: {})",
                        self.reference_contract_mismatches().join(", ")
                    )
                } else {
                    String::new()
                }
            ))),
            _ => Ok(tier),
        }
    }

    pub fn qdrant_with_collection(
        qdrant_url: impl Into<String>,
        qdrant_collection: impl Into<String>,
    ) -> Self {
        Self {
            vector_backend: VectorBackend::Qdrant,
            qdrant_url: qdrant_url.into(),
            qdrant_collection: qdrant_collection.into(),
            ..Self::default()
        }
    }

    /// The extraction / chat endpoint: `MEMQUIRA_LLM_*`, else `OPENAI_*`.
    pub fn llm_endpoint(&self) -> ResolvedEndpoint {
        ResolvedEndpoint {
            base_url: self
                .providers
                .llm
                .base_url
                .clone()
                .unwrap_or_else(|| self.openai_base_url.clone()),
            api_key: self
                .providers
                .llm
                .api_key
                .clone()
                .or_else(|| self.openai_api_key.clone())
                .filter(|key| !key.trim().is_empty()),
        }
    }

    /// The embedding endpoint: `MEMQUIRA_EMBEDDING_*`, else `OPENAI_*`.
    pub fn embedding_endpoint(&self) -> ResolvedEndpoint {
        ResolvedEndpoint {
            base_url: self
                .providers
                .embedding
                .base_url
                .clone()
                .unwrap_or_else(|| self.openai_base_url.clone()),
            api_key: self
                .providers
                .embedding
                .api_key
                .clone()
                .or_else(|| self.openai_api_key.clone())
                .filter(|key| !key.trim().is_empty()),
        }
    }

    /// Startup validation of the per-role provider settings.
    fn validate_provider_roles(&self) -> MemquiraResult<()> {
        let p = &self.providers;
        if self.llm_provider == LlmProviderKind::OpenAi {
            validate_provider_endpoint(
                "LLM",
                &self.llm_endpoint(),
                p.llm.allow_no_api_key,
                p.allow_insecure_http,
            )?;
        }
        if self.embedding_provider == EmbeddingProviderKind::OpenAi {
            validate_provider_endpoint(
                "EMBEDDING",
                &self.embedding_endpoint(),
                p.embedding.allow_no_api_key,
                p.allow_insecure_http,
            )?;
        }
        let e = &p.embedding_options;
        if e.dimensions == Some(0) || e.dimensions.is_some_and(|d| d > 65_536) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EMBEDDING_DIMENSIONS must be 1..=65536".to_string(),
            ));
        }
        if let Some(t) = e.dedup_threshold
            && !(t > 0.0 && t <= 1.0)
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD must be in (0, 1]".to_string(),
            ));
        }
        Ok(())
    }

    /// Refuses every context-cache configuration
    /// whose invalidation is not verified for the deployment's process topology. Context-cache
    /// invalidation (including the FU1 per-user generation) is process-local, so:
    /// - `disabled` is always allowed (the default; the only production configuration);
    /// - `redis` is always refused, with or without the `redis-cache` feature: shared-cache
    ///   invalidation across API processes is not verified (no fallback to another backend);
    /// - `memory` is allowed only for local single-process development: `MEMQUIRA_ENV` is
    ///   development (it must be set explicitly, checked when reading the environment),
    ///   `MEMQUIRA_STORAGE_MODE` is not production, neither the fact nor the event backend is
    ///   PostgreSQL (a shared database invites several API processes), and the operator opted in
    ///   with `MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true`. The opt-in states, it does not
    ///   enforce, a single process; it never overrides a production marker.
    ///
    /// Called by configuration validation (every `Settings::from_env` / `load_settings`, before
    /// logging, dependencies, stores or the listener) and again by `AppState` before a cache
    /// backend is built, for settings constructed in code. Errors name variables only.
    pub fn check_context_cache_policy(&self) -> MemquiraResult<()> {
        let refuse = |reason: String| {
            Err(MemquiraError::ValidationError(format!(
                "context cache refused: {reason}; set {MEMQUIRA_CONTEXT_CACHE_BACKEND}=disabled"
            )))
        };
        match self.context_cache_backend {
            ContextCacheBackend::Disabled => Ok(()),
            ContextCacheBackend::Redis => refuse(format!(
                "{MEMQUIRA_CONTEXT_CACHE_BACKEND}=redis is not supported: shared-cache invalidation \
                 across API processes is not verified as safe (AR-023)"
            )),
            ContextCacheBackend::Memory => {
                let production_markers = [
                    (
                        self.environment == Environment::Production,
                        "MEMQUIRA_ENV=production",
                    ),
                    (
                        self.storage_mode == StorageMode::Production,
                        "MEMQUIRA_STORAGE_MODE=production",
                    ),
                    (
                        self.fact_backend == FactBackend::Postgres,
                        "MEMQUIRA_FACT_BACKEND=postgres",
                    ),
                    (
                        self.event_backend == EventBackend::Postgres,
                        "MEMQUIRA_EVENT_BACKEND=postgres",
                    ),
                ];
                let markers: Vec<&str> = production_markers
                    .iter()
                    .filter(|(present, _)| *present)
                    .map(|(_, name)| *name)
                    .collect();
                if !markers.is_empty() {
                    return refuse(format!(
                        "{MEMQUIRA_CONTEXT_CACHE_BACKEND}=memory is a process-local cache, supported \
                         only for local single-process development, not with {}",
                        markers.join(", ")
                    ));
                }
                if !self.context_cache_local_single_process {
                    return refuse(format!(
                        "{MEMQUIRA_CONTEXT_CACHE_BACKEND}=memory needs \
                         {MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS}=true (local single-process \
                         development only; several API processes would each keep a cache that \
                         another process cannot invalidate)"
                    ));
                }
                Ok(())
            }
        }
    }

    /// The recovery sweep must never treat a request that is still running as
    /// abandoned, so the stale window exceeds the write deadline (when admission bounds requests).
    fn validate_ingest_journal(&self) -> MemquiraResult<()> {
        if !self.ingest_journal_enabled {
            return Ok(());
        }
        if self.ingest_recovery_interval_secs == 0 || self.ingest_recovery_interval_secs > 86_400 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS must be 1..=86400".to_string(),
            ));
        }
        if self.ingest_recovery_stale_secs == 0 || self.ingest_recovery_stale_secs > 604_800 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_INGEST_RECOVERY_STALE_SECS must be 1..=604800".to_string(),
            ));
        }
        if !(60..=2_592_000).contains(&self.ingest_receipt_ttl_secs) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_INGEST_RECEIPT_TTL_SECS must be 60..=2592000".to_string(),
            ));
        }
        if self.ingest_inflight_wait_ms > 600_000 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_INGEST_INFLIGHT_WAIT_MS must be 0..=600000".to_string(),
            ));
        }
        if self.admission.enabled
            && self.ingest_recovery_stale_secs.saturating_mul(1_000)
                <= self.admission.write.deadline_ms
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_INGEST_RECOVERY_STALE_SECS must exceed the write deadline (MEMQUIRA_ADMISSION_WRITE_DEADLINE_MS)"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> MemquiraResult<()> {
        // First, so an unsafe cache configuration is reported as such .
        self.check_context_cache_policy()?;
        if self.host.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_HOST cannot be empty".to_string(),
            ));
        }

        if self.database_url.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_DATABASE_URL cannot be empty".to_string(),
            ));
        }

        if self.backup_dir.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKUP_DIR cannot be empty".to_string(),
            ));
        }

        if self.backup_max_files == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKUP_MAX_FILES must be greater than 0".to_string(),
            ));
        }

        if self.max_request_body_bytes == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_MAX_REQUEST_BODY_BYTES must be greater than 0".to_string(),
            ));
        }

        self.validate_ingest_journal()?;

        validate_cors_settings(self)?;

        if self.lancedb_path.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_LANCEDB_PATH cannot be empty".to_string(),
            ));
        }

        if self.lancedb_table.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_LANCEDB_TABLE cannot be empty".to_string(),
            ));
        }

        if self.auth_enabled
            && self.auth_mode == AuthMode::Dev
            && self.dev_api_key.trim().is_empty()
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_DEV_API_KEY cannot be empty when MEMQUIRA_AUTH_ENABLED=true and MEMQUIRA_AUTH_MODE=dev"
                    .to_string(),
            ));
        }

        if self.auth_mode == AuthMode::Database
            && self
                .api_key_pepper
                .as_ref()
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_API_KEY_PEPPER is required when MEMQUIRA_AUTH_MODE=database".to_string(),
            ));
        }

        if self.openai_base_url.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "OPENAI_BASE_URL cannot be empty".to_string(),
            ));
        }

        validate_admission(&self.admission)?;
        self.validate_provider_roles()?;

        if self.rate_limit_requests_per_minute == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE must be greater than 0".to_string(),
            ));
        }

        if self.request_id_header.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_REQUEST_ID_HEADER cannot be empty".to_string(),
            ));
        }

        validate_metrics_settings(self)?;

        // API keys are validated per role in `validate_provider_roles` (required unless
        // the endpoint is loopback or explicitly allowed without one).

        if self.fact_backend == FactBackend::Postgres
            && self
                .postgres_url
                .as_ref()
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_POSTGRES_URL is required when MEMQUIRA_FACT_BACKEND=postgres".to_string(),
            ));
        }

        if self.event_backend == EventBackend::Postgres
            && self
                .postgres_url
                .as_ref()
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_POSTGRES_URL is required when MEMQUIRA_EVENT_BACKEND=postgres"
                    .to_string(),
            ));
        }

        if self.fact_retention_days > 365_000 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_FACT_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.event_retention_days > 365_000 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EVENT_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.provider_usage_retention_days > 365_000 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.background_job_history_retention_days > 365_000 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.background_job_runner_interval_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.memory_usage_snapshot_job_interval_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.provider_usage_retention_job_interval_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.memory_retention_job_interval_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS must be greater than 0".to_string(),
            ));
        }

        if self.background_job_lock_ttl_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS must be greater than 0".to_string(),
            ));
        }

        if self.background_job_initial_backoff_ms == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS must be greater than 0".to_string(),
            ));
        }
        if self.background_job_max_backoff_ms < self.background_job_initial_backoff_ms {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS must be >= MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS"
                    .to_string(),
            ));
        }
        if self.background_job_backoff_multiplier < 1.0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER must be >= 1.0".to_string(),
            ));
        }
        if self.startup_dependency_timeout_seconds > 3_600 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS must be at most 3600".to_string(),
            ));
        }
        if self.graceful_shutdown_timeout_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS must be greater than 0".to_string(),
            ));
        }
        if self.background_job_shutdown_timeout_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        match self.context_cache_backend {
            ContextCacheBackend::Disabled => {}
            ContextCacheBackend::Memory => {
                if self.context_cache_ttl_seconds == 0 {
                    return Err(MemquiraError::ValidationError(
                        "MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS must be greater than 0 when context cache backend is memory"
                            .to_string(),
                    ));
                }
                if self.context_cache_max_entries == 0 {
                    return Err(MemquiraError::ValidationError(
                        "MEMQUIRA_CONTEXT_CACHE_MAX_ENTRIES must be greater than 0 when context cache backend is memory"
                            .to_string(),
                    ));
                }
            }
            ContextCacheBackend::Redis => {
                if self.context_cache_ttl_seconds == 0 {
                    return Err(MemquiraError::ValidationError(
                        "MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS must be greater than 0 when context cache backend is redis"
                            .to_string(),
                    ));
                }
                if self
                    .redis_url
                    .as_ref()
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
                {
                    return Err(MemquiraError::ValidationError(
                        "MEMQUIRA_REDIS_URL is required when MEMQUIRA_CONTEXT_CACHE_BACKEND=redis"
                            .to_string(),
                    ));
                }
            }
        }

        if self.context_cache_key_prefix.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_CONTEXT_CACHE_KEY_PREFIX cannot be empty".to_string(),
            ));
        }

        if self.context_cache_enabled
            && self.context_cache_stampede_protection_enabled
            && self.context_cache_lock_timeout_seconds == 0
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS must be greater than 0 when stampede protection is enabled"
                    .to_string(),
            ));
        }

        if self.context_cache_enabled
            && self.context_cache_stale_while_revalidate_enabled
            && self.context_cache_stale_ttl_seconds == 0
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS must be greater than 0 when stale-while-revalidate is enabled"
                    .to_string(),
            ));
        }

        if self.vector_backend == VectorBackend::Qdrant {
            if self.qdrant_url.trim().is_empty() {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_QDRANT_URL is required when MEMQUIRA_VECTOR_BACKEND=qdrant"
                        .to_string(),
                ));
            }
            if self.qdrant_collection.trim().is_empty() {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_QDRANT_COLLECTION cannot be empty when MEMQUIRA_VECTOR_BACKEND=qdrant"
                        .to_string(),
                ));
            }
        }

        if self.provider_timeout_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_TIMEOUT_SECONDS must be greater than 0".to_string(),
            ));
        }
        if self.provider_initial_backoff_ms == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS must be greater than 0".to_string(),
            ));
        }
        if self.provider_max_backoff_ms < self.provider_initial_backoff_ms {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_MAX_BACKOFF_MS must be >= MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS"
                    .to_string(),
            ));
        }
        if self.provider_backoff_multiplier < 1.0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER must be >= 1.0".to_string(),
            ));
        }

        if self.provider_circuit_breaker_failure_threshold == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD must be greater than 0"
                    .to_string(),
            ));
        }
        if self.provider_circuit_breaker_reset_timeout_seconds == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }
        if self.provider_circuit_breaker_half_open_max_calls == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.provider_max_calls_per_run == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_MAX_CALLS_PER_RUN must be greater than 0".to_string(),
            ));
        }
        if self.provider_max_input_chars == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_MAX_INPUT_CHARS must be greater than 0".to_string(),
            ));
        }
        if self.provider_operation_header_enabled && self.provider_operation_header_hosts.is_empty()
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED=true requires MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS \
                 (the trusted proxy's host or host:port); the token is never sent to other provider URLs"
                    .to_string(),
            ));
        }
        if self
            .provider_operation_header_hosts
            .iter()
            .any(|h| h.contains(['*', '/', '@', ' ']))
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS entries are exact host or host:port values (no wildcards, \
                 paths or credentials)"
                    .to_string(),
            ));
        }
        if self.provider_max_output_tokens == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS must be greater than 0".to_string(),
            ));
        }
        for (key, weight) in [
            ("MEMQUIRA_RRF_WEIGHT_SEMANTIC", self.rrf_weight_semantic),
            ("MEMQUIRA_RRF_WEIGHT_KEYWORD", self.rrf_weight_keyword),
        ] {
            if !(weight.is_finite() && weight >= 0.0) {
                return Err(MemquiraError::ValidationError(format!(
                    "{key} must be a number >= 0"
                )));
            }
        }
        let pipeline_on = self.query_analysis_enabled
            || self.temporal_ranking_enabled
            || self.multi_query != MultiQuerySetting::None
            || self.rerank_provider != RerankProviderSetting::None
            || self.query_decomposition != QueryDecompositionSetting::None;
        if pipeline_on && !self.experimental_candidates_enabled {
            return Err(MemquiraError::ValidationError(
                "the Phase 4 candidate flags need MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED"
                    .to_string(),
            ));
        }
        if (self.temporal_ranking_enabled || self.multi_query != MultiQuerySetting::None)
            && !self.query_analysis_enabled
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_TEMPORAL_RANKING_ENABLED and MEMQUIRA_MULTI_QUERY need MEMQUIRA_QUERY_ANALYSIS_ENABLED"
                    .to_string(),
            ));
        }
        if !(self.temporal_boost > 0.0 && self.temporal_boost <= 1.0) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_TEMPORAL_BOOST must be greater than 0 and at most 1".to_string(),
            ));
        }
        if !(1..=100).contains(&self.rerank_candidates) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_RERANK_CANDIDATES must be between 1 and 100".to_string(),
            ));
        }
        if !(100..=600_000).contains(&self.rerank_timeout_ms)
            || !(100..=600_000).contains(&self.query_decomposition_timeout_ms)
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_RERANK_TIMEOUT_MS and MEMQUIRA_QUERY_DECOMPOSITION_TIMEOUT_MS must be between 100 and 600000"
                    .to_string(),
            ));
        }
        if self.rerank_provider == RerankProviderSetting::Local {
            if self.rerank_local_model_path.is_none()
                || self.rerank_local_model_sha256.is_none()
                || self.rerank_local_tokenizer_path.is_none()
                || self.rerank_local_tokenizer_sha256.is_none()
            {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_RERANK_PROVIDER=local needs MEMQUIRA_RERANK_LOCAL_MODEL_PATH, _MODEL_SHA256, _TOKENIZER_PATH and _TOKENIZER_SHA256"
                        .to_string(),
                ));
            }
            if !(1..=64).contains(&self.rerank_local_threads)
                || !(1..=64).contains(&self.rerank_local_batch_size)
            {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_RERANK_LOCAL_THREADS and MEMQUIRA_RERANK_LOCAL_BATCH_SIZE must be between 1 and 64"
                        .to_string(),
                ));
            }
        }
        if self.rerank_provider == RerankProviderSetting::Bedrock
            && (self.rerank_bedrock_region.is_empty()
                || !self
                    .rerank_bedrock_model_arn
                    .starts_with(&format!("arn:aws:bedrock:{}:", self.rerank_bedrock_region)))
        {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_RERANK_PROVIDER=bedrock needs MEMQUIRA_RERANK_BEDROCK_REGION, and                  MEMQUIRA_RERANK_BEDROCK_MODEL_ARN (if set) must be a Bedrock ARN in that region"
                    .to_string(),
            ));
        }
        if self.episodes_enabled && self.qdrant_episode_collection == self.qdrant_collection {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_QDRANT_EPISODE_COLLECTION must differ from MEMQUIRA_QDRANT_COLLECTION"
                    .to_string(),
            ));
        }
        if !(50..=200).contains(&self.search_max_limit) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_SEARCH_MAX_LIMIT must be between 50 and 200".to_string(),
            ));
        }
        if self.embedding_provider.is_local() {
            if self
                .local_embedding_model_dir
                .as_deref()
                .is_none_or(|dir| dir.trim().is_empty())
            {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR is required for a local embedding provider"
                        .to_string(),
                ));
            }
            if self.vector_backend != VectorBackend::Qdrant {
                return Err(MemquiraError::ValidationError(
                    "a local embedding provider requires MEMQUIRA_VECTOR_BACKEND=qdrant (local \
                     stores record and check their embedding identity)"
                        .to_string(),
                ));
            }
        }
        if !(1..=1024).contains(&self.local_embedding_queue_capacity) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY must be between 1 and 1024".to_string(),
            ));
        }
        if self.export_max_records == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EXPORT_MAX_RECORDS must be greater than 0".to_string(),
            ));
        }
        if self.query_embedding_cache_capacity > 1_000_000 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY must be at most 1000000".to_string(),
            ));
        }
        if !(self.rrf_k.is_finite() && self.rrf_k > 0.0) {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_RRF_K must be a positive number".to_string(),
            ));
        }
        if self.extraction_window_turns == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EXTRACTION_WINDOW_TURNS must be greater than 0".to_string(),
            ));
        }
        if self.extraction_window_overlap >= self.extraction_window_turns {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EXTRACTION_WINDOW_OVERLAP must be smaller than MEMQUIRA_EXTRACTION_WINDOW_TURNS"
                    .to_string(),
            ));
        }
        if self.extraction_concurrency == 0 {
            return Err(MemquiraError::ValidationError(
                "MEMQUIRA_EXTRACTION_CONCURRENCY must be greater than 0".to_string(),
            ));
        }
        for (key, value) in [
            (
                MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS,
                self.extraction_max_output_tokens,
            ),
            (
                MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS,
                self.classification_max_output_tokens,
            ),
        ] {
            // An explicit per-operation limit above the ceiling is rejected at startup rather than
            // silently clamped: a clamped extraction limit is exactly how responses got truncated.
            match value {
                Some(0) => {
                    return Err(MemquiraError::ValidationError(format!(
                        "{key} must be greater than 0"
                    )));
                }
                Some(limit) if limit > self.provider_max_output_tokens => {
                    return Err(MemquiraError::ValidationError(format!(
                        "{key} ({limit}) must not exceed MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS ({})",
                        self.provider_max_output_tokens
                    )));
                }
                _ => {}
            }
        }
        if self.provider_max_retries_per_call > MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES {
            return Err(MemquiraError::ValidationError(format!(
                "MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL cannot exceed {MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES}"
            )));
        }
        if self.provider_test_mode == ProviderTestMode::MultiReal {
            if !self.multi_provider_validation_enabled {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_MULTI_PROVIDER_VALIDATION_ENABLED must be true when MEMQUIRA_PROVIDER_TEST_MODE=multi_real"
                        .to_string(),
                ));
            }
            let confirmed = self
                .multi_provider_validation_confirmation
                .as_deref()
                .map(str::trim)
                == Some(MULTI_PROVIDER_VALIDATION_CONFIRMATION);
            if !confirmed {
                return Err(MemquiraError::ValidationError(
                    "MEMQUIRA_MULTI_PROVIDER_VALIDATION_CONFIRMATION must be exactly I_UNDERSTAND_THIS_WILL_USE_PROVIDER_CREDITS for multi_real mode"
                        .to_string(),
                ));
            }
        }

        Ok(())
    }
}

/// Loads optional `.env` from the current directory or parents, then reads settings
/// from the process environment.
///
/// Missing `.env` is normal and does not fail startup. Variables already set in the
/// process environment (Docker, CI, systemd, etc.) are never overwritten by `.env`.
pub fn load_settings() -> MemquiraResult<Settings> {
    load_dotenv_if_present();
    Settings::from_env()
}

/// Loads `.env` when present. No-op when the file is missing.
fn load_dotenv_if_present() {
    let _ = dotenvy::dotenv();
}

fn read_env_or(key: &str, default: &str) -> String {
    env_var(key).unwrap_or_else(|_| default.to_string())
}

/// Amazon Rerank 1.0 in `region`; empty when no region is configured (validation then rejects
/// `bedrock`).
fn amazon_rerank_model_arn(region: &str) -> String {
    if region.is_empty() {
        String::new()
    } else {
        format!("arn:aws:bedrock:{region}::foundation-model/amazon.rerank-v1:0")
    }
}

/// The default stale window, at least 300 s and twice the write deadline (rounded
/// up to a second) whenever admission bounds a write.
fn default_ingest_recovery_stale_secs(admission: &AdmissionSettings) -> u64 {
    if !admission.enabled {
        return 300;
    }
    300u64.max(
        admission
            .write
            .deadline_ms
            .saturating_mul(2)
            .div_ceil(1_000),
    )
}

fn read_env_optional(key: &str) -> Option<String> {
    match env_var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        Ok(_) | Err(_) => None,
    }
}

fn parse_provider_fallback_order(key: &str, default: &[String]) -> MemquiraResult<Vec<String>> {
    match env_var(key) {
        Ok(value) => {
            let parsed = value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| part.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if parsed.is_empty() {
                return Err(MemquiraError::ValidationError(format!(
                    "{key} cannot be empty"
                )));
            }
            Ok(parsed)
        }
        Err(_) => Ok(default.to_vec()),
    }
}

fn parse_string_list(key: &str, default: &[String]) -> Vec<String> {
    match env_var(key) {
        Ok(value) => value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
            .collect(),
        Err(_) => default.to_vec(),
    }
}

fn validate_metrics_settings(settings: &Settings) -> MemquiraResult<()> {
    let path = settings.metrics_path.trim();
    if path.is_empty() {
        return Err(MemquiraError::ValidationError(
            "MEMQUIRA_METRICS_PATH cannot be empty".to_string(),
        ));
    }
    if !path.starts_with('/') {
        return Err(MemquiraError::ValidationError(
            "MEMQUIRA_METRICS_PATH must start with '/'".to_string(),
        ));
    }
    const RESERVED: &[&str] = &["/health", "/ready", "/api/v1/version"];
    if RESERVED.contains(&path) {
        return Err(MemquiraError::ValidationError(format!(
            "MEMQUIRA_METRICS_PATH cannot collide with reserved path {path}"
        )));
    }
    Ok(())
}

fn validate_cors_settings(settings: &Settings) -> MemquiraResult<()> {
    for origin in &settings.cors_allowed_origins {
        if !is_valid_cors_origin(origin) {
            return Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_CORS_ALLOWED_ORIGINS value: {origin}"
            )));
        }
    }

    if settings.cors_allow_credentials
        && settings
            .cors_allowed_origins
            .iter()
            .any(|origin| origin.trim() == "*")
    {
        return Err(MemquiraError::ValidationError(
            "MEMQUIRA_CORS_ALLOWED_ORIGINS cannot include * when MEMQUIRA_CORS_ALLOW_CREDENTIALS=true"
                .to_string(),
        ));
    }

    for method in &settings.cors_allowed_methods {
        if !is_valid_http_method(method) {
            return Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_CORS_ALLOWED_METHODS value: {method}"
            )));
        }
    }

    for header in settings
        .cors_allowed_headers
        .iter()
        .chain(settings.cors_exposed_headers.iter())
    {
        if header.trim().is_empty() {
            return Err(MemquiraError::ValidationError(
                "CORS header names cannot be empty".to_string(),
            ));
        }
        if header.chars().any(|ch| ch.is_whitespace()) {
            return Err(MemquiraError::ValidationError(format!(
                "Invalid CORS header name: {header}"
            )));
        }
    }

    Ok(())
}

fn is_valid_cors_origin(origin: &str) -> bool {
    let origin = origin.trim();
    if origin == "*" {
        return true;
    }
    if origin.contains(char::is_whitespace) {
        return false;
    }
    (origin.starts_with("http://") || origin.starts_with("https://")) && origin.len() > 8
}

fn is_valid_http_method(method: &str) -> bool {
    matches!(
        method.trim().to_ascii_uppercase().as_str(),
        "GET" | "POST" | "PUT" | "DELETE" | "OPTIONS" | "PATCH" | "HEAD"
    )
}

fn parse_u16(key: &str, default: u16) -> MemquiraResult<u16> {
    match env_var(key) {
        Ok(value) => value
            .parse::<u16>()
            .map_err(|_| MemquiraError::ValidationError(format!("{key} must be a valid u16 port"))),
        Err(_) => Ok(default),
    }
}

fn parse_provider_roles(defaults: &ProviderRoleSettings) -> MemquiraResult<ProviderRoleSettings> {
    let text = |key: &str| read_env_optional(key).filter(|v| !v.trim().is_empty());
    let dimensions = match text(MEMQUIRA_EMBEDDING_DIMENSIONS) {
        None => None,
        Some(value) => Some(value.trim().parse::<usize>().map_err(|_| {
            MemquiraError::ValidationError(
                "MEMQUIRA_EMBEDDING_DIMENSIONS must be a positive integer".to_string(),
            )
        })?),
    };
    let dedup_threshold = match text(MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD) {
        None => None,
        Some(value) => Some(value.trim().parse::<f32>().map_err(|_| {
            MemquiraError::ValidationError(
                "MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD must be a number in (0, 1]".to_string(),
            )
        })?),
    };
    let probe = match text(MEMQUIRA_PROVIDER_PROBE) {
        None => defaults.probe,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => true,
            "off" | "false" | "0" => false,
            _ => {
                return Err(MemquiraError::ValidationError(format!(
                    "Invalid MEMQUIRA_PROVIDER_PROBE value: {value} (expected on|off)"
                )));
            }
        },
    };
    let llm_json_mode = match text(MEMQUIRA_LLM_JSON_MODE) {
        None => defaults.llm_json_mode,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "json_object" => LlmJsonMode::JsonObject,
            "json_schema" => LlmJsonMode::JsonSchema,
            "instruction" => LlmJsonMode::Instruction,
            _ => {
                return Err(MemquiraError::ValidationError(format!(
                    "Invalid MEMQUIRA_LLM_JSON_MODE value: {value} (expected json_object|json_schema|instruction)"
                )));
            }
        },
    };
    let require_quality_tier = match text(MEMQUIRA_REQUIRE_QUALITY_TIER) {
        None => defaults.require_quality_tier,
        Some(value) => Some(QualityTier::parse(&value).ok_or_else(|| {
            MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_REQUIRE_QUALITY_TIER value: {value} (expected reference-validated|compatible-unvalidated|development-mock)"
            ))
        })?),
    };
    Ok(ProviderRoleSettings {
        llm: RoleEndpoint {
            base_url: text(MEMQUIRA_LLM_BASE_URL),
            api_key: text(MEMQUIRA_LLM_API_KEY),
            allow_no_api_key: parse_bool(MEMQUIRA_LLM_ALLOW_NO_API_KEY, false)?,
        },
        llm_json_mode,
        embedding: RoleEndpoint {
            base_url: text(MEMQUIRA_EMBEDDING_BASE_URL),
            api_key: text(MEMQUIRA_EMBEDDING_API_KEY),
            allow_no_api_key: parse_bool(MEMQUIRA_EMBEDDING_ALLOW_NO_API_KEY, false)?,
        },
        embedding_options: EmbeddingRoleSettings {
            dimensions,
            send_dimensions: parse_bool(
                MEMQUIRA_EMBEDDING_SEND_DIMENSIONS,
                defaults.embedding_options.send_dimensions,
            )?,
            normalize: parse_bool(MEMQUIRA_EMBEDDING_NORMALIZE, false)?,
            query_prefix: read_env_optional(MEMQUIRA_EMBEDDING_QUERY_PREFIX)
                .filter(|v| !v.is_empty()),
            document_prefix: read_env_optional(MEMQUIRA_EMBEDDING_DOCUMENT_PREFIX)
                .filter(|v| !v.is_empty()),
            model_revision: text(MEMQUIRA_EMBEDDING_MODEL_REVISION),
            serving_variant: text(MEMQUIRA_EMBEDDING_SERVING_VARIANT),
            dedup_threshold,
        },
        allow_insecure_http: parse_bool(MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP, false)?,
        probe,
        require_quality_tier,
        llm_startup_probe: parse_bool(MEMQUIRA_LLM_STARTUP_PROBE, defaults.llm_startup_probe)?,
    })
}

fn parse_admission(defaults: &AdmissionSettings) -> MemquiraResult<AdmissionSettings> {
    let enabled = match read_env_optional(MEMQUIRA_ADMISSION_CONTROL) {
        None => defaults.enabled,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => true,
            "off" | "false" | "0" => false,
            _ => {
                return Err(MemquiraError::ValidationError(format!(
                    "Invalid MEMQUIRA_ADMISSION_CONTROL value: {value} (expected on|off)"
                )));
            }
        },
    };
    let class =
        |keys: [&str; 3], d: AdmissionClassLimits| -> MemquiraResult<AdmissionClassLimits> {
            Ok(AdmissionClassLimits {
                max_in_flight: parse_usize(keys[0], d.max_in_flight)?,
                max_queue: parse_usize(keys[1], d.max_queue)?,
                deadline_ms: parse_u64(keys[2], d.deadline_ms)?,
            })
        };
    Ok(AdmissionSettings {
        enabled,
        read: class(
            [
                MEMQUIRA_ADMISSION_READ_MAX_IN_FLIGHT,
                MEMQUIRA_ADMISSION_READ_MAX_QUEUE,
                MEMQUIRA_ADMISSION_READ_DEADLINE_MS,
            ],
            defaults.read,
        )?,
        write: class(
            [
                MEMQUIRA_ADMISSION_WRITE_MAX_IN_FLIGHT,
                MEMQUIRA_ADMISSION_WRITE_MAX_QUEUE,
                MEMQUIRA_ADMISSION_WRITE_DEADLINE_MS,
            ],
            defaults.write,
        )?,
        admin: class(
            [
                MEMQUIRA_ADMISSION_ADMIN_MAX_IN_FLIGHT,
                MEMQUIRA_ADMISSION_ADMIN_MAX_QUEUE,
                MEMQUIRA_ADMISSION_ADMIN_DEADLINE_MS,
            ],
            defaults.admin,
        )?,
        queue_timeout_ms: parse_u64(
            MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS,
            defaults.queue_timeout_ms,
        )?,
        retry_after_seconds: parse_u64(
            MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS,
            defaults.retry_after_seconds,
        )?,
        provider_chat_max_concurrency: parse_usize(
            MEMQUIRA_PROVIDER_CHAT_MAX_CONCURRENCY,
            defaults.provider_chat_max_concurrency,
        )?,
        provider_embedding_max_concurrency: parse_usize(
            MEMQUIRA_PROVIDER_EMBEDDING_MAX_CONCURRENCY,
            defaults.provider_embedding_max_concurrency,
        )?,
        provider_permit_timeout_ms: parse_u64(
            MEMQUIRA_PROVIDER_PERMIT_TIMEOUT_MS,
            defaults.provider_permit_timeout_ms,
        )?,
    })
}

/// Deterministic startup validation of the admission limits.
fn validate_admission(a: &AdmissionSettings) -> MemquiraResult<()> {
    let invalid = |message: String| Err(MemquiraError::ValidationError(message));
    for (name, c) in [("READ", a.read), ("WRITE", a.write), ("ADMIN", a.admin)] {
        if !(1..=100_000).contains(&c.max_in_flight) {
            return invalid(format!(
                "MEMQUIRA_ADMISSION_{name}_MAX_IN_FLIGHT must be 1..=100000"
            ));
        }
        if c.max_queue > 1_000_000 {
            return invalid(format!(
                "MEMQUIRA_ADMISSION_{name}_MAX_QUEUE must be 0..=1000000"
            ));
        }
        if c.deadline_ms <= a.queue_timeout_ms {
            return invalid(format!(
                "MEMQUIRA_ADMISSION_{name}_DEADLINE_MS must be greater than MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS"
            ));
        }
    }
    if a.queue_timeout_ms == 0 {
        return invalid("MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS must be greater than 0".to_string());
    }
    if !(1..=3_600).contains(&a.retry_after_seconds) {
        return invalid("MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS must be 1..=3600".to_string());
    }
    if a.provider_chat_max_concurrency == 0 || a.provider_embedding_max_concurrency == 0 {
        return invalid(
            "MEMQUIRA_PROVIDER_CHAT_MAX_CONCURRENCY and MEMQUIRA_PROVIDER_EMBEDDING_MAX_CONCURRENCY must be greater than 0"
                .to_string(),
        );
    }
    if a.provider_permit_timeout_ms == 0 {
        return invalid("MEMQUIRA_PROVIDER_PERMIT_TIMEOUT_MS must be greater than 0".to_string());
    }
    Ok(())
}

fn parse_u32(key: &str, default: u32) -> MemquiraResult<u32> {
    match env_var(key) {
        Ok(value) => value.parse::<u32>().map_err(|_| {
            MemquiraError::ValidationError(format!("{key} must be a valid unsigned integer"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_f32(key: &str, default: f32) -> MemquiraResult<f32> {
    match env_var(key) {
        Ok(value) => value.parse::<f32>().map_err(|_| {
            MemquiraError::ValidationError(format!("{key} must be a valid floating-point number"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_u64(key: &str, default: u64) -> MemquiraResult<u64> {
    match env_var(key) {
        Ok(value) => value.parse::<u64>().map_err(|_| {
            MemquiraError::ValidationError(format!("{key} must be a valid unsigned integer"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_optional_usize(key: &str) -> MemquiraResult<Option<usize>> {
    read_env_optional(key)
        .map(|value| {
            value.trim().parse::<usize>().map_err(|_| {
                MemquiraError::ValidationError(format!("{key} must be a valid unsigned integer"))
            })
        })
        .transpose()
}

fn parse_usize(key: &str, default: usize) -> MemquiraResult<usize> {
    match env_var(key) {
        Ok(value) => value.parse::<usize>().map_err(|_| {
            MemquiraError::ValidationError(format!("{key} must be a valid unsigned integer"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_bool(key: &str, default: bool) -> MemquiraResult<bool> {
    match env_var(key) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            _ => Err(MemquiraError::ValidationError(format!(
                "{key} must be one of: true, false, 1, 0, yes, no"
            ))),
        },
        Err(_) => Ok(default),
    }
}

impl FromStr for Environment {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "production" | "prod" => Ok(Self::Production),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_ENV value: {value}"
            ))),
        }
    }
}

impl FromStr for StorageMode {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "embedded" => Ok(Self::Embedded),
            "production" => Ok(Self::Production),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_STORAGE_MODE value: {value}"
            ))),
        }
    }
}

impl FromStr for VectorBackend {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "lancedb" => Ok(Self::LanceDb),
            "qdrant" => Ok(Self::Qdrant),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_VECTOR_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for FactBackend {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_FACT_BACKEND value: {value}"
            ))),
        }
    }
}

impl EventBackend {
    pub fn default_for_fact_backend(fact_backend: &FactBackend) -> Self {
        match fact_backend {
            FactBackend::Mock => Self::Mock,
            FactBackend::Sqlite => Self::Sqlite,
            FactBackend::Postgres => Self::Postgres,
        }
    }
}

impl FromStr for EventBackend {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_EVENT_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for LlmProviderKind {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "openai" => Ok(Self::OpenAi),
            "openrouter" => Ok(Self::OpenRouter),
            "anthropic" => Ok(Self::Anthropic),
            "groq" => Ok(Self::Groq),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_LLM_PROVIDER value: {value}"
            ))),
        }
    }
}

impl FromStr for EmbeddingProviderKind {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "openai" => Ok(Self::OpenAi),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_EMBEDDING_PROVIDER value: {value}"
            ))),
        }
    }
}

impl FromStr for ContextCacheBackend {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "memory" => Ok(Self::Memory),
            "redis" => Ok(Self::Redis),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_CONTEXT_CACHE_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for BackgroundJobLockBackend {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "database" => Ok(Self::Database),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for DatabaseMigrationMode {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "validate_only" | "validate-only" => Ok(Self::ValidateOnly),
            "disabled" => Ok(Self::Disabled),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_DATABASE_MIGRATION_MODE value: {value}"
            ))),
        }
    }
}

impl FromStr for AuthMode {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dev" => Ok(Self::Dev),
            "database" => Ok(Self::Database),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_AUTH_MODE value: {value}"
            ))),
        }
    }
}

impl FromStr for ProviderTestMode {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock_only" => Ok(Self::MockOnly),
            "single_real" => Ok(Self::SingleReal),
            "multi_real" => Ok(Self::MultiReal),
            "production" => Ok(Self::Production),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_PROVIDER_TEST_MODE value: {value} (expected mock_only|single_real|multi_real|production)"
            ))),
        }
    }
}

impl FromStr for QdrantWriteModeSetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "optimized" => Ok(Self::Optimized),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_QDRANT_WRITE_MODE value: {value} (expected legacy|optimized)"
            ))),
        }
    }
}

impl FromStr for ConsolidationMode {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "llm_v2" => Ok(Self::LlmV2),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_CONSOLIDATION_MODE value: {value} (expected legacy|llm_v2)"
            ))),
        }
    }
}

impl FromStr for RelatedFactMatcher {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "semantic" => Ok(Self::Semantic),
            "semantic_v23" => Ok(Self::SemanticV23),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_RELATED_FACT_MATCHER value: {value} (expected legacy|semantic|semantic_v23)"
            ))),
        }
    }
}

impl FromStr for TemporalRewriteSetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "preserving" => Ok(Self::Preserving),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_TEMPORAL_REWRITE value: {value} (expected legacy|preserving)"
            ))),
        }
    }
}

impl FromStr for UnknownMemoryTypeSetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "skip" => Ok(Self::Skip),
            "other" => Ok(Self::Other),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_UNKNOWN_MEMORY_TYPE value: {value} (expected skip|other)"
            ))),
        }
    }
}

impl FromStr for DedupValueGuardSetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "on" => Ok(Self::On),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_DEDUP_VALUE_GUARD value: {value} (expected off|on)"
            ))),
        }
    }
}

impl FromStr for EventUpdatePolicySetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "history" => Ok(Self::History),
            "change_marker" => Ok(Self::ChangeMarker),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_EVENT_UPDATE_POLICY value: {value} (expected history|change_marker)"
            ))),
        }
    }
}

impl FromStr for ImportancePolicySetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "durable" => Ok(Self::Durable),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_IMPORTANCE_POLICY value: {value} (expected legacy|durable)"
            ))),
        }
    }
}

impl FromStr for ClassificationPromptSetting {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "operations_v1" => Ok(Self::OperationsV1),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_CLASSIFICATION_PROMPT value: {value} (expected legacy|operations_v1)"
            ))),
        }
    }
}

impl FromStr for ContextPacking {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "prefix" => Ok(Self::Prefix),
            "fill" => Ok(Self::Fill),
            "evidence" => Ok(Self::Evidence),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_CONTEXT_DEFAULT_PACKING value: {value} (expected prefix|fill|evidence)"
            ))),
        }
    }
}

impl FromStr for StructuredExtractionPolicy {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "atomic" => Ok(Self::Atomic),
            "retentive" => Ok(Self::Retentive),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_STRUCTURED_EXTRACTION_POLICY value: {value}"
            ))),
        }
    }
}

impl FromStr for LogFormat {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "pretty" => Ok(Self::Pretty),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_LOG_FORMAT value: {value}"
            ))),
        }
    }
}

impl FromStr for LogLevel {
    type Err = MemquiraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(MemquiraError::ValidationError(format!(
                "Invalid MEMQUIRA_LOG_LEVEL value: {value}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    use super::{EmbeddingProviderKind, LlmJsonMode, LlmProviderKind};
    use super::{
        Environment, MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES, MULTI_PROVIDER_VALIDATION_CONFIRMATION,
        MultiQuerySetting, ProviderTestMode, QueryDecompositionSetting, RerankProviderSetting,
        Settings, StorageMode, VectorBackend,
    };

    const ENV_KEYS: &[&str] = &[
        "MEMQUIRA_ENV",
        "MEMQUIRA_HOST",
        "MEMQUIRA_PORT",
        "MEMQUIRA_STORAGE_MODE",
        "MEMQUIRA_VECTOR_BACKEND",
        "MEMQUIRA_FACT_BACKEND",
        "MEMQUIRA_EVENT_BACKEND",
        "MEMQUIRA_DATABASE_URL",
        "MEMQUIRA_POSTGRES_URL",
        "MEMQUIRA_DATABASE_MIGRATIONS_ENABLED",
        "MEMQUIRA_DATABASE_MIGRATION_MODE",
        "MEMQUIRA_DATABASE_REQUIRE_CLEAN_MIGRATIONS",
        "MEMQUIRA_BACKUP_ENABLED",
        "MEMQUIRA_BACKUP_DIR",
        "MEMQUIRA_BACKUP_MAX_FILES",
        "MEMQUIRA_RESTORE_ENABLED",
        "MEMQUIRA_SECURITY_HEADERS_ENABLED",
        "MEMQUIRA_MAX_REQUEST_BODY_BYTES",
        "MEMQUIRA_CORS_ENABLED",
        "MEMQUIRA_CORS_ALLOWED_ORIGINS",
        "MEMQUIRA_CORS_ALLOWED_METHODS",
        "MEMQUIRA_CORS_ALLOWED_HEADERS",
        "MEMQUIRA_CORS_EXPOSED_HEADERS",
        "MEMQUIRA_CORS_ALLOW_CREDENTIALS",
        "MEMQUIRA_QDRANT_URL",
        "MEMQUIRA_QDRANT_COLLECTION",
        "MEMQUIRA_LANCEDB_PATH",
        "MEMQUIRA_LANCEDB_TABLE",
        "MEMQUIRA_LLM_PROVIDER",
        "MEMQUIRA_LLM_MODEL",
        "MEMQUIRA_EMBEDDING_PROVIDER",
        "MEMQUIRA_EMBEDDING_MODEL",
        "MEMQUIRA_ENABLE_PII_REDACTION",
        "MEMQUIRA_STRUCTURED_EXTRACTION_ENABLED",
        "MEMQUIRA_EPISODES_ENABLED",
        "MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED",
        "MEMQUIRA_QUERY_ANALYSIS_ENABLED",
        "MEMQUIRA_TEMPORAL_RANKING_ENABLED",
        "MEMQUIRA_TEMPORAL_BOOST",
        "MEMQUIRA_MULTI_QUERY",
        "MEMQUIRA_RERANK_PROVIDER",
        "MEMQUIRA_RERANK_CANDIDATES",
        "MEMQUIRA_RERANK_TIMEOUT_MS",
        "MEMQUIRA_RERANK_LOCAL_MODEL_PATH",
        "MEMQUIRA_RERANK_LOCAL_MODEL_SHA256",
        "MEMQUIRA_RERANK_LOCAL_TOKENIZER_PATH",
        "MEMQUIRA_RERANK_LOCAL_TOKENIZER_SHA256",
        "MEMQUIRA_RERANK_LOCAL_THREADS",
        "MEMQUIRA_RERANK_LOCAL_BATCH_SIZE",
        "MEMQUIRA_RERANK_BEDROCK_REGION",
        "MEMQUIRA_RERANK_BEDROCK_MODEL_ARN",
        "MEMQUIRA_RERANK_BEDROCK_PROFILE",
        "MEMQUIRA_QUERY_DECOMPOSITION",
        "MEMQUIRA_QUERY_DECOMPOSITION_TIMEOUT_MS",
        "MEMQUIRA_QDRANT_EPISODE_COLLECTION",
        "MEMQUIRA_QDRANT_ENSURE_INDEXES",
        "MEMQUIRA_QDRANT_WRITE_MODE",
        "MEMQUIRA_QDRANT_SEARCH_HNSW_EF",
        "MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS",
        "MEMQUIRA_STRUCTURED_EXTRACTION_POLICY",
        "MEMQUIRA_CONSOLIDATION_MODE",
        "MEMQUIRA_RELATED_FACT_MATCHER",
        "MEMQUIRA_TEMPORAL_REWRITE",
        "MEMQUIRA_UNKNOWN_MEMORY_TYPE",
        "MEMQUIRA_EVENT_UPDATE_POLICY",
        "MEMQUIRA_DEDUP_VALUE_GUARD",
        "MEMQUIRA_RELATED_FACT_SIMILARITY_FLOOR",
        "MEMQUIRA_IMPORTANCE_POLICY",
        "MEMQUIRA_CLASSIFICATION_PROMPT",
        "MEMQUIRA_CONTEXT_DEFAULT_PACKING",
        "MEMQUIRA_CONSOLIDATION_CANDIDATES",
        "MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR",
        "MEMQUIRA_ENTITIES_ENABLED",
        "MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD",
        "MEMQUIRA_ENTITY_LLM_CONFIRMATION_ENABLED",
        "MEMQUIRA_SEARCH_STRATEGY",
        "MEMQUIRA_RRF_K",
        "MEMQUIRA_SEARCH_MAX_LIMIT",
        "MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY",
        "MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED",
        "MEMQUIRA_INGEST_JOURNAL_ENABLED",
        "MEMQUIRA_INGEST_RECOVERY_STALE_SECS",
        "MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS",
        "MEMQUIRA_INGEST_INFLIGHT_WAIT_MS",
        "MEMQUIRA_INGEST_RECEIPT_TTL_SECS",
        "MEMQUIRA_EXPORT_MAX_RECORDS",
        "MEMQUIRA_LOCAL_EMBEDDING_MODEL_DIR",
        "MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY",
        "MEMQUIRA_RRF_WEIGHT_SEMANTIC",
        "MEMQUIRA_RRF_WEIGHT_KEYWORD",
        "MEMQUIRA_EXTRACTION_WINDOW_TURNS",
        "MEMQUIRA_EXTRACTION_WINDOW_OVERLAP",
        "MEMQUIRA_EXTRACTION_CONCURRENCY",
        "MEMQUIRA_MIN_IMPORTANCE",
        "MEMQUIRA_AUTH_ENABLED",
        "MEMQUIRA_AUTH_MODE",
        "MEMQUIRA_DEV_API_KEY",
        "MEMQUIRA_API_KEY_PEPPER",
        "MEMQUIRA_RATE_LIMIT_ENABLED",
        "MEMQUIRA_LLM_BASE_URL",
        "MEMQUIRA_LLM_API_KEY",
        "MEMQUIRA_LLM_ALLOW_NO_API_KEY",
        "MEMQUIRA_EMBEDDING_BASE_URL",
        "MEMQUIRA_EMBEDDING_API_KEY",
        "MEMQUIRA_EMBEDDING_ALLOW_NO_API_KEY",
        "MEMQUIRA_EMBEDDING_DIMENSIONS",
        "MEMQUIRA_EMBEDDING_SEND_DIMENSIONS",
        "MEMQUIRA_EMBEDDING_NORMALIZE",
        "MEMQUIRA_EMBEDDING_QUERY_PREFIX",
        "MEMQUIRA_EMBEDDING_DOCUMENT_PREFIX",
        "MEMQUIRA_EMBEDDING_MODEL_REVISION",
        "MEMQUIRA_EMBEDDING_SERVING_VARIANT",
        "MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD",
        "MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP",
        "MEMQUIRA_PROVIDER_PROBE",
        "MEMQUIRA_REQUIRE_QUALITY_TIER",
        "MEMQUIRA_LLM_STARTUP_PROBE",
        "MEMQUIRA_LLM_JSON_MODE",
        "MEMQUIRA_ADMISSION_CONTROL",
        "MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS",
        "MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS",
        "MEMQUIRA_PROVIDER_CHAT_MAX_CONCURRENCY",
        "MEMQUIRA_PROVIDER_EMBEDDING_MAX_CONCURRENCY",
        "MEMQUIRA_PROVIDER_PERMIT_TIMEOUT_MS",
        "MEMQUIRA_ADMISSION_READ_MAX_IN_FLIGHT",
        "MEMQUIRA_ADMISSION_READ_MAX_QUEUE",
        "MEMQUIRA_ADMISSION_READ_DEADLINE_MS",
        "MEMQUIRA_ADMISSION_WRITE_MAX_IN_FLIGHT",
        "MEMQUIRA_ADMISSION_WRITE_MAX_QUEUE",
        "MEMQUIRA_ADMISSION_WRITE_DEADLINE_MS",
        "MEMQUIRA_ADMISSION_ADMIN_MAX_IN_FLIGHT",
        "MEMQUIRA_ADMISSION_ADMIN_MAX_QUEUE",
        "MEMQUIRA_ADMISSION_ADMIN_DEADLINE_MS",
        "MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE",
        "MEMQUIRA_LOG_FORMAT",
        "MEMQUIRA_LOG_LEVEL",
        "MEMQUIRA_REQUEST_ID_HEADER",
        "MEMQUIRA_METRICS_ENABLED",
        "MEMQUIRA_METRICS_PATH",
        "MEMQUIRA_METRICS_REQUIRE_AUTH",
        "MEMQUIRA_METRICS_INCLUDE_PROCESS",
        "MEMQUIRA_RETENTION_ENABLED",
        "MEMQUIRA_FACT_RETENTION_DAYS",
        "MEMQUIRA_EVENT_RETENTION_DAYS",
        "MEMQUIRA_CONTEXT_CACHE_ENABLED",
        "MEMQUIRA_CONTEXT_CACHE_BACKEND",
        "MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS",
        "MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS",
        "MEMQUIRA_CONTEXT_CACHE_MAX_ENTRIES",
        "MEMQUIRA_CONTEXT_CACHE_KEY_PREFIX",
        "MEMQUIRA_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED",
        "MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS",
        "MEMQUIRA_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED",
        "MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS",
        "MEMQUIRA_CONTEXT_CACHE_METRICS_ENABLED",
        "MEMQUIRA_PROVIDER_TIMEOUT_SECONDS",
        "MEMQUIRA_PROVIDER_MAX_RETRIES",
        "MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS",
        "MEMQUIRA_PROVIDER_MAX_BACKOFF_MS",
        "MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER",
        "MEMQUIRA_PROVIDER_RETRY_JITTER_ENABLED",
        "MEMQUIRA_PROVIDER_FALLBACK_ENABLED",
        "MEMQUIRA_LLM_FALLBACK_ORDER",
        "MEMQUIRA_EMBEDDING_FALLBACK_ORDER",
        "MEMQUIRA_SUMMARIZER_FALLBACK_ORDER",
        "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_ENABLED",
        "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD",
        "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS",
        "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS",
        "MEMQUIRA_PROVIDER_USAGE_METRICS_ENABLED",
        "MEMQUIRA_PROVIDER_COST_TRACKING_ENABLED",
        "MEMQUIRA_PROVIDER_USAGE_PERSISTENCE_ENABLED",
        "MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED",
        "MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS",
        "MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS",
        "MEMQUIRA_QUOTAS_ENABLED",
        "MEMQUIRA_MAX_USERS_PER_ORG",
        "MEMQUIRA_MAX_MEMORIES_PER_USER",
        "MEMQUIRA_MAX_MEMORIES_PER_ORG",
        "MEMQUIRA_DAILY_PROVIDER_REQUEST_LIMIT",
        "MEMQUIRA_DAILY_PROVIDER_TOKEN_LIMIT",
        "MEMQUIRA_PROVIDER_GUARDRAILS_ENABLED",
        "MEMQUIRA_REAL_PROVIDER_CALLS_ENABLED",
        "MEMQUIRA_PROVIDER_TEST_MODE",
        "MEMQUIRA_PROVIDER_MAX_CALLS_PER_RUN",
        "MEMQUIRA_PROVIDER_MAX_INPUT_CHARS",
        "MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS",
        "MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS",
        "MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS",
        "MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL",
        "MEMQUIRA_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS",
        "MEMQUIRA_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS",
        "MEMQUIRA_MULTI_PROVIDER_VALIDATION_ENABLED",
        "MEMQUIRA_MULTI_PROVIDER_VALIDATION_CONFIRMATION",
        "MEMQUIRA_BACKGROUND_JOBS_ENABLED",
        "MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS",
        "MEMQUIRA_BACKGROUND_JOB_ORG_IDS",
        "MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED",
        "MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS",
        "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_ENABLED",
        "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS",
        "MEMQUIRA_MEMORY_RETENTION_JOB_ENABLED",
        "MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS",
        "MEMQUIRA_BACKGROUND_JOB_HISTORY_ENABLED",
        "MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS",
        "MEMQUIRA_BACKGROUND_JOB_LOCK_ENABLED",
        "MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND",
        "MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS",
        "MEMQUIRA_BACKGROUND_JOB_LOCK_OWNER_ID",
        "MEMQUIRA_BACKGROUND_JOB_RETRIES_ENABLED",
        "MEMQUIRA_BACKGROUND_JOB_MAX_RETRIES",
        "MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS",
        "MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS",
        "MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER",
        "MEMQUIRA_BACKGROUND_JOB_RETRY_JITTER_ENABLED",
        "MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS",
        "MEMQUIRA_ALLOW_MOCK",
        "MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS",
        "MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS",
        "MEMQUIRA_REDIS_URL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ];

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        previous: Vec<(String, Option<OsString>)>,
    }

    /// Every key the tests manage, with its legacy `MEMCORE_*` alias (which `from_env` also reads).
    fn managed_keys() -> Vec<String> {
        ENV_KEYS
            .iter()
            .flat_map(|key| {
                [
                    Some((*key).to_string()),
                    memquira_common::env::legacy_name(key),
                ]
            })
            .flatten()
            .collect()
    }

    impl EnvGuard {
        fn new() -> Self {
            let previous = managed_keys()
                .into_iter()
                .map(|key| {
                    let value = std::env::var_os(&key);
                    (key, value)
                })
                .collect::<Vec<_>>();
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                match value {
                    // SAFETY: tests are serialized with a process-wide mutex, and values
                    // are restored only within that lock's lifetime.
                    Some(v) => unsafe { std::env::set_var(key, v) },
                    // SAFETY: same justification as above; serialized mutation in tests.
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }

    fn clear_env() {
        for key in managed_keys() {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    fn legacy_memcore_names_are_aliases_and_conflicts_are_refused() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMCORE_PORT", "9191") };
        let settings = Settings::from_env().expect("a legacy name alone should load");
        assert_eq!(settings.port, 9191);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_PORT", "9191") };
        let settings = Settings::from_env().expect("equal legacy and current values should load");
        assert_eq!(settings.port, 9191);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_PORT", "9292") };
        let error = Settings::from_env().expect_err("different values must be refused");
        let message = error.to_string();
        assert!(
            message.contains("MEMCORE_PORT and MEMQUIRA_PORT"),
            "{message}"
        );
        assert!(
            !message.contains("9292") && !message.contains("9191"),
            "{message}"
        );

        // SAFETY: as above.
        unsafe { std::env::remove_var("MEMCORE_PORT") };
        let settings = Settings::from_env().expect("the current name alone should load");
        assert_eq!(settings.port, 9292);
    }

    #[test]
    fn loads_defaults_when_env_is_missing() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("defaults should load");
        assert_eq!(settings.environment, Environment::Development);
        assert_eq!(settings.port, 8080);
        assert_eq!(settings.storage_mode, StorageMode::Embedded);
        assert_eq!(settings.vector_backend, VectorBackend::Qdrant);
        assert_eq!(settings.min_importance, 0.55);
        assert!(settings.enable_pii_redaction);
        assert!(settings.auth_enabled);
        assert_eq!(settings.dev_api_key, "memcore_dev_key");
        assert_eq!(settings.fact_backend, super::FactBackend::Sqlite);
        assert_eq!(settings.event_backend, super::EventBackend::Sqlite);
        assert!(settings.rate_limit_enabled);
        assert_eq!(settings.rate_limit_requests_per_minute, 60);
        assert_eq!(settings.log_format, super::LogFormat::Json);
        assert_eq!(settings.log_level, super::LogLevel::Info);
        assert_eq!(settings.request_id_header, super::DEFAULT_REQUEST_ID_HEADER);
        assert!(!settings.metrics_enabled);
        assert_eq!(settings.metrics_path, "/metrics");
        assert!(settings.metrics_require_auth);
        assert!(!settings.metrics_include_process);
        assert!(!settings.retention_enabled);
        assert_eq!(settings.fact_retention_days, 0);
        assert_eq!(settings.event_retention_days, 0);
        assert!(!settings.background_jobs_enabled);
        assert_eq!(settings.background_job_runner_interval_seconds, 60);
        assert!(settings.background_job_org_ids.is_empty());
        assert!(!settings.memory_usage_snapshot_job_enabled);
        assert_eq!(settings.memory_usage_snapshot_job_interval_seconds, 86_400);
        assert!(!settings.provider_usage_retention_job_enabled);
        assert_eq!(
            settings.provider_usage_retention_job_interval_seconds,
            86_400
        );
        assert!(!settings.memory_retention_job_enabled);
        assert_eq!(settings.memory_retention_job_interval_seconds, 86_400);
        assert!(settings.background_job_history_enabled);
        assert_eq!(settings.background_job_history_retention_days, 30);
        assert!(!settings.background_job_lock_enabled);
        assert_eq!(
            settings.background_job_lock_backend,
            super::BackgroundJobLockBackend::Database
        );
        assert_eq!(settings.background_job_lock_ttl_seconds, 300);
        assert!(settings.background_job_lock_owner_id.is_none());
        assert!(settings.background_job_retries_enabled);
        assert_eq!(settings.background_job_max_retries, 2);
        assert_eq!(settings.background_job_initial_backoff_ms, 500);
        assert_eq!(settings.background_job_max_backoff_ms, 5000);
        assert!((settings.background_job_backoff_multiplier - 2.0).abs() < f32::EPSILON);
        assert!(settings.background_job_retry_jitter_enabled);
        assert_eq!(settings.graceful_shutdown_timeout_seconds, 30);
        assert_eq!(settings.background_job_shutdown_timeout_seconds, 30);
        assert!(settings.database_migrations_enabled);
        assert_eq!(
            settings.database_migration_mode,
            super::DatabaseMigrationMode::Auto
        );
        assert!(settings.database_require_clean_migrations);
        assert!(!settings.backup_enabled);
        assert_eq!(settings.backup_dir, "./backups");
        assert_eq!(settings.backup_max_files, 10);
        assert!(!settings.restore_enabled);
        assert!(settings.security_headers_enabled);
        assert_eq!(
            settings.max_request_body_bytes,
            super::DEFAULT_MAX_REQUEST_BODY_BYTES
        );
        assert!(!settings.cors_enabled);
        assert!(settings.cors_allowed_origins.is_empty());
        assert_eq!(
            settings.cors_allowed_methods,
            vec!["GET", "POST", "PUT", "DELETE", "OPTIONS"]
        );
        assert_eq!(
            settings.cors_allowed_headers,
            vec![
                "Authorization",
                "Content-Type",
                "X-Organization-ID",
                "X-Memquira-Test-Source",
                "X-Memcore-Test-Source",
            ]
        );
        assert!(settings.cors_exposed_headers.is_empty());
        assert!(!settings.cors_allow_credentials);
    }

    #[test]
    fn security_settings_parse_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_SECURITY_HEADERS_ENABLED", "false");
            std::env::set_var("MEMQUIRA_MAX_REQUEST_BODY_BYTES", "2048");
            std::env::set_var("MEMQUIRA_CORS_ENABLED", "true");
            std::env::set_var(
                "MEMQUIRA_CORS_ALLOWED_ORIGINS",
                "https://app.example.com,https://admin.example.com",
            );
            std::env::set_var("MEMQUIRA_CORS_ALLOWED_METHODS", "GET,POST,OPTIONS");
            std::env::set_var(
                "MEMQUIRA_CORS_ALLOWED_HEADERS",
                "Authorization,Content-Type,X-Organization-ID,X-Request-ID",
            );
            std::env::set_var("MEMQUIRA_CORS_EXPOSED_HEADERS", "X-Request-ID");
            std::env::set_var("MEMQUIRA_CORS_ALLOW_CREDENTIALS", "true");
        }

        let settings = Settings::from_env().expect("security settings should load");
        assert!(!settings.security_headers_enabled);
        assert_eq!(settings.max_request_body_bytes, 2048);
        assert!(settings.cors_enabled);
        assert_eq!(
            settings.cors_allowed_origins,
            vec!["https://app.example.com", "https://admin.example.com"]
        );
        assert_eq!(
            settings.cors_allowed_methods,
            vec!["GET", "POST", "OPTIONS"]
        );
        assert_eq!(
            settings.cors_allowed_headers,
            vec![
                "Authorization",
                "Content-Type",
                "X-Organization-ID",
                "X-Request-ID"
            ]
        );
        assert_eq!(settings.cors_exposed_headers, vec!["X-Request-ID"]);
        assert!(settings.cors_allow_credentials);
    }

    #[test]
    fn zero_max_request_body_bytes_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_MAX_REQUEST_BODY_BYTES", "0");
        }

        let error = Settings::from_env().expect_err("zero body limit should fail");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_MAX_REQUEST_BODY_BYTES must be greater than 0")
        );
    }

    #[test]
    fn wildcard_cors_origin_rejected_when_credentials_enabled() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_CORS_ALLOW_CREDENTIALS", "true");
            std::env::set_var("MEMQUIRA_CORS_ALLOWED_ORIGINS", "*");
        }

        let error = Settings::from_env().expect_err("wildcard with credentials should fail");
        assert!(error.to_string().contains("cannot include *"));
    }

    #[test]
    fn invalid_cors_http_method_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_CORS_ALLOWED_METHODS", "GET,TRACE");
        }

        let error = Settings::from_env().expect_err("invalid method should fail");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_CORS_ALLOWED_METHODS value")
        );
    }

    #[test]
    fn invalid_empty_cors_header_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            // Comma-separated parser drops empties; force validation via exposed headers with spaces.
            std::env::set_var("MEMQUIRA_CORS_ALLOWED_HEADERS", "Authorization, Bad Header");
        }

        let error = Settings::from_env().expect_err("invalid header should fail");
        assert!(error.to_string().contains("Invalid CORS header name"));
    }

    #[test]
    fn settings_debug_output_redacts_secrets() {
        let settings = Settings {
            database_url: "postgres://user:dbpass@localhost/memcore".to_string(),
            openai_api_key: Some("sk-test-key".to_string()),
            api_key_pepper: Some("super-pepper-secret".to_string()),
            redis_url: Some("redis://:rpass@localhost:6379/0".to_string()),
            ..Settings::default()
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("dbpass"));
        assert!(!debug.contains("sk-test-key"));
        assert!(!debug.contains("super-pepper-secret"));
        assert!(!debug.contains("rpass"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn backup_settings_parse_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_BACKUP_ENABLED", "true");
            std::env::set_var("MEMQUIRA_BACKUP_DIR", "./tmp/backups");
            std::env::set_var("MEMQUIRA_BACKUP_MAX_FILES", "3");
            std::env::set_var("MEMQUIRA_RESTORE_ENABLED", "true");
        }

        let settings = Settings::from_env().expect("backup settings should load");
        assert!(settings.backup_enabled);
        assert_eq!(settings.backup_dir, "./tmp/backups");
        assert_eq!(settings.backup_max_files, 3);
        assert!(settings.restore_enabled);
    }

    #[test]
    fn empty_backup_dir_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_BACKUP_DIR", "   ");
        }

        let error = Settings::from_env().expect_err("empty backup dir should fail");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_BACKUP_DIR cannot be empty")
        );
    }

    #[test]
    fn zero_backup_max_files_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_BACKUP_MAX_FILES", "0");
        }

        let error = Settings::from_env().expect_err("zero backup max files should fail");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_BACKUP_MAX_FILES must be greater than 0")
        );
    }

    #[test]
    fn database_migration_settings_parse_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_DATABASE_MIGRATIONS_ENABLED", "false");
            std::env::set_var("MEMQUIRA_DATABASE_MIGRATION_MODE", "validate_only");
            std::env::set_var("MEMQUIRA_DATABASE_REQUIRE_CLEAN_MIGRATIONS", "false");
        }

        let settings = Settings::from_env().expect("migration settings should load");
        assert!(!settings.database_migrations_enabled);
        assert_eq!(
            settings.database_migration_mode,
            super::DatabaseMigrationMode::ValidateOnly
        );
        assert!(!settings.database_require_clean_migrations);
    }

    #[test]
    fn background_job_settings_parse_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_BACKGROUND_JOBS_ENABLED", "true");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS", "5");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_ORG_IDS", "org_a, org_b,,org_c");
            std::env::set_var("MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED", "true");
            std::env::set_var("MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS", "10");
            std::env::set_var("MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_ENABLED", "true");
            std::env::set_var(
                "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS",
                "20",
            );
            std::env::set_var("MEMQUIRA_MEMORY_RETENTION_JOB_ENABLED", "true");
            std::env::set_var("MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS", "30");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_HISTORY_ENABLED", "false");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS", "45");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_LOCK_ENABLED", "true");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND", "database");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS", "120");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_LOCK_OWNER_ID", "instance-a");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_RETRIES_ENABLED", "false");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_MAX_RETRIES", "0");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS", "100");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS", "500");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER", "1.5");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_RETRY_JITTER_ENABLED", "false");
            std::env::set_var("MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS", "20");
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS", "15");
        }

        let settings = Settings::from_env().expect("background job settings should load");
        assert!(settings.background_jobs_enabled);
        assert_eq!(settings.background_job_runner_interval_seconds, 5);
        assert_eq!(
            settings.background_job_org_ids,
            vec![
                "org_a".to_string(),
                "org_b".to_string(),
                "org_c".to_string()
            ]
        );
        assert!(settings.memory_usage_snapshot_job_enabled);
        assert_eq!(settings.memory_usage_snapshot_job_interval_seconds, 10);
        assert!(settings.provider_usage_retention_job_enabled);
        assert_eq!(settings.provider_usage_retention_job_interval_seconds, 20);
        assert!(settings.memory_retention_job_enabled);
        assert_eq!(settings.memory_retention_job_interval_seconds, 30);
        assert!(!settings.background_job_history_enabled);
        assert_eq!(settings.background_job_history_retention_days, 45);
        assert!(settings.background_job_lock_enabled);
        assert_eq!(
            settings.background_job_lock_backend,
            super::BackgroundJobLockBackend::Database
        );
        assert_eq!(settings.background_job_lock_ttl_seconds, 120);
        assert_eq!(
            settings.background_job_lock_owner_id.as_deref(),
            Some("instance-a")
        );
        assert!(!settings.background_job_retries_enabled);
        assert_eq!(settings.background_job_max_retries, 0);
        assert_eq!(settings.background_job_initial_backoff_ms, 100);
        assert_eq!(settings.background_job_max_backoff_ms, 500);
        assert!((settings.background_job_backoff_multiplier - 1.5).abs() < f32::EPSILON);
        assert!(!settings.background_job_retry_jitter_enabled);
        assert_eq!(settings.graceful_shutdown_timeout_seconds, 20);
        assert_eq!(settings.background_job_shutdown_timeout_seconds, 15);
    }

    #[test]
    fn zero_background_job_intervals_fail_validation() {
        for key in [
            "MEMQUIRA_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS",
            "MEMQUIRA_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS",
            "MEMQUIRA_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS",
            "MEMQUIRA_MEMORY_RETENTION_JOB_INTERVAL_SECONDS",
            "MEMQUIRA_BACKGROUND_JOB_LOCK_TTL_SECONDS",
            "MEMQUIRA_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS",
            "MEMQUIRA_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS",
        ] {
            let _lock = env_test_lock()
                .lock()
                .expect("env test lock should not be poisoned");
            let _guard = EnvGuard::new();
            clear_env();

            unsafe {
                std::env::set_var(key, "0");
            }

            let error = Settings::from_env().expect_err("zero interval should fail");
            assert!(
                error.to_string().contains("must be greater than 0"),
                "unexpected error for {key}: {error}"
            );
        }
    }

    #[test]
    fn invalid_background_job_retry_config_fails_validation() {
        for (key, value, expected) in [
            (
                "MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS",
                "0",
                "MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS must be greater than 0",
            ),
            (
                "MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS",
                "100",
                "MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS must be >= MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS",
            ),
            (
                "MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER",
                "0.5",
                "MEMQUIRA_BACKGROUND_JOB_BACKOFF_MULTIPLIER must be >= 1.0",
            ),
        ] {
            let _lock = env_test_lock()
                .lock()
                .expect("env test lock should not be poisoned");
            let _guard = EnvGuard::new();
            clear_env();

            unsafe {
                std::env::set_var("MEMQUIRA_BACKGROUND_JOB_INITIAL_BACKOFF_MS", "500");
                std::env::set_var("MEMQUIRA_BACKGROUND_JOB_MAX_BACKOFF_MS", "5000");
                std::env::set_var(key, value);
            }

            let error = Settings::from_env().expect_err("invalid retry config should fail");
            assert!(
                error.to_string().contains(expected),
                "unexpected error for {key}: {error}"
            );
        }
    }

    #[test]
    fn invalid_background_job_lock_backend_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND", "redis");
        }

        let error = Settings::from_env().expect_err("invalid backend should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_BACKGROUND_JOB_LOCK_BACKEND value")
        );
    }

    #[test]
    fn context_cache_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.context_cache_enabled);
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Disabled
        );
        assert_eq!(settings.context_cache_ttl_seconds, 300);
        assert_eq!(settings.context_cache_max_entries, 1000);
        assert_eq!(
            settings.context_cache_key_prefix,
            super::DEFAULT_CONTEXT_CACHE_KEY_PREFIX
        );
    }

    #[test]
    fn context_default_packing_is_prefix_and_parses_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_ENV", "development");
        }
        let settings = Settings::from_env().expect("defaults load");
        assert_eq!(
            settings.context_default_packing,
            super::ContextPacking::Prefix
        );
        assert_eq!(
            Settings::default().context_default_packing,
            super::ContextPacking::Prefix
        );
        for (value, want) in [
            ("prefix", super::ContextPacking::Prefix),
            (" Fill ", super::ContextPacking::Fill),
            ("evidence", super::ContextPacking::Evidence),
        ] {
            // SAFETY: as above.
            unsafe {
                std::env::set_var("MEMQUIRA_CONTEXT_DEFAULT_PACKING", value);
            }
            let settings = Settings::from_env().expect("valid packing loads");
            assert_eq!(settings.context_default_packing, want, "{value}");
            assert_eq!(
                settings.context_default_packing.as_str(),
                value.trim().to_ascii_lowercase()
            );
        }
        // SAFETY: as above.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_DEFAULT_PACKING", "greedy");
        }
        let err = Settings::from_env().expect_err("an unknown packing is refused at startup");
        assert!(
            err.to_string().contains("MEMQUIRA_CONTEXT_DEFAULT_PACKING"),
            "{err}"
        );
    }

    #[test]
    fn context_cache_memory_backend_parses_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
        }

        let settings = Settings::from_env().expect("memory backend should load");
        assert!(settings.context_cache_enabled);
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Memory
        );
    }

    #[test]
    fn context_cache_disabled_backend_parses_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled");
        }

        let settings = Settings::from_env().expect("disabled backend should load");
        assert!(!settings.context_cache_enabled);
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Disabled
        );
    }

    #[test]
    fn context_cache_redis_backend_is_refused_even_with_a_url() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "redis");
            std::env::set_var("MEMQUIRA_REDIS_URL", "redis://localhost:6379");
        }

        let error = Settings::from_env().expect_err("redis backend must be refused");
        assert_eq!(error.code(), "validation_error");
        let message = error.to_string();
        assert!(message.contains("MEMQUIRA_CONTEXT_CACHE_BACKEND=redis is not supported"));
        assert!(message.contains("MEMQUIRA_CONTEXT_CACHE_BACKEND=disabled"));
        assert!(!message.contains("localhost:6379"));
    }

    #[test]
    fn context_cache_redis_backend_without_url_is_refused_by_the_policy_first() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "redis");
        }

        let error = Settings::from_env().expect_err("redis without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_CONTEXT_CACHE_BACKEND=redis is not supported")
        );
    }

    #[test]
    fn fails_on_invalid_context_cache_backend() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "not-a-backend");
        }

        let error = Settings::from_env().expect_err("invalid backend should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_CONTEXT_CACHE_BACKEND value")
        );
    }

    #[test]
    fn legacy_context_cache_enabled_env_selects_memory_backend() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_ENABLED", "true");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
        }

        let settings = Settings::from_env().expect("legacy enabled flag should load");
        assert!(settings.context_cache_enabled);
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Memory
        );
    }

    /// `Settings::from_env` with exactly `vars` set (others cleared).
    fn cache_gate_from_env(vars: &[(&str, &str)]) -> super::MemquiraResult<Settings> {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
        }
        Settings::from_env()
    }

    const LOCAL_MEMORY: &[(&str, &str)] = &[
        ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory"),
        ("MEMQUIRA_ENV", "development"),
        ("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
    ];

    fn with(
        base: &[(&'static str, &'static str)],
        extra: &[(&'static str, &'static str)],
    ) -> Vec<(&'static str, &'static str)> {
        let mut vars: Vec<(&str, &str)> = base
            .iter()
            .filter(|(key, _)| !extra.iter().any(|(k, _)| k == key))
            .copied()
            .collect();
        vars.extend_from_slice(extra);
        vars
    }

    fn refused(result: super::MemquiraResult<Settings>, expected: &str) -> String {
        let error = result.expect_err("configuration must be refused");
        assert_eq!(error.code(), "validation_error");
        let message = error.to_string();
        assert!(message.contains(expected), "{message}");
        message
    }

    #[test]
    fn cache_gate_disabled_is_allowed_in_every_mode() {
        for vars in [
            vec![],
            vec![("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled")],
            vec![("MEMQUIRA_CONTEXT_CACHE_BACKEND", " Disabled ")],
            vec![("MEMQUIRA_CONTEXT_CACHE_ENABLED", "false")],
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMQUIRA_CONTEXT_CACHE_ENABLED", "false"),
            ],
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMQUIRA_ENV", "production"),
            ],
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMQUIRA_STORAGE_MODE", "production"),
            ],
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
            ],
        ] {
            let settings = cache_gate_from_env(&vars).expect("disabled cache must load");
            assert_eq!(
                settings.context_cache_backend,
                super::ContextCacheBackend::Disabled,
                "{vars:?}"
            );
            assert!(!settings.context_cache_enabled);
            settings
                .check_context_cache_policy()
                .expect("disabled passes the policy");
        }
    }

    #[test]
    fn cache_gate_redis_is_refused_in_every_mode_and_spelling() {
        for vars in [
            vec![("MEMQUIRA_CONTEXT_CACHE_BACKEND", "redis")],
            vec![("MEMQUIRA_CONTEXT_CACHE_BACKEND", " REDIS ")],
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "redis"),
                (
                    "MEMQUIRA_REDIS_URL",
                    "redis://:s3cret-pass@cache.internal:6379/0",
                ),
                ("MEMQUIRA_ENV", "development"),
                ("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
            ],
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "redis"),
                (
                    "MEMQUIRA_REDIS_URL",
                    "redis://:s3cret-pass@cache.internal:6379/0",
                ),
                ("MEMQUIRA_ENV", "production"),
            ],
        ] {
            let message = refused(
                cache_gate_from_env(&vars),
                "MEMQUIRA_CONTEXT_CACHE_BACKEND=redis is not supported",
            );
            assert!(message.contains("set MEMQUIRA_CONTEXT_CACHE_BACKEND=disabled"));
            assert!(!message.contains("s3cret-pass") && !message.contains("cache.internal"));
        }
    }

    #[test]
    fn cache_gate_memory_needs_the_local_opt_in_and_an_explicit_development_env() {
        let settings = cache_gate_from_env(LOCAL_MEMORY).expect("local memory opt-in must load");
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Memory
        );
        assert!(settings.context_cache_local_single_process);
        // `dev` alias, case and whitespace, the other accepted true spellings.
        for (env, ack) in [("dev", "1"), (" Development ", " YES "), ("DEV", "true")] {
            cache_gate_from_env(&with(
                LOCAL_MEMORY,
                &[
                    ("MEMQUIRA_ENV", env),
                    ("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", ack),
                ],
            ))
            .expect("accepted spellings");
        }
        // SQLite (embedded) is the supported local store.
        cache_gate_from_env(&with(
            LOCAL_MEMORY,
            &[
                ("MEMQUIRA_FACT_BACKEND", "sqlite"),
                ("MEMQUIRA_EVENT_BACKEND", "sqlite"),
                ("MEMQUIRA_DATABASE_URL", "sqlite::memory:"),
            ],
        ))
        .expect("local sqlite memory cache");

        // CG5: no opt-in / opt-in false.
        let without: Vec<_> = LOCAL_MEMORY
            .iter()
            .filter(|(k, _)| *k != "MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS")
            .copied()
            .collect();
        refused(
            cache_gate_from_env(&without),
            "needs MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
        );
        refused(
            cache_gate_from_env(&with(
                LOCAL_MEMORY,
                &[("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "false")],
            )),
            "needs MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
        );
        // CG7 (ambiguous): MEMQUIRA_ENV unset, even with the opt-in.
        let ambiguous: Vec<_> = LOCAL_MEMORY
            .iter()
            .filter(|(k, _)| *k != "MEMQUIRA_ENV")
            .copied()
            .collect();
        refused(
            cache_gate_from_env(&ambiguous),
            "needs MEMQUIRA_ENV set explicitly to development",
        );
    }

    #[test]
    fn cache_gate_memory_is_refused_with_any_production_marker_despite_the_opt_in() {
        for (extra, marker) in [
            (
                vec![("MEMQUIRA_ENV", "production")],
                "MEMQUIRA_ENV=production",
            ),
            (vec![("MEMQUIRA_ENV", "prod")], "MEMQUIRA_ENV=production"),
            (
                vec![("MEMQUIRA_STORAGE_MODE", "production")],
                "MEMQUIRA_STORAGE_MODE=production",
            ),
            (
                vec![
                    ("MEMQUIRA_FACT_BACKEND", "postgres"),
                    (
                        "MEMQUIRA_POSTGRES_URL",
                        "postgres://user:pg-pass@db.internal/memcore",
                    ),
                ],
                "MEMQUIRA_FACT_BACKEND=postgres",
            ),
            (
                vec![
                    ("MEMQUIRA_EVENT_BACKEND", "postgres"),
                    (
                        "MEMQUIRA_POSTGRES_URL",
                        "postgres://user:pg-pass@db.internal/memcore",
                    ),
                ],
                "MEMQUIRA_EVENT_BACKEND=postgres",
            ),
        ] {
            let message = refused(cache_gate_from_env(&with(LOCAL_MEMORY, &extra)), marker);
            assert!(message.contains("process-local cache"), "{message}");
            assert!(!message.contains("pg-pass") && !message.contains("db.internal"));
        }
        // The legacy flag selects memory and is held to the same policy.
        refused(
            cache_gate_from_env(&[
                ("MEMQUIRA_CONTEXT_CACHE_ENABLED", "true"),
                ("MEMQUIRA_ENV", "production"),
                ("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
            ]),
            "MEMQUIRA_ENV=production",
        );
        refused(
            cache_gate_from_env(&[
                ("MEMQUIRA_CONTEXT_CACHE_ENABLED", "true"),
                ("MEMQUIRA_ENV", "development"),
            ]),
            "needs MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
        );
    }

    #[test]
    fn cache_gate_unknown_malformed_and_contradictory_values_are_refused() {
        refused(
            cache_gate_from_env(&[("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memcached")]),
            "Invalid MEMQUIRA_CONTEXT_CACHE_BACKEND value",
        );
        refused(
            cache_gate_from_env(&with(
                LOCAL_MEMORY,
                &[("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "sure")],
            )),
            "MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS must be one of",
        );
        refused(
            cache_gate_from_env(&[("MEMQUIRA_CONTEXT_CACHE_ENABLED", "maybe")]),
            "MEMQUIRA_CONTEXT_CACHE_ENABLED must be one of",
        );
        for vars in [
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMQUIRA_CONTEXT_CACHE_ENABLED", "true"),
            ],
            with(LOCAL_MEMORY, &[("MEMQUIRA_CONTEXT_CACHE_ENABLED", "false")]),
            vec![
                ("MEMQUIRA_CONTEXT_CACHE_BACKEND", "redis"),
                ("MEMQUIRA_CONTEXT_CACHE_ENABLED", "false"),
            ],
        ] {
            refused(
                cache_gate_from_env(&vars),
                "MEMQUIRA_CONTEXT_CACHE_ENABLED contradicts MEMQUIRA_CONTEXT_CACHE_BACKEND",
            );
        }
        // Agreeing values are fine.
        cache_gate_from_env(&with(
            LOCAL_MEMORY,
            &[("MEMQUIRA_CONTEXT_CACHE_ENABLED", "true")],
        ))
        .expect("agreeing legacy flag");
    }

    #[test]
    fn cache_gate_policy_applies_to_settings_built_in_code() {
        let redis = Settings {
            context_cache_enabled: true,
            context_cache_backend: super::ContextCacheBackend::Redis,
            redis_url: Some("redis://:s3cret@cache.internal:6379".to_string()),
            ..Settings::default()
        };
        let message = redis
            .check_context_cache_policy()
            .expect_err("redis refused")
            .to_string();
        assert!(!message.contains("s3cret"));
        let memory = Settings {
            context_cache_enabled: true,
            context_cache_backend: super::ContextCacheBackend::Memory,
            ..Settings::default()
        };
        assert!(memory.check_context_cache_policy().is_err());
        let production = Settings {
            environment: Environment::Production,
            context_cache_local_single_process: true,
            ..memory.clone()
        };
        assert!(production.check_context_cache_policy().is_err());
        let local = Settings {
            context_cache_local_single_process: true,
            ..memory
        };
        local.check_context_cache_policy().expect("local opt-in");
    }

    #[test]
    fn stampede_protection_defaults_to_enabled_when_cache_configured() {
        let settings = Settings {
            context_cache_enabled: true,
            context_cache_backend: super::ContextCacheBackend::Memory,
            ..Settings::default()
        };
        assert!(settings.context_cache_stampede_protection_enabled);
        assert_eq!(settings.context_cache_lock_timeout_seconds, 30);
    }

    #[test]
    fn loads_stampede_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var(
                "MEMQUIRA_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED",
                "false",
            );
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS", "45");
        }

        let settings = Settings::from_env().expect("stampede settings should load");
        assert!(!settings.context_cache_stampede_protection_enabled);
        assert_eq!(settings.context_cache_lock_timeout_seconds, 45);
    }

    #[test]
    fn stale_while_revalidate_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.context_cache_stale_while_revalidate_enabled);
        assert_eq!(settings.context_cache_stale_ttl_seconds, 120);
    }

    #[test]
    fn loads_swr_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var(
                "MEMQUIRA_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED",
                "true",
            );
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS", "90");
        }

        let settings = Settings::from_env().expect("swr settings should load");
        assert!(settings.context_cache_stale_while_revalidate_enabled);
        assert_eq!(settings.context_cache_stale_ttl_seconds, 90);
    }

    #[test]
    fn zero_stale_ttl_with_swr_enabled_fails_when_cache_enabled() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var(
                "MEMQUIRA_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED",
                "true",
            );
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS", "0");
        }

        let error = Settings::from_env().expect_err("zero stale ttl should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_CONTEXT_CACHE_STALE_TTL_SECONDS must be greater than 0")
        );
    }

    #[test]
    fn zero_lock_timeout_with_stampede_enabled_fails_when_cache_enabled() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED", "true");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS", "0");
        }

        let error = Settings::from_env().expect_err("zero lock timeout should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS must be greater than 0")
        );
    }

    #[test]
    fn provider_policy_defaults_are_expected() {
        let settings = Settings::default();
        assert_eq!(settings.provider_timeout_seconds, 30);
        assert_eq!(settings.provider_max_retries, 2);
        assert!(!settings.provider_fallback_enabled);
        assert!(settings.provider_circuit_breaker_enabled);
        assert!(settings.provider_usage_metrics_enabled);
        assert!(!settings.provider_cost_tracking_enabled);
        assert_eq!(settings.provider_circuit_breaker_failure_threshold, 5);
    }

    #[test]
    fn loads_provider_policy_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_TIMEOUT_SECONDS", "45");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_RETRIES", "0");
            std::env::set_var("MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS", "100");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_BACKOFF_MS", "500");
            std::env::set_var("MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER", "1.5");
            std::env::set_var("MEMQUIRA_PROVIDER_RETRY_JITTER_ENABLED", "false");
        }

        let settings = Settings::from_env().expect("provider policy should load");
        assert_eq!(settings.provider_timeout_seconds, 45);
        assert_eq!(settings.provider_max_retries, 0);
        assert_eq!(settings.provider_initial_backoff_ms, 100);
        assert_eq!(settings.provider_max_backoff_ms, 500);
        assert!((settings.provider_backoff_multiplier - 1.5).abs() < f32::EPSILON);
        assert!(!settings.provider_retry_jitter_enabled);
    }

    #[test]
    fn fallback_and_circuit_breaker_settings_load_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_FALLBACK_ENABLED", "true");
            std::env::set_var("MEMQUIRA_LLM_FALLBACK_ORDER", "mock,openai");
            std::env::set_var("MEMQUIRA_EMBEDDING_FALLBACK_ORDER", "mock");
            std::env::set_var("MEMQUIRA_SUMMARIZER_FALLBACK_ORDER", "mock");
            std::env::set_var("MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_ENABLED", "false");
            std::env::set_var("MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD", "3");
            std::env::set_var(
                "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS",
                "30",
            );
            std::env::set_var("MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS", "2");
        }

        let settings = Settings::from_env().expect("fallback/circuit settings should load");
        assert!(settings.provider_fallback_enabled);
        assert_eq!(
            settings.llm_fallback_order,
            vec!["mock".to_string(), "openai".to_string()]
        );
        assert!(!settings.provider_circuit_breaker_enabled);
        assert_eq!(settings.provider_circuit_breaker_failure_threshold, 3);
        assert_eq!(settings.provider_circuit_breaker_reset_timeout_seconds, 30);
        assert_eq!(settings.provider_circuit_breaker_half_open_max_calls, 2);
    }

    #[test]
    fn invalid_circuit_breaker_values_fail_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD", "0");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var(
                "MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS",
                "0",
            );
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS", "0");
        }
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn invalid_provider_backoff_values_fail_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_TIMEOUT_SECONDS", "0");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS", "0");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_INITIAL_BACKOFF_MS", "500");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_BACKOFF_MS", "100");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_BACKOFF_MULTIPLIER", "0.5");
        }
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn redis_password_not_exposed_in_sanitized_url_output() {
        use memquira_common::sanitize_redis_url_for_display;

        let sanitized = sanitize_redis_url_for_display("redis://:super_secret@localhost:6379/0");
        assert!(!sanitized.contains("super_secret"));
        assert!(sanitized.contains("***@"));
    }

    #[test]
    fn loads_context_cache_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMQUIRA_ENV", "development");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_TTL_SECONDS", "120");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_MAX_ENTRIES", "50");
            std::env::set_var("MEMQUIRA_CONTEXT_CACHE_KEY_PREFIX", "custom");
        }

        let settings = Settings::from_env().expect("context cache settings should load");
        assert!(settings.context_cache_enabled);
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Memory
        );
        assert_eq!(settings.context_cache_ttl_seconds, 120);
        assert_eq!(settings.context_cache_max_entries, 50);
        assert_eq!(settings.context_cache_key_prefix, "custom");
    }

    #[test]
    fn retention_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.retention_enabled);
        assert_eq!(settings.fact_retention_days, 0);
        assert_eq!(settings.event_retention_days, 0);
    }

    #[test]
    fn loads_retention_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_RETENTION_ENABLED", "true");
            std::env::set_var("MEMQUIRA_FACT_RETENTION_DAYS", "365");
            std::env::set_var("MEMQUIRA_EVENT_RETENTION_DAYS", "90");
        }

        let settings = Settings::from_env().expect("retention settings should load");
        assert!(settings.retention_enabled);
        assert_eq!(settings.fact_retention_days, 365);
        assert_eq!(settings.event_retention_days, 90);
    }

    #[test]
    fn fails_on_negative_retention_days() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_FACT_RETENTION_DAYS", "-1");
        }

        let error = Settings::from_env().expect_err("negative retention days should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_FACT_RETENTION_DAYS must be a valid unsigned integer")
        );
    }

    #[test]
    fn provider_usage_retention_days_default_is_180() {
        let settings = Settings::default();
        assert_eq!(settings.provider_usage_retention_days, 180);
    }

    #[test]
    fn provider_usage_retention_zero_disables_cleanup() {
        let settings = Settings {
            provider_usage_retention_days: 0,
            ..Settings::default()
        };
        assert_eq!(settings.provider_usage_retention_days, 0);
    }

    #[test]
    fn loads_provider_usage_retention_days_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS", "90");
        }

        let settings = Settings::from_env().expect("provider usage retention should load");
        assert_eq!(settings.provider_usage_retention_days, 90);
    }

    #[test]
    fn fails_on_negative_provider_usage_retention_days() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS", "-1");
        }

        let error =
            Settings::from_env().expect_err("negative provider usage retention days should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error.to_string().contains(
                "MEMQUIRA_PROVIDER_USAGE_RETENTION_DAYS must be a valid unsigned integer"
            )
        );
    }

    #[test]
    fn background_job_history_retention_zero_disables_cleanup() {
        let settings = Settings {
            background_job_history_retention_days: 0,
            ..Settings::default()
        };
        assert_eq!(settings.background_job_history_retention_days, 0);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn fails_on_negative_background_job_history_retention_days() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS", "-1");
        }

        let error = Settings::from_env()
            .expect_err("negative background job history retention days should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains(
            "MEMQUIRA_BACKGROUND_JOB_HISTORY_RETENTION_DAYS must be a valid unsigned integer"
        ));
    }

    #[test]
    fn quotas_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.quotas_enabled);
        assert_eq!(settings.max_users_per_org, 0);
        assert_eq!(settings.max_memories_per_user, 0);
        assert_eq!(settings.max_memories_per_org, 0);
        assert_eq!(settings.daily_provider_request_limit, 0);
        assert_eq!(settings.daily_provider_token_limit, 0);
    }

    #[test]
    fn quota_zero_limits_parse_as_unlimited() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_QUOTAS_ENABLED", "true");
            std::env::set_var("MEMQUIRA_MAX_USERS_PER_ORG", "0");
            std::env::set_var("MEMQUIRA_MAX_MEMORIES_PER_USER", "0");
            std::env::set_var("MEMQUIRA_MAX_MEMORIES_PER_ORG", "0");
            std::env::set_var("MEMQUIRA_DAILY_PROVIDER_REQUEST_LIMIT", "0");
            std::env::set_var("MEMQUIRA_DAILY_PROVIDER_TOKEN_LIMIT", "0");
        }

        let settings = Settings::from_env().expect("quota settings should load");
        assert!(settings.quotas_enabled);
        assert_eq!(settings.max_users_per_org, 0);
        assert_eq!(settings.max_memories_per_user, 0);
        assert_eq!(settings.max_memories_per_org, 0);
        assert_eq!(settings.daily_provider_request_limit, 0);
        assert_eq!(settings.daily_provider_token_limit, 0);
    }

    #[test]
    fn positive_quota_limits_parse_correctly() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_QUOTAS_ENABLED", "true");
            std::env::set_var("MEMQUIRA_MAX_USERS_PER_ORG", "10");
            std::env::set_var("MEMQUIRA_MAX_MEMORIES_PER_USER", "20");
            std::env::set_var("MEMQUIRA_MAX_MEMORIES_PER_ORG", "30");
            std::env::set_var("MEMQUIRA_DAILY_PROVIDER_REQUEST_LIMIT", "40");
            std::env::set_var("MEMQUIRA_DAILY_PROVIDER_TOKEN_LIMIT", "50");
        }

        let settings = Settings::from_env().expect("quota settings should load");
        assert!(settings.quotas_enabled);
        assert_eq!(settings.max_users_per_org, 10);
        assert_eq!(settings.max_memories_per_user, 20);
        assert_eq!(settings.max_memories_per_org, 30);
        assert_eq!(settings.daily_provider_request_limit, 40);
        assert_eq!(settings.daily_provider_token_limit, 50);
    }

    #[test]
    fn negative_quota_limit_fails_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        unsafe {
            std::env::set_var("MEMQUIRA_MAX_MEMORIES_PER_ORG", "-1");
        }

        let error = Settings::from_env().expect_err("negative quota should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_MAX_MEMORIES_PER_ORG must be a valid unsigned integer")
        );
    }

    #[test]
    fn default_struct_uses_mock_backends() {
        let settings = Settings::default();
        assert_eq!(settings.fact_backend, super::FactBackend::Mock);
        assert_eq!(settings.event_backend, super::EventBackend::Mock);
        assert_eq!(settings.vector_backend, super::VectorBackend::Mock);
        assert_eq!(settings.auth_mode, super::AuthMode::Dev);
    }

    #[test]
    fn sqlite_memory_settings_use_in_memory_database() {
        let settings = Settings::sqlite_memory();
        assert_eq!(settings.fact_backend, super::FactBackend::Sqlite);
        assert_eq!(settings.event_backend, super::EventBackend::Sqlite);
        assert!(settings.database_url.contains(":memory:"));
    }

    #[test]
    fn event_backend_defaults_to_match_fact_backend() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_FACT_BACKEND", "postgres");
            std::env::set_var("MEMQUIRA_POSTGRES_URL", "postgres://localhost:5432/memcore");
        }

        let settings = Settings::from_env().expect("settings should load");
        assert_eq!(settings.fact_backend, super::FactBackend::Postgres);
        assert_eq!(settings.event_backend, super::EventBackend::Postgres);
    }

    #[test]
    fn database_auth_mode_requires_api_key_pepper() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_AUTH_MODE", "database");
        }

        let error = Settings::from_env().expect_err("database auth without pepper should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_API_KEY_PEPPER is required when MEMQUIRA_AUTH_MODE=database")
        );
    }

    #[test]
    fn fails_on_invalid_auth_mode() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_AUTH_MODE", "invalid-mode");
        }

        let error = Settings::from_env().expect_err("invalid auth mode should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_AUTH_MODE value")
        );
    }

    #[test]
    fn loads_auth_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_AUTH_ENABLED", "false");
            std::env::set_var("MEMQUIRA_DEV_API_KEY", "custom_dev_key");
        }

        let settings = Settings::from_env().expect("auth settings should load");
        assert!(!settings.auth_enabled);
        assert_eq!(settings.dev_api_key, "custom_dev_key");
    }

    #[test]
    fn loads_custom_values_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_ENV", "production");
            std::env::set_var("MEMQUIRA_PORT", "9090");
            std::env::set_var("MEMQUIRA_STORAGE_MODE", "production");
            std::env::set_var("MEMQUIRA_VECTOR_BACKEND", "qdrant");
            std::env::set_var("MEMQUIRA_FACT_BACKEND", "postgres");
            std::env::set_var("MEMQUIRA_POSTGRES_URL", "postgres://localhost:5432/memcore");
            std::env::set_var("MEMQUIRA_MIN_IMPORTANCE", "0.8");
            std::env::set_var("MEMQUIRA_ENABLE_PII_REDACTION", "false");
        }

        let settings = Settings::from_env().expect("custom settings should load");
        assert_eq!(settings.environment, Environment::Production);
        assert_eq!(settings.port, 9090);
        assert_eq!(settings.storage_mode, StorageMode::Production);
        assert_eq!(settings.vector_backend, VectorBackend::Qdrant);
        assert_eq!(settings.min_importance, 0.8);
        assert!(!settings.enable_pii_redaction);
    }

    #[test]
    fn lancedb_remains_selectable_after_the_default_moved_to_qdrant() {
        // Qdrant is the default; `lancedb` still parses and validates as a setting (the server
        // refuses it at startup because that backend is no longer built).
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_VECTOR_BACKEND", "lancedb");
        }

        let settings = Settings::from_env().expect("lancedb should still load");
        assert_eq!(settings.vector_backend, VectorBackend::LanceDb);
        settings
            .validate()
            .expect("lancedb needs no extra configuration");
    }

    #[test]
    fn extraction_output_limit_defaults_to_the_provider_ceiling() {
        // Extraction used to hardcode 300 and ignore MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS entirely.
        let settings = Settings::default();
        assert_eq!(
            settings.provider_max_output_tokens,
            super::DEFAULT_PROVIDER_MAX_OUTPUT_TOKENS
        );
        assert_eq!(settings.effective_extraction_max_output_tokens(), 2_048);
        assert_eq!(settings.effective_classification_max_output_tokens(), 300);
        settings.validate().expect("defaults must validate");
    }

    #[test]
    fn lowering_the_ceiling_lowers_extraction_and_caps_classification() {
        // Backward compatible with deployments that pinned the ceiling: an unset extraction limit
        // follows it, and classification never exceeds it.
        let settings = Settings {
            provider_max_output_tokens: 256,
            ..Settings::default()
        };
        assert_eq!(settings.effective_extraction_max_output_tokens(), 256);
        assert_eq!(settings.effective_classification_max_output_tokens(), 256);
        settings
            .validate()
            .expect("a lower ceiling alone must validate");
    }

    #[test]
    fn extraction_and_classification_limits_are_set_independently() {
        let settings = Settings {
            extraction_max_output_tokens: Some(1_500),
            classification_max_output_tokens: Some(120),
            ..Settings::default()
        };
        assert_eq!(settings.effective_extraction_max_output_tokens(), 1_500);
        assert_eq!(settings.effective_classification_max_output_tokens(), 120);
        settings
            .validate()
            .expect("limits within the ceiling must validate");
    }

    #[test]
    fn an_explicit_limit_above_the_ceiling_is_rejected_rather_than_clamped() {
        // Silently clamping an extraction limit is how responses got truncated in the first place.
        for settings in [
            Settings {
                provider_max_output_tokens: 1_000,
                extraction_max_output_tokens: Some(4_000),
                ..Settings::default()
            },
            Settings {
                provider_max_output_tokens: 1_000,
                classification_max_output_tokens: Some(4_000),
                ..Settings::default()
            },
        ] {
            let error = settings
                .validate()
                .expect_err("above the ceiling must fail");
            assert!(
                error
                    .to_string()
                    .contains("must not exceed MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_zero_per_operation_limit_is_rejected() {
        let settings = Settings {
            extraction_max_output_tokens: Some(0),
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn per_operation_output_limits_load_from_the_environment() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS", "1800");
            std::env::set_var("MEMQUIRA_CLASSIFICATION_MAX_OUTPUT_TOKENS", "150");
        }

        let settings = Settings::from_env().expect("settings should load");
        assert_eq!(settings.extraction_max_output_tokens, Some(1_800));
        assert_eq!(settings.classification_max_output_tokens, Some(150));
        assert_eq!(settings.effective_extraction_max_output_tokens(), 1_800);

        unsafe {
            std::env::set_var("MEMQUIRA_EXTRACTION_MAX_OUTPUT_TOKENS", "lots");
        }
        assert!(
            Settings::from_env().is_err(),
            "a non-numeric limit must fail"
        );
    }

    #[test]
    fn default_vector_backend_is_qdrant_and_validates_without_extra_config() {
        // Configuration alone must not block startup: the Qdrant URL and
        // collection defaults are non-empty, so validation passes with no env
        // set. Actually constructing the store still needs the `qdrant` cargo
        // feature — as the previous `lancedb` default needed its own feature —
        // so a default build without either still has to pick a backend it was
        // compiled with (e.g. MEMQUIRA_VECTOR_BACKEND=mock).
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("defaults should load");
        assert_eq!(settings.vector_backend, VectorBackend::Qdrant);
        settings
            .validate()
            .expect("default qdrant settings should validate");
    }

    #[test]
    fn fails_on_invalid_port() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_PORT", "not-a-port") };

        let error = Settings::from_env().expect_err("invalid port should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_PORT must be a valid u16 port")
        );
    }

    #[test]
    fn fails_on_invalid_min_importance() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_MIN_IMPORTANCE", "1.5") };

        let error = Settings::from_env().expect_err("out-of-range min importance should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_MIN_IMPORTANCE must be between 0.0 and 1.0")
        );
    }

    #[test]
    fn openai_provider_requires_api_key() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_LLM_PROVIDER", "openai");
        }

        let error = Settings::from_env().expect_err("openai without key should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains("OPENAI_API_KEY"));
    }

    fn real_provider_settings() -> Settings {
        Settings {
            llm_provider: super::LlmProviderKind::OpenAi,
            embedding_provider: super::EmbeddingProviderKind::OpenAi,
            openai_api_key: Some("test-key".to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn startup_refuses_default_mock_providers() {
        let error = Settings::default()
            .check_startup_providers()
            .expect_err("mock providers must be refused without MEMQUIRA_ALLOW_MOCK");
        let message = error.to_string();
        assert!(message.contains("MEMQUIRA_LLM_PROVIDER and MEMQUIRA_EMBEDDING_PROVIDER"));
        assert!(message.contains("MEMQUIRA_ALLOW_MOCK=true"));
    }

    #[test]
    fn startup_refuses_a_single_mock_role() {
        let settings = Settings {
            embedding_provider: super::EmbeddingProviderKind::Mock,
            ..real_provider_settings()
        };
        let message = settings
            .check_startup_providers()
            .expect_err("a mock embedding provider must be refused")
            .to_string();
        assert!(message.contains("MEMQUIRA_EMBEDDING_PROVIDER resolves to the mock provider"));
        assert!(!message.contains("MEMQUIRA_LLM_PROVIDER resolves"));
    }

    #[test]
    fn startup_allows_mock_providers_only_when_explicit() {
        let settings = Settings {
            allow_mock: true,
            ..Settings::default()
        };
        assert_eq!(
            settings
                .check_startup_providers()
                .expect("explicit mock mode"),
            super::ProviderStartupMode::MockAllowed
        );
    }

    #[test]
    fn startup_accepts_real_providers() {
        assert_eq!(
            real_provider_settings()
                .check_startup_providers()
                .expect("real providers"),
            super::ProviderStartupMode::Real
        );
    }

    #[test]
    fn startup_refuses_mock_in_a_real_fallback_chain_even_when_mock_is_allowed() {
        let settings = Settings {
            allow_mock: true,
            provider_fallback_enabled: true,
            llm_fallback_order: vec!["openai".to_string(), "mock".to_string()],
            ..real_provider_settings()
        };
        let message = settings
            .check_startup_providers()
            .expect_err("a real provider must never fall back to the mock model")
            .to_string();
        assert!(message.contains("MEMQUIRA_LLM_FALLBACK_ORDER mixes `mock` with a real provider"));
    }

    #[test]
    fn startup_checks_fallback_orders_when_fallback_is_enabled() {
        let real = Settings {
            provider_fallback_enabled: true,
            ..real_provider_settings()
        };
        assert_eq!(
            real.check_startup_providers()
                .expect("default fallback orders are real"),
            super::ProviderStartupMode::Real
        );
        let all_mock = Settings {
            provider_fallback_enabled: true,
            embedding_fallback_order: vec!["mock".to_string()],
            ..real_provider_settings()
        };
        let message = all_mock
            .check_startup_providers()
            .expect_err("a mock-only embedding chain needs MEMQUIRA_ALLOW_MOCK")
            .to_string();
        assert!(
            message.contains("MEMQUIRA_EMBEDDING_FALLBACK_ORDER resolves to the mock provider")
        );
    }

    #[test]
    fn allow_mock_and_dependency_timeout_load_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let defaults = Settings::from_env().expect("defaults load");
        assert!(!defaults.allow_mock);
        assert_eq!(defaults.startup_dependency_timeout_seconds, 60);

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_ALLOW_MOCK", "true");
            std::env::set_var("MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS", "5");
        }
        let settings = Settings::from_env().expect("settings load");
        assert!(settings.allow_mock);
        assert_eq!(settings.startup_dependency_timeout_seconds, 5);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_STARTUP_DEPENDENCY_TIMEOUT_SECONDS", "3601") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn openai_api_key_not_required_for_mock_providers() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("mock providers should load without openai key");
        assert_eq!(settings.llm_provider, super::LlmProviderKind::Mock);
        assert!(settings.openai_api_key.is_none());
    }

    #[test]
    fn loads_rate_limit_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_RATE_LIMIT_ENABLED", "false");
            std::env::set_var("MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE", "120");
        }

        let settings = Settings::from_env().expect("rate limit settings should load");
        assert!(!settings.rate_limit_enabled);
        assert_eq!(settings.rate_limit_requests_per_minute, 120);
    }

    #[test]
    fn fails_on_zero_rate_limit_requests_per_minute() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE", "0") };

        let error = Settings::from_env().expect_err("zero rate limit should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE must be greater than 0")
        );
    }

    #[test]
    fn fails_on_invalid_rate_limit_requests_per_minute() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE", "not-a-number") };

        let error = Settings::from_env().expect_err("invalid rate limit should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error.to_string().contains(
                "MEMQUIRA_RATE_LIMIT_REQUESTS_PER_MINUTE must be a valid unsigned integer"
            )
        );
    }

    #[test]
    fn postgres_event_backend_requires_postgres_url() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_FACT_BACKEND", "mock");
            std::env::set_var("MEMQUIRA_EVENT_BACKEND", "postgres");
        }

        let error = Settings::from_env().expect_err("postgres event without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_POSTGRES_URL is required when MEMQUIRA_EVENT_BACKEND=postgres")
        );
    }

    #[test]
    fn fails_on_invalid_event_backend() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_EVENT_BACKEND", "not-a-backend");
        }

        let error = Settings::from_env().expect_err("invalid event backend should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_EVENT_BACKEND value")
        );
    }

    #[test]
    fn postgres_fact_backend_requires_postgres_url() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_FACT_BACKEND", "postgres");
        }

        let error = Settings::from_env().expect_err("postgres without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_POSTGRES_URL is required when MEMQUIRA_FACT_BACKEND=postgres")
        );
    }

    #[test]
    fn fails_on_invalid_log_format() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_LOG_FORMAT", "yaml") };

        let error = Settings::from_env().expect_err("invalid log format should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_LOG_FORMAT value")
        );
    }

    #[test]
    fn episodes_default_off_with_a_derived_collection() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().unwrap();
        assert!(!settings.episodes_enabled);
        assert_eq!(
            settings.qdrant_episode_collection,
            "memcore_vectors_episodes"
        );

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_EPISODES_ENABLED", "true");
            std::env::set_var("MEMQUIRA_QDRANT_COLLECTION", "facts_a");
        }
        let settings = Settings::from_env().unwrap();
        assert!(settings.episodes_enabled);
        assert_eq!(settings.qdrant_episode_collection, "facts_a_episodes");

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_QDRANT_EPISODE_COLLECTION", "facts_a") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn qdrant_ensure_indexes_defaults_on_and_can_be_disabled() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        assert!(Settings::from_env().unwrap().qdrant_ensure_indexes);

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_QDRANT_ENSURE_INDEXES", "false") };
        assert!(!Settings::from_env().unwrap().qdrant_ensure_indexes);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_QDRANT_ENSURE_INDEXES", "sometimes") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn byok_roles_resolve_independently_and_fall_back_to_openai_settings() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_LLM_PROVIDER", "openai");
            std::env::set_var("MEMQUIRA_EMBEDDING_PROVIDER", "openai");
            std::env::set_var("OPENAI_BASE_URL", "https://shared.example.test/v1");
            std::env::set_var("OPENAI_API_KEY", "sk-shared-INVENTED");
        }
        let s = Settings::from_env().unwrap();
        assert_eq!(
            s.llm_endpoint(),
            s.embedding_endpoint(),
            "single endpoint unchanged"
        );
        // SAFETY: as above.
        unsafe {
            std::env::set_var("MEMQUIRA_EMBEDDING_BASE_URL", "http://127.0.0.1:8090/v1");
            std::env::set_var("MEMQUIRA_EMBEDDING_API_KEY", "sk-emb-INVENTED");
            std::env::set_var("MEMQUIRA_LLM_BASE_URL", "https://llm.example.test/v1");
            std::env::set_var("MEMQUIRA_LLM_API_KEY", "sk-llm-INVENTED");
        }
        let s = Settings::from_env().unwrap();
        assert_eq!(s.llm_endpoint().base_url, "https://llm.example.test/v1");
        assert_eq!(s.llm_endpoint().api_key.as_deref(), Some("sk-llm-INVENTED"));
        assert_eq!(s.embedding_endpoint().base_url, "http://127.0.0.1:8090/v1");
        assert_eq!(
            s.embedding_endpoint().api_key.as_deref(),
            Some("sk-emb-INVENTED")
        );
        let printed = format!("{s:?}");
        for secret in ["sk-llm-INVENTED", "sk-emb-INVENTED", "sk-shared-INVENTED"] {
            assert!(!printed.contains(secret), "{secret} leaked in Debug");
        }
    }

    /// `KEY: value` lines of a Compose environment block, `${VAR:-default}` resolved to the
    /// default (the profile as shipped, without a docker/.env).
    fn compose_env_block(compose: &str, start: &str, indent: usize) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut lines = compose
            .lines()
            .skip_while(|line| line.trim_end() != start)
            .skip(1);
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if line.len() - line.trim_start().len() < indent {
                break;
            }
            let (key, value) = trimmed.split_once(':').expect("KEY: value");
            let mut value = value.trim().trim_matches('"').to_string();
            if let Some(inner) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
                value = inner
                    .split_once(":-")
                    .map(|(_, d)| d)
                    .unwrap_or("")
                    .to_string();
            }
            out.push((key.to_string(), value));
        }
        assert!(!out.is_empty(), "no env block after {start:?}");
        out
    }

    /// The No-AWS Local Starter profile (as shipped in docker/) loads, uses only known
    /// settings, never selects mock providers or AWS, needs no keys, and pins the BGE identity.
    #[test]
    fn local_starter_compose_profile_loads_without_aws_or_keys() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        let docker = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker");
        let base =
            std::fs::read_to_string(docker.join("docker-compose.local-starter.yml")).unwrap();
        let fallback =
            std::fs::read_to_string(docker.join("docker-compose.local-starter.ollama-embed.yml"))
                .unwrap();
        let profile = compose_env_block(&base, "x-memquira-env: &memquira-env", 2);
        let memquira_service = &fallback[fallback
            .find(
                "
  memquira:",
            )
            .expect("memquira service")..];
        let fallback_env = compose_env_block(memquira_service, "    environment:", 6);
        assert!(fallback_env.len() >= 6, "{fallback_env:?}");

        let load = |extra: &[(String, String)]| {
            clear_env();
            for (key, value) in profile.iter().chain(extra.iter()) {
                assert!(ENV_KEYS.contains(&key.as_str()), "unknown setting {key}");
                assert!(
                    !key.starts_with("AWS_")
                        && ![
                            "OPENAI_API_KEY",
                            "MEMQUIRA_LLM_API_KEY",
                            "MEMQUIRA_EMBEDDING_API_KEY"
                        ]
                        .contains(&key.as_str()),
                    "{key}: no AWS settings and no provider keys"
                );
                // SAFETY: tests mutate env only while holding the process-wide mutex.
                unsafe { std::env::set_var(key, value) };
            }
            Settings::from_env().expect("local starter profile must load")
        };

        let s = load(&[]);
        assert_eq!(s.vector_backend, VectorBackend::Qdrant);
        assert!(!s.allow_mock);
        assert_eq!(s.llm_provider, LlmProviderKind::OpenAi);
        assert_eq!(s.embedding_provider, EmbeddingProviderKind::OpenAi);
        assert!(s.real_provider_calls_enabled);
        assert_eq!(s.provider_test_mode, ProviderTestMode::Production);
        assert_eq!(s.llm_endpoint().base_url, "http://ollama:11434/v1");
        assert_eq!(s.llm_endpoint().api_key, None);
        assert_eq!(s.embedding_endpoint().base_url, "http://tei:80/v1");
        assert_eq!(s.embedding_endpoint().api_key, None);
        assert_eq!(s.embedding_model, "BAAI/bge-small-en-v1.5");
        let o = &s.providers.embedding_options;
        assert_eq!(o.dimensions, Some(384));
        assert_eq!(
            o.model_revision.as_deref(),
            Some("5c38ec7c405ec4b44b94cc5a9bb96e735b38267a")
        );
        assert_eq!(
            o.query_prefix.as_deref(),
            Some("Represent this sentence for searching relevant passages: ")
        );
        assert_eq!(o.document_prefix, None);
        assert!(o.normalize && !o.send_dimensions);
        assert_eq!(
            o.dedup_threshold, None,
            "uncalibrated: exact-only dedup (D-L3)"
        );
        assert!(s.providers.probe);
        assert!(
            !s.structured_extraction_enabled,
            "small local models: legacy extraction prompt (T20 probe)"
        );
        assert_eq!(s.providers.llm_json_mode, LlmJsonMode::JsonSchema);

        let f = load(&fallback_env);
        assert_eq!(f.embedding_endpoint().base_url, "http://ollama:11434/v1");
        assert_eq!(f.providers.embedding_options.dimensions, Some(768));
        assert_eq!(
            f.providers.embedding_options.document_prefix.as_deref(),
            Some("search_document: ")
        );
        assert_eq!(f.providers.embedding_options.dedup_threshold, None);
        assert!(!f.allow_mock);
    }

    /// `MEMQUIRA_QDRANT_SEARCH_HNSW_EF` is unset by default (the application default is
    /// unchanged by the reference promotion), parses 1..=4096 and refuses anything else (startup
    /// fails); the reference contract accepts exactly 256 (the accepted value), and unset, 128,
    /// 512 or any other beam is a mismatch.
    #[test]
    fn qdrant_search_hnsw_ef_parsing_and_reference_contract() {
        use super::*;
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        assert_eq!(Settings::from_env().unwrap().qdrant_search_hnsw_ef, None);
        for (value, expected) in [
            ("256", Some(256)),
            ("128", Some(128)),
            (" 64 ", Some(64)),
            ("4096", Some(4096)),
        ] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_QDRANT_SEARCH_HNSW_EF", value) };
            assert_eq!(
                Settings::from_env().unwrap().qdrant_search_hnsw_ef,
                expected,
                "{value}"
            );
        }
        for bad in ["0", "4097", "-1", "abc", "12.5"] {
            // SAFETY: as above.
            unsafe { std::env::set_var("MEMQUIRA_QDRANT_SEARCH_HNSW_EF", bad) };
            let error = Settings::from_env().expect_err(bad).to_string();
            assert!(error.contains("MEMQUIRA_QDRANT_SEARCH_HNSW_EF"), "{error}");
        }
        let mut s = Settings::default();
        s.llm_provider = LlmProviderKind::OpenAi;
        s.embedding_provider = EmbeddingProviderKind::OpenAi;
        s.llm_model = REFERENCE_LLM_MODEL.to_string();
        s.embedding_model = REFERENCE_EMBEDDING_MODEL.to_string();
        s.episodes_enabled = true;
        s.search_max_limit = 100;
        for (ef, reference) in [
            (Some(256), true),
            (None, false),
            (Some(128), false),
            (Some(512), false),
            (Some(255), false),
            (Some(257), false),
            (Some(64), false),
            (Some(1), false),
            (Some(4096), false),
        ] {
            s.qdrant_search_hnsw_ef = ef;
            assert_eq!(
                !s.reference_contract_mismatches()
                    .contains(&"qdrant_search_hnsw_ef"),
                reference,
                "{ef:?}"
            );
        }
    }

    /// An otherwise exact reference configuration is
    /// `reference-validated` only with `MEMQUIRA_QDRANT_SEARCH_HNSW_EF=256`. Unset (the rollback) and
    /// any other beam are `compatible-unvalidated`, and
    /// `MEMQUIRA_REQUIRE_QUALITY_TIER=reference-validated` refuses them, naming the setting; a
    /// rollback that requires `compatible-unvalidated` starts.
    #[test]
    fn only_reference_qdrant_search_hnsw_ef_is_reference_validated() {
        use super::*;
        assert_eq!(REFERENCE_QDRANT_SEARCH_HNSW_EF, 256);
        let mut s = Settings::default();
        s.llm_provider = LlmProviderKind::OpenAi;
        s.embedding_provider = EmbeddingProviderKind::OpenAi;
        s.llm_model = REFERENCE_LLM_MODEL.to_string();
        s.embedding_model = REFERENCE_EMBEDDING_MODEL.to_string();
        s.episodes_enabled = true;
        s.search_max_limit = 100;
        s.qdrant_search_hnsw_ef = Some(REFERENCE_QDRANT_SEARCH_HNSW_EF);
        s.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
        assert_eq!(s.reference_contract_mismatches(), Vec::<&str>::new());
        assert_eq!(s.quality_tier(), QualityTier::ReferenceValidated);
        assert!(s.check_required_quality_tier().is_ok());
        for ef in [
            None,
            Some(128),
            Some(512),
            Some(64),
            Some(255),
            Some(1),
            Some(4096),
        ] {
            s.qdrant_search_hnsw_ef = ef;
            assert_eq!(
                s.reference_contract_mismatches(),
                vec!["qdrant_search_hnsw_ef"],
                "{ef:?}"
            );
            assert_eq!(
                s.quality_tier(),
                QualityTier::CompatibleUnvalidated,
                "{ef:?}"
            );
            let error = s.check_required_quality_tier().unwrap_err().to_string();
            assert!(error.contains("qdrant_search_hnsw_ef"), "{ef:?}: {error}");
        }
        // The rollback (unset) runs when the deployment requires the tier it actually has.
        s.qdrant_search_hnsw_ef = None;
        s.providers.require_quality_tier = Some(QualityTier::CompatibleUnvalidated);
        assert_eq!(
            s.check_required_quality_tier().unwrap(),
            QualityTier::CompatibleUnvalidated
        );
        s.qdrant_search_hnsw_ef = Some(REFERENCE_QDRANT_SEARCH_HNSW_EF);
        s.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
        assert_eq!(s.quality_tier(), QualityTier::ReferenceValidated);
    }

    /// Unset by default; parses 1..=1,000,000 and refuses anything
    /// else (startup fails); any explicit value (the 12,000 candidate included) is a reference mismatch,
    /// `compatible-unvalidated`, refused under a pinned `reference-validated`; unset restores the tier.
    #[test]
    fn qdrant_exact_search_max_tenant_points_parsing_and_tier() {
        use super::*;
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        assert_eq!(
            Settings::from_env()
                .unwrap()
                .qdrant_exact_search_max_tenant_points,
            None
        );
        for (value, expected) in [("12000", 12_000), (" 1 ", 1), ("1000000", 1_000_000)] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS", value) };
            assert_eq!(
                Settings::from_env()
                    .unwrap()
                    .qdrant_exact_search_max_tenant_points,
                Some(expected),
                "{value}"
            );
        }
        for bad in ["0", "1000001", "-1", "abc", "1.5"] {
            // SAFETY: as above.
            unsafe { std::env::set_var("MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS", bad) };
            let error = Settings::from_env().expect_err(bad).to_string();
            assert!(
                error.contains("MEMQUIRA_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS"),
                "{error}"
            );
        }
        let mut s = Settings::default();
        s.llm_provider = LlmProviderKind::OpenAi;
        s.embedding_provider = EmbeddingProviderKind::OpenAi;
        s.llm_model = REFERENCE_LLM_MODEL.to_string();
        s.embedding_model = REFERENCE_EMBEDDING_MODEL.to_string();
        s.episodes_enabled = true;
        s.search_max_limit = 100;
        s.qdrant_search_hnsw_ef = Some(REFERENCE_QDRANT_SEARCH_HNSW_EF);
        s.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
        assert_eq!(s.quality_tier(), QualityTier::ReferenceValidated);
        for n in [12_000, 1, 1_000_000] {
            s.qdrant_exact_search_max_tenant_points = Some(n);
            assert_eq!(
                s.reference_contract_mismatches(),
                vec!["qdrant_exact_search_max_tenant_points"],
                "{n}"
            );
            assert_eq!(s.quality_tier(), QualityTier::CompatibleUnvalidated, "{n}");
            assert!(s.check_required_quality_tier().is_err(), "{n}");
        }
        s.qdrant_exact_search_max_tenant_points = None;
        assert_eq!(s.quality_tier(), QualityTier::ReferenceValidated);
        assert!(s.check_required_quality_tier().is_ok());
    }

    /// The reference contract and the tier of changed configurations.
    #[test]
    fn reference_contract_and_quality_tiers() {
        use super::*;
        let mut s = Settings::default();
        assert_eq!(s.quality_tier(), QualityTier::DevelopmentMock);
        s.llm_provider = LlmProviderKind::OpenAi;
        s.embedding_provider = EmbeddingProviderKind::OpenAi;
        s.llm_model = REFERENCE_LLM_MODEL.to_string();
        s.embedding_model = REFERENCE_EMBEDDING_MODEL.to_string();
        s.episodes_enabled = true;
        s.search_max_limit = 100;
        s.qdrant_search_hnsw_ef = Some(REFERENCE_QDRANT_SEARCH_HNSW_EF);
        assert_eq!(s.reference_contract_mismatches(), Vec::<&str>::new());
        assert_eq!(s.quality_tier(), QualityTier::ReferenceValidated);
        // Any change to a required reference parameter loses the tier.
        type Change = Box<dyn Fn(&mut Settings)>;
        let changes: Vec<(&str, Change)> = vec![
            ("llm_model", Box::new(|s| s.llm_model = "other".into())),
            (
                "embedding_model",
                Box::new(|s| s.embedding_model = "other".into()),
            ),
            (
                "embedding_dimensions",
                Box::new(|s| s.providers.embedding_options.dimensions = Some(1024)),
            ),
            (
                "llm_json_mode",
                Box::new(|s| s.providers.llm_json_mode = LlmJsonMode::JsonSchema),
            ),
            ("episodes_enabled", Box::new(|s| s.episodes_enabled = false)),
            ("search_max_limit", Box::new(|s| s.search_max_limit = 50)),
            (
                "consolidation_mode",
                Box::new(|s| s.consolidation_mode = ConsolidationMode::LlmV2),
            ),
            (
                "related_fact_matcher",
                Box::new(|s| s.related_fact_matcher = RelatedFactMatcher::Semantic),
            ),
            (
                "classification_prompt",
                Box::new(|s| s.classification_prompt = ClassificationPromptSetting::OperationsV1),
            ),
            (
                "importance_policy",
                Box::new(|s| s.importance_policy = ImportancePolicySetting::Durable),
            ),
            (
                "temporal_rewrite",
                Box::new(|s| s.temporal_rewrite = TemporalRewriteSetting::Preserving),
            ),
            (
                "unknown_memory_type",
                Box::new(|s| s.unknown_memory_type = UnknownMemoryTypeSetting::Other),
            ),
            (
                "event_update_policy",
                Box::new(|s| s.event_update_policy = EventUpdatePolicySetting::ChangeMarker),
            ),
            (
                "dedup_value_guard",
                Box::new(|s| s.dedup_value_guard = DedupValueGuardSetting::On),
            ),
            (
                "related_fact_similarity_floor",
                Box::new(|s| s.related_fact_similarity_floor = 0.45),
            ),
            (
                "context_default_packing",
                Box::new(|s| s.context_default_packing = ContextPacking::Evidence),
            ),
            (
                "rerank_provider",
                Box::new(|s| s.rerank_provider = RerankProviderSetting::Local),
            ),
            (
                "query_decomposition",
                Box::new(|s| s.query_decomposition = QueryDecompositionSetting::Llm),
            ),
            (
                "structured_extraction_policy",
                Box::new(|s| s.structured_extraction_policy = StructuredExtractionPolicy::Atomic),
            ),
            (
                "context_cache_enabled",
                Box::new(|s| s.context_cache_enabled = true),
            ),
            (
                "embedding_dedup_threshold",
                Box::new(|s| s.providers.embedding_options.dedup_threshold = Some(0.9)),
            ),
            (
                "embedding_prefixes",
                Box::new(|s| s.providers.embedding_options.query_prefix = Some("q: ".into())),
            ),
            (
                "qdrant_search_hnsw_ef",
                Box::new(|s| s.qdrant_search_hnsw_ef = None),
            ),
        ];
        for (name, change) in changes {
            let mut changed = s.clone();
            change(&mut changed);
            assert_eq!(
                changed.quality_tier(),
                QualityTier::CompatibleUnvalidated,
                "{name}"
            );
            assert!(
                changed.reference_contract_mismatches().contains(&name),
                "{name}"
            );
        }
        // Required tier: refused when it differs, accepted when it matches.
        let mut required = s.clone();
        required.providers.require_quality_tier = Some(QualityTier::ReferenceValidated);
        assert!(required.check_required_quality_tier().is_ok());
        required.llm_model = "other".into();
        let error = required
            .check_required_quality_tier()
            .unwrap_err()
            .to_string();
        assert!(error.contains("llm_model"), "{error}");
    }

    #[test]
    fn quality_tier_and_llm_probe_settings_parse() {
        use super::*;
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        assert_eq!(
            Settings::from_env().unwrap().providers.require_quality_tier,
            None
        );
        assert!(!Settings::from_env().unwrap().providers.llm_startup_probe);
        unsafe {
            std::env::set_var("MEMQUIRA_REQUIRE_QUALITY_TIER", "reference-validated");
            std::env::set_var("MEMQUIRA_LLM_STARTUP_PROBE", "true");
            std::env::set_var("MEMQUIRA_LLM_JSON_MODE", "instruction");
        }
        let s = Settings::from_env().unwrap();
        assert_eq!(
            s.providers.require_quality_tier,
            Some(QualityTier::ReferenceValidated)
        );
        assert!(s.providers.llm_startup_probe);
        assert_eq!(s.providers.llm_json_mode, LlmJsonMode::Instruction);
        unsafe { std::env::set_var("MEMQUIRA_REQUIRE_QUALITY_TIER", "gold") };
        assert!(Settings::from_env().is_err());
        clear_env();
    }

    /// `MEMQUIRA_LLM_JSON_MODE` defaults to `json_object` (request bodies unchanged).
    #[test]
    fn llm_json_mode_defaults_to_json_object_and_rejects_unknown_values() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        assert_eq!(
            Settings::from_env().unwrap().providers.llm_json_mode,
            LlmJsonMode::JsonObject
        );
        for (value, expected) in [
            ("json_object", LlmJsonMode::JsonObject),
            ("JSON_SCHEMA", LlmJsonMode::JsonSchema),
        ] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_LLM_JSON_MODE", value) };
            assert_eq!(
                Settings::from_env().unwrap().providers.llm_json_mode,
                expected
            );
        }
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_LLM_JSON_MODE", "grammar") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn byok_endpoint_validation_refuses_unsafe_combinations() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        let base = [
            ("MEMQUIRA_LLM_PROVIDER", "mock"),
            ("MEMQUIRA_ALLOW_MOCK", "true"),
            ("MEMQUIRA_EMBEDDING_PROVIDER", "openai"),
        ];
        let case = |extra: &[(&str, &str)]| {
            clear_env();
            for (k, v) in base.iter().chain(extra.iter()) {
                // SAFETY: tests mutate env only while holding the process-wide mutex.
                unsafe { std::env::set_var(k, v) };
            }
            Settings::from_env()
        };
        // Key-free loopback (local server) is allowed; a remote one needs a key or explicit opt-in.
        assert!(case(&[("MEMQUIRA_EMBEDDING_BASE_URL", "http://localhost:8080/v1")]).is_ok());
        assert!(case(&[("MEMQUIRA_EMBEDDING_BASE_URL", "http://[::1]:8080")]).is_ok());
        let error = case(&[("MEMQUIRA_EMBEDDING_BASE_URL", "https://emb.example.test/v1")])
            .expect_err("remote without key");
        assert!(
            error.to_string().contains("MEMQUIRA_EMBEDDING_API_KEY"),
            "{error}"
        );
        assert!(
            case(&[
                ("MEMQUIRA_EMBEDDING_BASE_URL", "https://emb.example.test/v1"),
                ("MEMQUIRA_EMBEDDING_ALLOW_NO_API_KEY", "true"),
            ])
            .is_ok()
        );
        // A key over plain http to a remote host is refused unless explicitly allowed.
        let error = case(&[
            ("MEMQUIRA_EMBEDDING_BASE_URL", "http://emb.example.test/v1"),
            ("MEMQUIRA_EMBEDDING_API_KEY", "sk-INVENTED-secret"),
        ])
        .expect_err("plain http with a key");
        assert!(!error.to_string().contains("sk-INVENTED-secret"), "{error}");
        assert!(
            case(&[
                ("MEMQUIRA_EMBEDDING_BASE_URL", "http://emb.example.test/v1"),
                ("MEMQUIRA_EMBEDDING_API_KEY", "sk-INVENTED-secret"),
                ("MEMQUIRA_PROVIDER_ALLOW_INSECURE_HTTP", "true"),
            ])
            .is_ok()
        );
        // Malformed URLs.
        for bad in [
            "not a url",
            "ftp://emb.example.test/v1",
            "https://user:pw@emb.example.test/v1",
            "https://emb.example.test/v1?key=abc",
        ] {
            let error = case(&[
                ("MEMQUIRA_EMBEDDING_BASE_URL", bad),
                ("MEMQUIRA_EMBEDDING_API_KEY", "sk-INVENTED"),
            ])
            .expect_err(bad);
            assert!(
                !error.to_string().contains("pw@") && !error.to_string().contains("key=abc"),
                "{error}"
            );
        }
        // Embedding options.
        assert!(
            case(&[
                ("MEMQUIRA_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
                ("MEMQUIRA_EMBEDDING_DIMENSIONS", "0")
            ])
            .is_err()
        );
        assert!(
            case(&[
                ("MEMQUIRA_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
                ("MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD", "1.5")
            ])
            .is_err()
        );
        assert!(
            case(&[
                ("MEMQUIRA_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
                ("MEMQUIRA_PROVIDER_PROBE", "sometimes")
            ])
            .is_err()
        );
        let s = case(&[
            ("MEMQUIRA_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
            ("MEMQUIRA_EMBEDDING_DIMENSIONS", "384"),
            ("MEMQUIRA_EMBEDDING_DEDUP_THRESHOLD", "0.95"),
            ("MEMQUIRA_PROVIDER_PROBE", "off"),
            ("MEMQUIRA_EMBEDDING_SEND_DIMENSIONS", "false"),
        ])
        .unwrap();
        assert_eq!(s.providers.embedding_options.dimensions, Some(384));
        assert_eq!(s.providers.embedding_options.dedup_threshold, Some(0.95));
        assert!(!s.providers.probe);
        assert!(!s.providers.embedding_options.send_dimensions);
    }

    #[test]
    fn admission_defaults_overrides_and_validation() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let a = Settings::from_env().unwrap().admission;
        assert!(a.enabled);
        assert_eq!(
            (a.read.max_in_flight, a.read.max_queue, a.read.deadline_ms),
            (16, 64, 30_000)
        );
        assert_eq!(
            (
                a.write.max_in_flight,
                a.write.max_queue,
                a.write.deadline_ms
            ),
            (8, 32, 120_000)
        );
        assert_eq!(a.retry_after_seconds, 1);

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_ADMISSION_CONTROL", "off");
            std::env::set_var("MEMQUIRA_ADMISSION_WRITE_MAX_IN_FLIGHT", "3");
            std::env::set_var("MEMQUIRA_PROVIDER_CHAT_MAX_CONCURRENCY", "2");
        }
        let a = Settings::from_env().unwrap().admission;
        assert!(!a.enabled);
        assert_eq!(a.write.max_in_flight, 3);
        assert_eq!(a.provider_chat_max_concurrency, 2);

        for (key, value) in [
            ("MEMQUIRA_ADMISSION_CONTROL", "maybe"),
            ("MEMQUIRA_ADMISSION_READ_MAX_IN_FLIGHT", "0"),
            ("MEMQUIRA_ADMISSION_READ_MAX_IN_FLIGHT", "-1"),
            ("MEMQUIRA_ADMISSION_QUEUE_TIMEOUT_MS", "0"),
            // Below the 2,000 ms default queue timeout.
            ("MEMQUIRA_ADMISSION_READ_DEADLINE_MS", "1000"),
            ("MEMQUIRA_ADMISSION_RETRY_AFTER_SECONDS", "0"),
            ("MEMQUIRA_PROVIDER_EMBEDDING_MAX_CONCURRENCY", "0"),
            ("MEMQUIRA_PROVIDER_PERMIT_TIMEOUT_MS", "0"),
        ] {
            clear_env();
            // SAFETY: as above.
            unsafe { std::env::set_var(key, value) };
            assert!(
                Settings::from_env().is_err(),
                "{key}={value} must be refused"
            );
        }
    }

    #[test]
    fn qdrant_write_mode_parses_and_rejects_unknown_values() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        assert_eq!(
            Settings::from_env().unwrap().qdrant_write_mode,
            super::QdrantWriteModeSetting::Optimized
        );
        for (value, expected) in [
            ("legacy", super::QdrantWriteModeSetting::Legacy),
            ("Optimized", super::QdrantWriteModeSetting::Optimized),
        ] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_QDRANT_WRITE_MODE", value) };
            assert_eq!(Settings::from_env().unwrap().qdrant_write_mode, expected);
        }
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_QDRANT_WRITE_MODE", "fast") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn phase4_candidate_flags_default_off_and_are_validated() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().unwrap();
        assert!(!settings.query_analysis_enabled);
        assert!(!settings.temporal_ranking_enabled);
        assert_eq!(settings.temporal_boost, 0.05);
        assert_eq!(settings.multi_query, MultiQuerySetting::None);
        assert_eq!(settings.rerank_provider, RerankProviderSetting::None);
        assert_eq!(settings.rerank_candidates, 60);

        let set = |key: &str, value: &str| {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var(key, value) };
        };
        // A pipeline flag without the endpoint is refused.
        set("MEMQUIRA_QUERY_ANALYSIS_ENABLED", "true");
        assert!(Settings::from_env().is_err());
        set("MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED", "true");
        assert!(Settings::from_env().is_ok());
        // Temporal ranking and multi-query need analysis.
        set("MEMQUIRA_QUERY_ANALYSIS_ENABLED", "false");
        set("MEMQUIRA_TEMPORAL_RANKING_ENABLED", "true");
        assert!(Settings::from_env().is_err());
        set("MEMQUIRA_QUERY_ANALYSIS_ENABLED", "true");
        set("MEMQUIRA_MULTI_QUERY", "deterministic");
        set("MEMQUIRA_TEMPORAL_BOOST", "0.1");
        let settings = Settings::from_env().unwrap();
        assert_eq!(settings.multi_query, MultiQuerySetting::Deterministic);
        assert_eq!(settings.temporal_boost, 0.1);
        // Bedrock needs no credentials at settings time; local needs its four file settings.
        // No default region; the model ARN defaults to Amazon Rerank 1.0 in the configured
        // region and must name that region.
        set("MEMQUIRA_RERANK_PROVIDER", "bedrock");
        assert!(Settings::from_env().is_err(), "bedrock needs a region");
        set("MEMQUIRA_RERANK_BEDROCK_REGION", "eu-central-1");
        let settings = Settings::from_env().unwrap();
        assert_eq!(settings.rerank_provider, RerankProviderSetting::Bedrock);
        assert_eq!(settings.rerank_bedrock_region, "eu-central-1");
        assert_eq!(
            settings.rerank_bedrock_model_arn,
            "arn:aws:bedrock:eu-central-1::foundation-model/amazon.rerank-v1:0"
        );
        set("MEMQUIRA_RERANK_BEDROCK_MODEL_ARN", "amazon.rerank-v1:0");
        assert!(
            Settings::from_env().is_err(),
            "a bare model id is not an ARN"
        );
        set(
            "MEMQUIRA_RERANK_BEDROCK_MODEL_ARN",
            "arn:aws:bedrock:us-west-2::foundation-model/amazon.rerank-v1:0",
        );
        assert!(
            Settings::from_env().is_err(),
            "the ARN must name the configured region"
        );
        set("MEMQUIRA_RERANK_BEDROCK_REGION", "us-west-2");
        assert!(Settings::from_env().is_ok(), "any region can be configured");
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("MEMQUIRA_RERANK_BEDROCK_MODEL_ARN");
            std::env::remove_var("MEMQUIRA_RERANK_BEDROCK_REGION");
        }
        set("MEMQUIRA_RERANK_PROVIDER", "local");
        assert!(Settings::from_env().is_err());
        for (key, value) in [
            ("MEMQUIRA_RERANK_LOCAL_MODEL_PATH", "m.onnx"),
            ("MEMQUIRA_RERANK_LOCAL_MODEL_SHA256", "ab"),
            ("MEMQUIRA_RERANK_LOCAL_TOKENIZER_PATH", "t.json"),
            ("MEMQUIRA_RERANK_LOCAL_TOKENIZER_SHA256", "cd"),
        ] {
            set(key, value);
        }
        assert_eq!(
            Settings::from_env().unwrap().rerank_provider,
            RerankProviderSetting::Local
        );
        set("MEMQUIRA_QUERY_DECOMPOSITION", "llm");
        assert_eq!(
            Settings::from_env().unwrap().query_decomposition,
            QueryDecompositionSetting::Llm
        );
        set("MEMQUIRA_QUERY_DECOMPOSITION", "noop");
        assert_eq!(
            Settings::from_env().unwrap().query_decomposition,
            QueryDecompositionSetting::Noop
        );
        for (key, bad) in [
            ("MEMQUIRA_TEMPORAL_BOOST", "0"),
            ("MEMQUIRA_TEMPORAL_BOOST", "1.5"),
            ("MEMQUIRA_RERANK_CANDIDATES", "0"),
            ("MEMQUIRA_MULTI_QUERY", "llm"),
            ("MEMQUIRA_RERANK_PROVIDER", "cohere"),
            ("MEMQUIRA_RERANK_TIMEOUT_MS", "5"),
            ("MEMQUIRA_RERANK_LOCAL_BATCH_SIZE", "0"),
            ("MEMQUIRA_QUERY_DECOMPOSITION", "nova-lite"),
        ] {
            let previous = std::env::var(key).ok();
            set(key, bad);
            assert!(Settings::from_env().is_err(), "{key}={bad}");
            match previous {
                Some(value) => set(key, &value),
                // SAFETY: as above.
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }

    #[test]
    fn experimental_candidates_default_off() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        assert!(
            !Settings::from_env()
                .unwrap()
                .experimental_candidates_enabled
        );
        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED", "true") };
        assert!(
            Settings::from_env()
                .unwrap()
                .experimental_candidates_enabled
        );
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_EXPERIMENTAL_CANDIDATES_ENABLED", "maybe") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn search_max_limit_defaults_to_fifty_and_is_bounded() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        assert_eq!(Settings::from_env().unwrap().search_max_limit, 50);
        for (value, ok) in [
            ("50", true),
            ("100", true),
            ("200", true),
            ("49", false),
            ("201", false),
            ("1000", false),
            ("x", false),
        ] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_SEARCH_MAX_LIMIT", value) };
            assert_eq!(Settings::from_env().is_ok(), ok, "{value}");
        }
    }

    #[test]
    fn local_embedding_queue_defaults_to_16_and_is_bounded() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let defaults = Settings::from_env().unwrap();
        assert_eq!(defaults.local_embedding_queue_capacity, 16);
        assert_eq!(defaults.local_embedding_model_dir, None);
        for (value, ok) in [("1", true), ("1024", true), ("0", false), ("1025", false)] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_LOCAL_EMBEDDING_QUEUE_CAPACITY", value) };
            assert_eq!(Settings::from_env().is_ok(), ok, "{value}");
        }
    }

    #[test]
    fn ingest_journal_is_on_by_default_and_validated() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().unwrap();
        assert!(settings.ingest_journal_enabled);
        assert_eq!(settings.ingest_recovery_stale_secs, 300);
        assert_eq!(settings.ingest_recovery_interval_secs, 60);
        assert_eq!(settings.ingest_inflight_wait_ms, 10_000);
        assert_eq!(settings.ingest_receipt_ttl_secs, 86_400);
        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_INGEST_JOURNAL_ENABLED", "false") };
        assert!(!Settings::from_env().unwrap().ingest_journal_enabled);
        // SAFETY: as above. The stale window must exceed the write deadline (default 120 s).
        unsafe { std::env::set_var("MEMQUIRA_INGEST_JOURNAL_ENABLED", "true") };
        unsafe { std::env::set_var("MEMQUIRA_INGEST_RECOVERY_STALE_SECS", "120") };
        assert!(Settings::from_env().is_err());
        unsafe { std::env::set_var("MEMQUIRA_INGEST_RECOVERY_STALE_SECS", "121") };
        assert!(Settings::from_env().is_ok());
        unsafe { std::env::set_var("MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS", "0") };
        assert!(Settings::from_env().is_err());
        // SAFETY: as above. Unset, the window follows a longer write deadline (2x, at least 300 s).
        unsafe { std::env::remove_var("MEMQUIRA_INGEST_RECOVERY_INTERVAL_SECS") };
        unsafe { std::env::remove_var("MEMQUIRA_INGEST_RECOVERY_STALE_SECS") };
        unsafe { std::env::set_var("MEMQUIRA_ADMISSION_WRITE_DEADLINE_MS", "900000") };
        assert_eq!(
            Settings::from_env().unwrap().ingest_recovery_stale_secs,
            1_800
        );
    }

    #[test]
    fn classification_skip_when_no_related_is_off_by_default() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        assert!(
            !Settings::from_env()
                .unwrap()
                .classification_skip_when_no_related
        );
        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED", "true") };
        assert!(
            Settings::from_env()
                .unwrap()
                .classification_skip_when_no_related
        );
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_CLASSIFICATION_SKIP_WHEN_NO_RELATED", "sometimes") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn query_embedding_cache_is_off_by_default_and_bounded() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        assert_eq!(
            Settings::from_env().unwrap().query_embedding_cache_capacity,
            0
        );
        for (value, ok) in [
            ("0", true),
            ("1024", true),
            ("1000000", true),
            ("1000001", false),
            ("-1", false),
            ("x", false),
        ] {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::set_var("MEMQUIRA_QUERY_EMBEDDING_CACHE_CAPACITY", value) };
            assert_eq!(Settings::from_env().is_ok(), ok, "{value}");
        }
    }

    #[test]
    fn consolidation_settings_default_to_legacy_and_validate() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("defaults should load");
        assert_eq!(
            settings.consolidation_mode,
            super::ConsolidationMode::Legacy
        );
        assert_eq!(settings.consolidation_candidates, 10);
        assert_eq!(settings.consolidation_similarity_floor, 0.5);

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_CONSOLIDATION_MODE", "LLM_V2");
            std::env::set_var("MEMQUIRA_CONSOLIDATION_CANDIDATES", "20");
            std::env::set_var("MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR", "0.3");
        };
        let settings = Settings::from_env().expect("v2 should load");
        assert_eq!(settings.consolidation_mode, super::ConsolidationMode::LlmV2);
        assert_eq!(settings.consolidation_candidates, 20);
        assert_eq!(settings.consolidation_similarity_floor, 0.3);

        for (key, value) in [
            ("MEMQUIRA_CONSOLIDATION_MODE", "v2"),
            ("MEMQUIRA_CONSOLIDATION_CANDIDATES", "0"),
            ("MEMQUIRA_CONSOLIDATION_CANDIDATES", "51"),
            ("MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR", "1.5"),
            ("MEMQUIRA_CONSOLIDATION_SIMILARITY_FLOOR", "-0.1"),
        ] {
            clear_env();
            // SAFETY: as above.
            unsafe { std::env::set_var(key, value) };
            assert!(Settings::from_env().is_err(), "{key}={value} must fail");
        }
    }

    #[test]
    fn entity_settings_default_off_and_validate() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("defaults should load");
        assert!(!settings.entities_enabled);
        assert_eq!(settings.entity_name_similarity_threshold, 0.70);
        assert!(settings.entity_llm_confirmation_enabled);

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_ENTITIES_ENABLED", "true");
            std::env::set_var("MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD", "0.8");
            std::env::set_var("MEMQUIRA_ENTITY_LLM_CONFIRMATION_ENABLED", "false");
        };
        let settings = Settings::from_env().expect("entities should load");
        assert!(settings.entities_enabled);
        assert_eq!(settings.entity_name_similarity_threshold, 0.8);
        assert!(!settings.entity_llm_confirmation_enabled);

        for value in ["1.5", "-0.1"] {
            clear_env();
            // SAFETY: as above.
            unsafe { std::env::set_var("MEMQUIRA_ENTITY_NAME_SIMILARITY_THRESHOLD", value) };
            assert!(Settings::from_env().is_err(), "{value} must fail");
        }
    }

    #[test]
    fn search_strategy_and_rrf_k_default_and_validate() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("defaults should load");
        assert_eq!(settings.search_strategy, super::SearchStrategy::Hybrid);
        assert_eq!(settings.rrf_k, 60.0);
        assert_eq!(
            (settings.rrf_weight_semantic, settings.rrf_weight_keyword),
            (1.0, 1.0)
        );

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_SEARCH_STRATEGY", "RRF");
            std::env::set_var("MEMQUIRA_RRF_K", "20");
        };
        let settings = Settings::from_env().expect("rrf should load");
        assert_eq!(settings.search_strategy, super::SearchStrategy::Rrf);
        assert_eq!(settings.rrf_k, 20.0);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_RRF_K", "0") };
        assert!(Settings::from_env().is_err(), "k must be positive");
        // SAFETY: as above.
        unsafe {
            std::env::set_var("MEMQUIRA_RRF_K", "60");
            std::env::set_var("MEMQUIRA_SEARCH_STRATEGY", "bm25");
        };
        assert!(Settings::from_env().is_err(), "unknown strategy must fail");

        // SAFETY: as above.
        unsafe {
            std::env::set_var("MEMQUIRA_SEARCH_STRATEGY", "rrf");
            std::env::set_var("MEMQUIRA_RRF_WEIGHT_KEYWORD", "0.5");
        };
        let settings = Settings::from_env().expect("weights should load");
        assert_eq!(settings.rrf_weight_keyword, 0.5);
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_RRF_WEIGHT_KEYWORD", "-1") };
        assert!(Settings::from_env().is_err(), "a negative weight must fail");
    }

    #[test]
    fn structured_extraction_defaults_to_enabled_retentive_and_rejects_unknown_policies() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let settings = Settings::from_env().expect("defaults should load");
        assert!(settings.structured_extraction_enabled);
        assert_eq!(
            settings.structured_extraction_policy,
            super::StructuredExtractionPolicy::Retentive
        );

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_STRUCTURED_EXTRACTION_POLICY", "Atomic");
            std::env::set_var("MEMQUIRA_STRUCTURED_EXTRACTION_ENABLED", "false");
        };
        let settings = Settings::from_env().expect("atomic should load");
        assert!(!settings.structured_extraction_enabled);
        assert_eq!(
            settings.structured_extraction_policy,
            super::StructuredExtractionPolicy::Atomic
        );

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMQUIRA_STRUCTURED_EXTRACTION_POLICY", "verbose") };
        let error = Settings::from_env().expect_err("unknown policy should fail");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_STRUCTURED_EXTRACTION_POLICY value")
        );
    }

    #[test]
    fn fails_on_invalid_log_level() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_LOG_LEVEL", "verbose") };

        let error = Settings::from_env().expect_err("invalid log level should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_LOG_LEVEL value")
        );
    }

    #[test]
    fn loads_observability_settings_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_LOG_FORMAT", "pretty");
            std::env::set_var("MEMQUIRA_LOG_LEVEL", "debug");
            std::env::set_var("MEMQUIRA_REQUEST_ID_HEADER", "X-Correlation-ID");
            std::env::set_var("MEMQUIRA_METRICS_ENABLED", "true");
            std::env::set_var("MEMQUIRA_METRICS_PATH", "/internal/metrics");
            std::env::set_var("MEMQUIRA_METRICS_REQUIRE_AUTH", "false");
            std::env::set_var("MEMQUIRA_METRICS_INCLUDE_PROCESS", "true");
        }

        let settings = Settings::from_env().expect("observability settings should load");
        assert_eq!(settings.log_format, super::LogFormat::Pretty);
        assert_eq!(settings.log_level, super::LogLevel::Debug);
        assert_eq!(settings.request_id_header, "X-Correlation-ID");
        assert!(settings.metrics_enabled);
        assert_eq!(settings.metrics_path, "/internal/metrics");
        assert!(!settings.metrics_require_auth);
        assert!(settings.metrics_include_process);
    }

    #[test]
    fn metrics_path_must_start_with_slash() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_METRICS_PATH", "metrics");
        }

        let error = Settings::from_env().expect_err("invalid metrics path should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains("MEMQUIRA_METRICS_PATH"));
    }

    #[test]
    fn metrics_path_cannot_collide_with_health() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_METRICS_PATH", "/health");
        }

        let error = Settings::from_env().expect_err("reserved metrics path should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains("cannot collide"));
    }

    #[test]
    fn metrics_disabled_by_default_struct() {
        let settings = Settings::default();
        assert!(!settings.metrics_enabled);
        assert_eq!(settings.metrics_path, "/metrics");
        assert!(settings.metrics_require_auth);
        assert!(!settings.metrics_include_process);
    }

    #[test]
    fn load_settings_succeeds_when_dotenv_file_is_missing() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        super::load_settings().expect("missing .env should not fail startup");
    }

    #[test]
    fn load_settings_reads_values_from_dotenv_file() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let dir = std::env::temp_dir().join(format!("memquira-dotenv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        std::fs::write(dir.join(".env"), "MEMQUIRA_PORT=9123\n").expect(".env should be written");

        let original = std::env::current_dir().expect("cwd should exist");
        std::env::set_current_dir(&dir).expect("chdir should succeed");
        let settings = super::load_settings().expect("settings should load from dotenv");
        std::env::set_current_dir(&original).expect("cwd should be restored");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(settings.port, 9123);
    }

    #[test]
    fn dotenv_does_not_override_existing_environment_variables() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        let dir = std::env::temp_dir().join(format!(
            "memquira-dotenv-override-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        std::fs::write(dir.join(".env"), "MEMQUIRA_PORT=9123\n").expect(".env should be written");

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_PORT", "7777") };

        let original = std::env::current_dir().expect("cwd should exist");
        std::env::set_current_dir(&dir).expect("chdir should succeed");
        let settings = super::load_settings().expect("settings should load");
        std::env::set_current_dir(&original).expect("cwd should be restored");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(settings.port, 7777);
    }

    #[test]
    fn qdrant_backend_requires_qdrant_url() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_VECTOR_BACKEND", "qdrant");
            std::env::set_var("MEMQUIRA_QDRANT_URL", "   ");
        }

        let error = Settings::from_env().expect_err("qdrant without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_QDRANT_URL is required when MEMQUIRA_VECTOR_BACKEND=qdrant")
        );
    }

    #[test]
    fn qdrant_collection_defaults_to_memcore_vectors() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_VECTOR_BACKEND", "qdrant");
        }

        let settings = Settings::from_env().expect("qdrant settings should load");
        assert_eq!(settings.vector_backend, VectorBackend::Qdrant);
        assert_eq!(settings.qdrant_url, "http://localhost:6334");
        assert_eq!(settings.qdrant_collection, "memcore_vectors");
    }

    #[test]
    fn loads_qdrant_collection_from_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe {
            std::env::set_var("MEMQUIRA_VECTOR_BACKEND", "qdrant");
            std::env::set_var("MEMQUIRA_QDRANT_COLLECTION", "custom_vectors");
        }

        let settings = Settings::from_env().expect("qdrant collection should load");
        assert_eq!(settings.qdrant_collection, "custom_vectors");
    }

    #[test]
    fn fails_on_invalid_enum_value() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMQUIRA_VECTOR_BACKEND", "bad-backend") };

        let error = Settings::from_env().expect_err("invalid enum should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMQUIRA_VECTOR_BACKEND value")
        );
    }

    #[test]
    fn provider_guardrails_defaults() {
        let settings = Settings::default();
        assert!(settings.provider_guardrails_enabled);
        assert!(!settings.real_provider_calls_enabled);
        assert_eq!(settings.provider_test_mode, ProviderTestMode::MockOnly);
        assert_eq!(settings.provider_max_calls_per_run, 10);
        assert_eq!(settings.provider_max_input_chars, 4000);
        // Raised from 300: extraction honours this ceiling (see the constant).
        assert_eq!(settings.provider_max_output_tokens, 2_048);
        assert_eq!(settings.provider_max_retries_per_call, 1);
        assert!(!settings.allow_real_providers_during_load_tests);
        assert!(!settings.background_jobs_allow_real_providers);
        assert!(!settings.multi_provider_validation_enabled);
        assert!(settings.multi_provider_validation_confirmation.is_none());
    }

    #[test]
    fn invalid_provider_test_mode_fails() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_TEST_MODE", "unsafe_all");
        }
        let error = Settings::from_env().expect_err("invalid test mode should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains("MEMQUIRA_PROVIDER_TEST_MODE"));
    }

    #[test]
    fn operation_header_forwarding_needs_an_exact_host_allow_list() {
        let mut settings = Settings::default();
        assert!(settings.validate().is_ok());
        assert!(!settings.provider_operation_header_enabled);
        settings.provider_operation_header_enabled = true;
        let error = settings
            .validate()
            .expect_err("enabled without hosts must fail");
        assert!(
            error
                .to_string()
                .contains("MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS")
        );
        settings.provider_operation_header_hosts = vec!["vertex-adapter:8790".to_string()];
        assert!(settings.validate().is_ok());
        for bad in [
            "*.internal",
            "vertex-adapter/v1",
            "user@vertex-adapter",
            "vertex adapter",
        ] {
            settings.provider_operation_header_hosts = vec![bad.to_string()];
            assert!(settings.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn operation_header_hosts_are_read_from_the_environment() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED", "true");
            std::env::set_var(
                "MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS",
                " vertex-adapter:8790 , localhost ",
            );
        }
        let settings = Settings::from_env().expect("valid settings");
        assert!(settings.provider_operation_header_enabled);
        assert_eq!(
            settings.provider_operation_header_hosts,
            vec!["vertex-adapter:8790".to_string(), "localhost".to_string()]
        );
    }

    #[test]
    fn provider_guardrail_limits_must_be_positive() {
        let mut settings = Settings::default();
        settings.provider_max_calls_per_run = 0;
        assert!(settings.validate().is_err());
        settings = Settings::default();
        settings.provider_max_input_chars = 0;
        assert!(settings.validate().is_err());
        settings = Settings::default();
        settings.provider_max_output_tokens = 0;
        assert!(settings.validate().is_err());
        settings = Settings::default();
        settings.provider_max_retries_per_call = MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES + 1;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn multi_real_requires_confirmation() {
        let mut settings = Settings::default();
        settings.provider_test_mode = ProviderTestMode::MultiReal;
        settings.multi_provider_validation_enabled = true;
        assert!(settings.validate().is_err());
        settings.multi_provider_validation_confirmation =
            Some(MULTI_PROVIDER_VALIDATION_CONFIRMATION.to_string());
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn loads_provider_guardrail_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        clear_env();
        unsafe {
            std::env::set_var("MEMQUIRA_PROVIDER_TEST_MODE", "single_real");
            std::env::set_var("MEMQUIRA_REAL_PROVIDER_CALLS_ENABLED", "true");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_CALLS_PER_RUN", "6");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_INPUT_CHARS", "500");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_OUTPUT_TOKENS", "256");
            std::env::set_var("MEMQUIRA_PROVIDER_MAX_RETRIES_PER_CALL", "0");
        }
        let settings = Settings::from_env().expect("guardrail env should load");
        assert_eq!(settings.provider_test_mode, ProviderTestMode::SingleReal);
        assert!(settings.real_provider_calls_enabled);
        assert_eq!(settings.provider_max_calls_per_run, 6);
        assert_eq!(settings.provider_max_input_chars, 500);
        assert_eq!(settings.provider_max_output_tokens, 256);
        assert_eq!(settings.provider_max_retries_per_call, 0);
    }
}
