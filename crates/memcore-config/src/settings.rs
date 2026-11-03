use std::env;
use std::fmt;
use std::str::FromStr;

use memcore_common::{MemcoreError, MemcoreResult};

const MEMCORE_ENV: &str = "MEMCORE_ENV";
const MEMCORE_HOST: &str = "MEMCORE_HOST";
const MEMCORE_PORT: &str = "MEMCORE_PORT";
const MEMCORE_STORAGE_MODE: &str = "MEMCORE_STORAGE_MODE";
const MEMCORE_VECTOR_BACKEND: &str = "MEMCORE_VECTOR_BACKEND";
const MEMCORE_FACT_BACKEND: &str = "MEMCORE_FACT_BACKEND";
const MEMCORE_EVENT_BACKEND: &str = "MEMCORE_EVENT_BACKEND";
const MEMCORE_DATABASE_URL: &str = "MEMCORE_DATABASE_URL";
const MEMCORE_POSTGRES_URL: &str = "MEMCORE_POSTGRES_URL";
const MEMCORE_DATABASE_MIGRATIONS_ENABLED: &str = "MEMCORE_DATABASE_MIGRATIONS_ENABLED";
const MEMCORE_DATABASE_MIGRATION_MODE: &str = "MEMCORE_DATABASE_MIGRATION_MODE";
const MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS: &str = "MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS";
const MEMCORE_BACKUP_ENABLED: &str = "MEMCORE_BACKUP_ENABLED";
const MEMCORE_BACKUP_DIR: &str = "MEMCORE_BACKUP_DIR";
const MEMCORE_BACKUP_MAX_FILES: &str = "MEMCORE_BACKUP_MAX_FILES";
const MEMCORE_RESTORE_ENABLED: &str = "MEMCORE_RESTORE_ENABLED";
const MEMCORE_SECURITY_HEADERS_ENABLED: &str = "MEMCORE_SECURITY_HEADERS_ENABLED";
const MEMCORE_MAX_REQUEST_BODY_BYTES: &str = "MEMCORE_MAX_REQUEST_BODY_BYTES";
const MEMCORE_CORS_ENABLED: &str = "MEMCORE_CORS_ENABLED";
const MEMCORE_CORS_ALLOWED_ORIGINS: &str = "MEMCORE_CORS_ALLOWED_ORIGINS";
const MEMCORE_CORS_ALLOWED_METHODS: &str = "MEMCORE_CORS_ALLOWED_METHODS";
const MEMCORE_CORS_ALLOWED_HEADERS: &str = "MEMCORE_CORS_ALLOWED_HEADERS";
const MEMCORE_CORS_EXPOSED_HEADERS: &str = "MEMCORE_CORS_EXPOSED_HEADERS";
const MEMCORE_CORS_ALLOW_CREDENTIALS: &str = "MEMCORE_CORS_ALLOW_CREDENTIALS";
const MEMCORE_QDRANT_URL: &str = "MEMCORE_QDRANT_URL";
const MEMCORE_QDRANT_COLLECTION: &str = "MEMCORE_QDRANT_COLLECTION";
const MEMCORE_QDRANT_EPISODE_COLLECTION: &str = "MEMCORE_QDRANT_EPISODE_COLLECTION";
const MEMCORE_QDRANT_ENSURE_INDEXES: &str = "MEMCORE_QDRANT_ENSURE_INDEXES";
const MEMCORE_QDRANT_WRITE_MODE: &str = "MEMCORE_QDRANT_WRITE_MODE";
const MEMCORE_QDRANT_SEARCH_HNSW_EF: &str = "MEMCORE_QDRANT_SEARCH_HNSW_EF";
/// Upper bound for `MEMCORE_QDRANT_SEARCH_HNSW_EF` (a larger beam is a misconfiguration).
const MAX_QDRANT_SEARCH_HNSW_EF: u64 = 4096;
const MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS: &str =
    "MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS";
/// Upper bound for `MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS`: an exact (brute-force) search
/// scans every point of the tenant, so a larger bound is a misconfiguration.
const MAX_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS: u64 = 1_000_000;
const MEMCORE_EPISODES_ENABLED: &str = "MEMCORE_EPISODES_ENABLED";
const MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED: &str = "MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED";
const MEMCORE_QUERY_ANALYSIS_ENABLED: &str = "MEMCORE_QUERY_ANALYSIS_ENABLED";
const MEMCORE_TEMPORAL_RANKING_ENABLED: &str = "MEMCORE_TEMPORAL_RANKING_ENABLED";
const MEMCORE_TEMPORAL_BOOST: &str = "MEMCORE_TEMPORAL_BOOST";
const MEMCORE_MULTI_QUERY: &str = "MEMCORE_MULTI_QUERY";
const MEMCORE_RERANK_PROVIDER: &str = "MEMCORE_RERANK_PROVIDER";
const MEMCORE_RERANK_CANDIDATES: &str = "MEMCORE_RERANK_CANDIDATES";
const MEMCORE_RERANK_TIMEOUT_MS: &str = "MEMCORE_RERANK_TIMEOUT_MS";
const MEMCORE_RERANK_LOCAL_MODEL_PATH: &str = "MEMCORE_RERANK_LOCAL_MODEL_PATH";
const MEMCORE_RERANK_LOCAL_MODEL_SHA256: &str = "MEMCORE_RERANK_LOCAL_MODEL_SHA256";
const MEMCORE_RERANK_LOCAL_TOKENIZER_PATH: &str = "MEMCORE_RERANK_LOCAL_TOKENIZER_PATH";
const MEMCORE_RERANK_LOCAL_TOKENIZER_SHA256: &str = "MEMCORE_RERANK_LOCAL_TOKENIZER_SHA256";
const MEMCORE_RERANK_LOCAL_THREADS: &str = "MEMCORE_RERANK_LOCAL_THREADS";
const MEMCORE_RERANK_LOCAL_BATCH_SIZE: &str = "MEMCORE_RERANK_LOCAL_BATCH_SIZE";
const MEMCORE_RERANK_BEDROCK_REGION: &str = "MEMCORE_RERANK_BEDROCK_REGION";
const MEMCORE_RERANK_BEDROCK_MODEL_ARN: &str = "MEMCORE_RERANK_BEDROCK_MODEL_ARN";
const MEMCORE_RERANK_BEDROCK_PROFILE: &str = "MEMCORE_RERANK_BEDROCK_PROFILE";
const MEMCORE_QUERY_DECOMPOSITION: &str = "MEMCORE_QUERY_DECOMPOSITION";
const MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS: &str = "MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS";
const MEMCORE_LANCEDB_PATH: &str = "MEMCORE_LANCEDB_PATH";
const MEMCORE_LANCEDB_TABLE: &str = "MEMCORE_LANCEDB_TABLE";
const MEMCORE_LLM_PROVIDER: &str = "MEMCORE_LLM_PROVIDER";
const MEMCORE_LLM_MODEL: &str = "MEMCORE_LLM_MODEL";
const MEMCORE_EMBEDDING_PROVIDER: &str = "MEMCORE_EMBEDDING_PROVIDER";
const MEMCORE_EMBEDDING_MODEL: &str = "MEMCORE_EMBEDDING_MODEL";
const MEMCORE_ENABLE_PII_REDACTION: &str = "MEMCORE_ENABLE_PII_REDACTION";
const MEMCORE_STRUCTURED_EXTRACTION_ENABLED: &str = "MEMCORE_STRUCTURED_EXTRACTION_ENABLED";
const MEMCORE_STRUCTURED_EXTRACTION_POLICY: &str = "MEMCORE_STRUCTURED_EXTRACTION_POLICY";
const MEMCORE_CONSOLIDATION_MODE: &str = "MEMCORE_CONSOLIDATION_MODE";
const MEMCORE_RELATED_FACT_MATCHER: &str = "MEMCORE_RELATED_FACT_MATCHER";
const MEMCORE_CLASSIFICATION_PROMPT: &str = "MEMCORE_CLASSIFICATION_PROMPT";
const MEMCORE_IMPORTANCE_POLICY: &str = "MEMCORE_IMPORTANCE_POLICY";
const MEMCORE_TEMPORAL_REWRITE: &str = "MEMCORE_TEMPORAL_REWRITE";
const MEMCORE_CONTEXT_DEFAULT_PACKING: &str = "MEMCORE_CONTEXT_DEFAULT_PACKING";
const MEMCORE_CONSOLIDATION_CANDIDATES: &str = "MEMCORE_CONSOLIDATION_CANDIDATES";
const MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR: &str = "MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR";
const MEMCORE_ENTITIES_ENABLED: &str = "MEMCORE_ENTITIES_ENABLED";
const MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD: &str = "MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD";
const MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED: &str = "MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED";
const MEMCORE_SEARCH_STRATEGY: &str = "MEMCORE_SEARCH_STRATEGY";
const MEMCORE_RRF_K: &str = "MEMCORE_RRF_K";
const MEMCORE_SEARCH_MAX_LIMIT: &str = "MEMCORE_SEARCH_MAX_LIMIT";
const MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY: &str = "MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY";
const MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED: &str =
    "MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED";
const MEMCORE_INGEST_JOURNAL_ENABLED: &str = "MEMCORE_INGEST_JOURNAL_ENABLED";
const MEMCORE_INGEST_RECOVERY_STALE_SECS: &str = "MEMCORE_INGEST_RECOVERY_STALE_SECS";
const MEMCORE_INGEST_RECOVERY_INTERVAL_SECS: &str = "MEMCORE_INGEST_RECOVERY_INTERVAL_SECS";
const MEMCORE_INGEST_INFLIGHT_WAIT_MS: &str = "MEMCORE_INGEST_INFLIGHT_WAIT_MS";
const MEMCORE_INGEST_RECEIPT_TTL_SECS: &str = "MEMCORE_INGEST_RECEIPT_TTL_SECS";
const MEMCORE_LOCAL_EMBEDDING_MODEL_DIR: &str = "MEMCORE_LOCAL_EMBEDDING_MODEL_DIR";
const MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY: &str = "MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY";
const MEMCORE_RRF_WEIGHT_SEMANTIC: &str = "MEMCORE_RRF_WEIGHT_SEMANTIC";
const MEMCORE_RRF_WEIGHT_KEYWORD: &str = "MEMCORE_RRF_WEIGHT_KEYWORD";
const MEMCORE_EXTRACTION_WINDOW_TURNS: &str = "MEMCORE_EXTRACTION_WINDOW_TURNS";
const MEMCORE_EXTRACTION_WINDOW_OVERLAP: &str = "MEMCORE_EXTRACTION_WINDOW_OVERLAP";
const MEMCORE_EXTRACTION_CONCURRENCY: &str = "MEMCORE_EXTRACTION_CONCURRENCY";
const MEMCORE_MIN_IMPORTANCE: &str = "MEMCORE_MIN_IMPORTANCE";
const MEMCORE_AUTH_ENABLED: &str = "MEMCORE_AUTH_ENABLED";
const MEMCORE_AUTH_MODE: &str = "MEMCORE_AUTH_MODE";
const MEMCORE_DEV_API_KEY: &str = "MEMCORE_DEV_API_KEY";
const MEMCORE_API_KEY_PEPPER: &str = "MEMCORE_API_KEY_PEPPER";
const MEMCORE_RATE_LIMIT_ENABLED: &str = "MEMCORE_RATE_LIMIT_ENABLED";
const MEMCORE_ADMISSION_CONTROL: &str = "MEMCORE_ADMISSION_CONTROL";
const MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS: &str = "MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS";
const MEMCORE_ADMISSION_RETRY_AFTER_SECONDS: &str = "MEMCORE_ADMISSION_RETRY_AFTER_SECONDS";
const MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY: &str = "MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY";
const MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY: &str =
    "MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY";
const MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS: &str = "MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS";
const MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT: &str = "MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT";
const MEMCORE_ADMISSION_READ_MAX_QUEUE: &str = "MEMCORE_ADMISSION_READ_MAX_QUEUE";
const MEMCORE_ADMISSION_READ_DEADLINE_MS: &str = "MEMCORE_ADMISSION_READ_DEADLINE_MS";
const MEMCORE_ADMISSION_WRITE_MAX_IN_FLIGHT: &str = "MEMCORE_ADMISSION_WRITE_MAX_IN_FLIGHT";
const MEMCORE_ADMISSION_WRITE_MAX_QUEUE: &str = "MEMCORE_ADMISSION_WRITE_MAX_QUEUE";
const MEMCORE_ADMISSION_WRITE_DEADLINE_MS: &str = "MEMCORE_ADMISSION_WRITE_DEADLINE_MS";
const MEMCORE_ADMISSION_ADMIN_MAX_IN_FLIGHT: &str = "MEMCORE_ADMISSION_ADMIN_MAX_IN_FLIGHT";
const MEMCORE_ADMISSION_ADMIN_MAX_QUEUE: &str = "MEMCORE_ADMISSION_ADMIN_MAX_QUEUE";
const MEMCORE_ADMISSION_ADMIN_DEADLINE_MS: &str = "MEMCORE_ADMISSION_ADMIN_DEADLINE_MS";
const MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE: &str = "MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE";
const MEMCORE_LOG_FORMAT: &str = "MEMCORE_LOG_FORMAT";
const MEMCORE_LOG_LEVEL: &str = "MEMCORE_LOG_LEVEL";
const MEMCORE_REQUEST_ID_HEADER: &str = "MEMCORE_REQUEST_ID_HEADER";
const MEMCORE_METRICS_ENABLED: &str = "MEMCORE_METRICS_ENABLED";
const MEMCORE_METRICS_PATH: &str = "MEMCORE_METRICS_PATH";
const MEMCORE_METRICS_REQUIRE_AUTH: &str = "MEMCORE_METRICS_REQUIRE_AUTH";
const MEMCORE_METRICS_INCLUDE_PROCESS: &str = "MEMCORE_METRICS_INCLUDE_PROCESS";
const MEMCORE_RETENTION_ENABLED: &str = "MEMCORE_RETENTION_ENABLED";
const DEFAULT_METRICS_PATH: &str = "/metrics";
const MEMCORE_FACT_RETENTION_DAYS: &str = "MEMCORE_FACT_RETENTION_DAYS";
const MEMCORE_EVENT_RETENTION_DAYS: &str = "MEMCORE_EVENT_RETENTION_DAYS";
const MEMCORE_CONTEXT_CACHE_ENABLED: &str = "MEMCORE_CONTEXT_CACHE_ENABLED";
const MEMCORE_CONTEXT_CACHE_BACKEND: &str = "MEMCORE_CONTEXT_CACHE_BACKEND";
const MEMCORE_CONTEXT_CACHE_TTL_SECONDS: &str = "MEMCORE_CONTEXT_CACHE_TTL_SECONDS";
const MEMCORE_CONTEXT_CACHE_MAX_ENTRIES: &str = "MEMCORE_CONTEXT_CACHE_MAX_ENTRIES";
const MEMCORE_CONTEXT_CACHE_KEY_PREFIX: &str = "MEMCORE_CONTEXT_CACHE_KEY_PREFIX";
const MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED: &str =
    "MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED";
const MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS: &str =
    "MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS";
const MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED: &str =
    "MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED";
const MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS: &str = "MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS";
const MEMCORE_CONTEXT_CACHE_METRICS_ENABLED: &str = "MEMCORE_CONTEXT_CACHE_METRICS_ENABLED";
const MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS: &str =
    "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS";
const MEMCORE_PROVIDER_TIMEOUT_SECONDS: &str = "MEMCORE_PROVIDER_TIMEOUT_SECONDS";
const MEMCORE_PROVIDER_MAX_RETRIES: &str = "MEMCORE_PROVIDER_MAX_RETRIES";
const MEMCORE_PROVIDER_INITIAL_BACKOFF_MS: &str = "MEMCORE_PROVIDER_INITIAL_BACKOFF_MS";
const MEMCORE_PROVIDER_MAX_BACKOFF_MS: &str = "MEMCORE_PROVIDER_MAX_BACKOFF_MS";
const MEMCORE_PROVIDER_BACKOFF_MULTIPLIER: &str = "MEMCORE_PROVIDER_BACKOFF_MULTIPLIER";
const MEMCORE_PROVIDER_RETRY_JITTER_ENABLED: &str = "MEMCORE_PROVIDER_RETRY_JITTER_ENABLED";
const MEMCORE_PROVIDER_FALLBACK_ENABLED: &str = "MEMCORE_PROVIDER_FALLBACK_ENABLED";
const MEMCORE_LLM_FALLBACK_ORDER: &str = "MEMCORE_LLM_FALLBACK_ORDER";
const MEMCORE_EMBEDDING_FALLBACK_ORDER: &str = "MEMCORE_EMBEDDING_FALLBACK_ORDER";
const MEMCORE_SUMMARIZER_FALLBACK_ORDER: &str = "MEMCORE_SUMMARIZER_FALLBACK_ORDER";
const MEMCORE_PROVIDER_CIRCUIT_BREAKER_ENABLED: &str = "MEMCORE_PROVIDER_CIRCUIT_BREAKER_ENABLED";
const MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD: &str =
    "MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD";
const MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS: &str =
    "MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS";
const MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS: &str =
    "MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS";
const MEMCORE_PROVIDER_USAGE_METRICS_ENABLED: &str = "MEMCORE_PROVIDER_USAGE_METRICS_ENABLED";
const MEMCORE_PROVIDER_COST_TRACKING_ENABLED: &str = "MEMCORE_PROVIDER_COST_TRACKING_ENABLED";
const MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED: &str =
    "MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED";
const MEMCORE_PROVIDER_USAGE_RETENTION_DAYS: &str = "MEMCORE_PROVIDER_USAGE_RETENTION_DAYS";
const MEMCORE_QUOTAS_ENABLED: &str = "MEMCORE_QUOTAS_ENABLED";
const MEMCORE_MAX_USERS_PER_ORG: &str = "MEMCORE_MAX_USERS_PER_ORG";
const MEMCORE_MAX_MEMORIES_PER_USER: &str = "MEMCORE_MAX_MEMORIES_PER_USER";
const MEMCORE_MAX_MEMORIES_PER_ORG: &str = "MEMCORE_MAX_MEMORIES_PER_ORG";
const MEMCORE_DAILY_PROVIDER_REQUEST_LIMIT: &str = "MEMCORE_DAILY_PROVIDER_REQUEST_LIMIT";
const MEMCORE_DAILY_PROVIDER_TOKEN_LIMIT: &str = "MEMCORE_DAILY_PROVIDER_TOKEN_LIMIT";
const MEMCORE_PROVIDER_GUARDRAILS_ENABLED: &str = "MEMCORE_PROVIDER_GUARDRAILS_ENABLED";
const MEMCORE_REAL_PROVIDER_CALLS_ENABLED: &str = "MEMCORE_REAL_PROVIDER_CALLS_ENABLED";
const MEMCORE_PROVIDER_TEST_MODE: &str = "MEMCORE_PROVIDER_TEST_MODE";
const MEMCORE_PROVIDER_MAX_CALLS_PER_RUN: &str = "MEMCORE_PROVIDER_MAX_CALLS_PER_RUN";
const MEMCORE_PROVIDER_MAX_INPUT_CHARS: &str = "MEMCORE_PROVIDER_MAX_INPUT_CHARS";
const MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS: &str = "MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS";
const MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS: &str = "MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS";
const MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS: &str = "MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS";
const MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL: &str = "MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL";
const MEMCORE_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS: &str =
    "MEMCORE_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS";
const MEMCORE_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS: &str =
    "MEMCORE_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS";
const MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED: &str = "MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED";
const MEMCORE_MULTI_PROVIDER_VALIDATION_CONFIRMATION: &str =
    "MEMCORE_MULTI_PROVIDER_VALIDATION_CONFIRMATION";
const MEMCORE_BACKGROUND_JOBS_ENABLED: &str = "MEMCORE_BACKGROUND_JOBS_ENABLED";
const MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS: &str =
    "MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS";
const MEMCORE_BACKGROUND_JOB_ORG_IDS: &str = "MEMCORE_BACKGROUND_JOB_ORG_IDS";
const MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED: &str = "MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED";
const MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS: &str =
    "MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS";
const MEMCORE_PROVIDER_USAGE_RETENTION_JOB_ENABLED: &str =
    "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_ENABLED";
const MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS: &str =
    "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS";
const MEMCORE_MEMORY_RETENTION_JOB_ENABLED: &str = "MEMCORE_MEMORY_RETENTION_JOB_ENABLED";
const MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS: &str =
    "MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS";
const MEMCORE_BACKGROUND_JOB_HISTORY_ENABLED: &str = "MEMCORE_BACKGROUND_JOB_HISTORY_ENABLED";
const MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS: &str =
    "MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS";
const MEMCORE_BACKGROUND_JOB_LOCK_ENABLED: &str = "MEMCORE_BACKGROUND_JOB_LOCK_ENABLED";
const MEMCORE_BACKGROUND_JOB_LOCK_BACKEND: &str = "MEMCORE_BACKGROUND_JOB_LOCK_BACKEND";
const MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS: &str = "MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS";
const MEMCORE_BACKGROUND_JOB_LOCK_OWNER_ID: &str = "MEMCORE_BACKGROUND_JOB_LOCK_OWNER_ID";
const MEMCORE_BACKGROUND_JOB_RETRIES_ENABLED: &str = "MEMCORE_BACKGROUND_JOB_RETRIES_ENABLED";
const MEMCORE_BACKGROUND_JOB_MAX_RETRIES: &str = "MEMCORE_BACKGROUND_JOB_MAX_RETRIES";
const MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS: &str = "MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS";
const MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS: &str = "MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS";
const MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER: &str = "MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER";
const MEMCORE_BACKGROUND_JOB_RETRY_JITTER_ENABLED: &str =
    "MEMCORE_BACKGROUND_JOB_RETRY_JITTER_ENABLED";
const MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS: &str = "MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS";
const MEMCORE_ALLOW_MOCK: &str = "MEMCORE_ALLOW_MOCK";
const MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS: &str =
    "MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS";
const MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS: &str =
    "MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS";
const MEMCORE_REDIS_URL: &str = "MEMCORE_REDIS_URL";
const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
const MEMCORE_LLM_BASE_URL: &str = "MEMCORE_LLM_BASE_URL";
const MEMCORE_LLM_API_KEY: &str = "MEMCORE_LLM_API_KEY";
const MEMCORE_LLM_ALLOW_NO_API_KEY: &str = "MEMCORE_LLM_ALLOW_NO_API_KEY";
const MEMCORE_EMBEDDING_BASE_URL: &str = "MEMCORE_EMBEDDING_BASE_URL";
const MEMCORE_EMBEDDING_API_KEY: &str = "MEMCORE_EMBEDDING_API_KEY";
const MEMCORE_EMBEDDING_ALLOW_NO_API_KEY: &str = "MEMCORE_EMBEDDING_ALLOW_NO_API_KEY";
const MEMCORE_EMBEDDING_DIMENSIONS: &str = "MEMCORE_EMBEDDING_DIMENSIONS";
const MEMCORE_EMBEDDING_SEND_DIMENSIONS: &str = "MEMCORE_EMBEDDING_SEND_DIMENSIONS";
const MEMCORE_EMBEDDING_NORMALIZE: &str = "MEMCORE_EMBEDDING_NORMALIZE";
const MEMCORE_EMBEDDING_QUERY_PREFIX: &str = "MEMCORE_EMBEDDING_QUERY_PREFIX";
const MEMCORE_EMBEDDING_DOCUMENT_PREFIX: &str = "MEMCORE_EMBEDDING_DOCUMENT_PREFIX";
const MEMCORE_EMBEDDING_MODEL_REVISION: &str = "MEMCORE_EMBEDDING_MODEL_REVISION";
const MEMCORE_EMBEDDING_SERVING_VARIANT: &str = "MEMCORE_EMBEDDING_SERVING_VARIANT";
const MEMCORE_EMBEDDING_DEDUP_THRESHOLD: &str = "MEMCORE_EMBEDDING_DEDUP_THRESHOLD";
const MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP: &str = "MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP";
const MEMCORE_LLM_JSON_MODE: &str = "MEMCORE_LLM_JSON_MODE";
const MEMCORE_PROVIDER_PROBE: &str = "MEMCORE_PROVIDER_PROBE";
const MEMCORE_EXPORT_MAX_RECORDS: &str = "MEMCORE_EXPORT_MAX_RECORDS";
const MEMCORE_REQUIRE_QUALITY_TIER: &str = "MEMCORE_REQUIRE_QUALITY_TIER";
const MEMCORE_LLM_STARTUP_PROBE: &str = "MEMCORE_LLM_STARTUP_PROBE";
const OPENAI_BASE_URL: &str = "OPENAI_BASE_URL";

pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_REQUEST_ID_HEADER: &str = "X-Request-ID";
pub const DEFAULT_CONTEXT_CACHE_KEY_PREFIX: &str = "memcore";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 1_048_576;

/// Default ceiling on any single LLM call's output (`MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS`), and the
/// default fact-extraction limit.
///
/// It was 300, and extraction hardcoded the same 300 regardless of this setting. Measured on 8,700
/// measured extraction responses: a fact costs 121–183 output tokens (p50–p99), a
/// complete response carried up to 6 facts, and responses cut off at 300 tokens had already listed
/// up to 4. At 300, a noticeable share of responses was truncated — each one
/// losing every fact in its batch. 2,048 fits about 11 facts at the p99 cost per fact. Output is
/// billed as generated, so the higher ceiling costs nothing on responses that were not truncated.
pub const DEFAULT_PROVIDER_MAX_OUTPUT_TOKENS: usize = 2_048;

/// Default fact-classification limit (`MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS`). Classification
/// returns one small JSON decision: across 9,222 measured responses the longest was 92 tokens and
/// none was truncated, so the previous 300 is kept.
pub const DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS: usize = 300;
pub const DEFAULT_CORS_ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "OPTIONS"];
pub const DEFAULT_CORS_ALLOWED_HEADERS: &[&str] = &[
    "Authorization",
    "Content-Type",
    "X-Organization-ID",
    "X-Memcore-Test-Source",
];

/// Exact confirmation required for `multi_real` provider test mode.
pub const MULTI_PROVIDER_VALIDATION_CONFIRMATION: &str =
    "I_UNDERSTAND_THIS_WILL_USE_PROVIDER_CREDITS";

/// Safe upper bound for `MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL`.
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

/// Retrieval strategy for searches that name none (`MEMCORE_SEARCH_STRATEGY`).
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
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "hybrid" => Ok(Self::Hybrid),
            "semantic" | "vector" => Ok(Self::Semantic),
            "fulltext" => Ok(Self::Fulltext),
            "rrf" => Ok(Self::Rrf),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_SEARCH_STRATEGY value: {value}"
            ))),
        }
    }
}

/// Retention policy of the structured extraction prompt (`MEMCORE_STRUCTURED_EXTRACTION_POLICY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredExtractionPolicy {
    Atomic,
    Retentive,
}

/// How a new extracted fact is reconciled with the user's stored facts
/// (`MEMCORE_CONSOLIDATION_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationMode {
    /// The original path (text and embedding dedup, then the classifier), unchanged. Default.
    Legacy,
    /// Related-fact candidates, fast paths, one model decision
    /// (ADD / UPDATE / INVALIDATE / NOOP) validated in Rust, history kept on invalidation.
    LlmV2,
}

/// How the legacy ingest path finds the stored facts a candidate is classified against
/// (`MEMCORE_RELATED_FACT_MATCHER`).
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
/// (`MEMCORE_TEMPORAL_REWRITE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalRewriteSetting {
    /// The settled date is prefixed and the model's time expression removed, whatever it is. Default.
    Legacy,
    /// Opt-in: only a time expression the resolver recognised is removed; a span with no time in it gives no event time; a date that is an attribute's value stays in the sentence. Not answer-validated.
    Preserving,
}

/// Which extracted candidates pass the importance threshold (`MEMCORE_IMPORTANCE_POLICY`, Memcore
/// V2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportancePolicySetting {
    /// A candidate is stored when its adjusted importance reaches `MEMCORE_MIN_IMPORTANCE`. Default.
    Legacy,
    /// Opt-in: also store a non-conversational, non-system fact the extractor rated at least medium (0.5). Not answer-validated.
    Durable,
}

/// Which instructions fact-classification requests carry (`MEMCORE_CLASSIFICATION_PROMPT`, Memcore
/// V2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationPromptSetting {
    /// The production fact-classification instructions, unchanged. Default.
    Legacy,
    /// Experimental and opt-in: the same JSON contract with Add / Update / Delete / NoOp defined and invented examples. Not measured with a real model.
    OperationsV1,
}

/// The packing `POST /api/v1/memories/context` uses when a request does not name one
/// (`MEMCORE_CONTEXT_DEFAULT_PACKING`). An explicit request `packing` always wins.
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

/// How fact and episode vectors are written to Qdrant (`MEMCORE_QDRANT_WRITE_MODE`).
/// Unrelated to `MEMCORE_QDRANT_ENSURE_INDEXES`, which only controls index creation.
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
    /// Expected vector size (`MEMCORE_EMBEDDING_DIMENSIONS`); default from the model name.
    pub dimensions: Option<usize>,
    /// Send the `dimensions` request field (`MEMCORE_EMBEDDING_SEND_DIMENSIONS`, default true;
    /// some servers reject it).
    pub send_dimensions: bool,
    /// L2-normalise returned vectors in memcore (`MEMCORE_EMBEDDING_NORMALIZE`, default false).
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
/// (`MEMCORE_LLM_JSON_MODE`). The `/responses` transport is unaffected.
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
    /// Mock providers (`MEMCORE_ALLOW_MOCK=true`): tests and development only.
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
/// The reference store's vector size: memcore's default for `bedrock-titan-embed-v2` (the gateway
/// zero-pads Titan's 1,024-d output to it, as the validated proxy did).
pub const REFERENCE_EMBEDDING_DIMENSIONS: usize = 1536;
/// The reference profile's query-time Qdrant HNSW beam (`MEMCORE_QDRANT_SEARCH_HNSW_EF`): the value of
/// the maintainers' acceptance runs on SQLite and PostgreSQL.
/// The reference requires exactly this value; unset (the application default and the operational
/// rollback) and every other beam are `compatible-unvalidated`.
pub const REFERENCE_QDRANT_SEARCH_HNSW_EF: u64 = 256;

/// Deployment-level provider configuration per role. Keys come from the
/// deployment environment only, never from tenants or requests.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRoleSettings {
    pub llm: RoleEndpoint,
    /// `MEMCORE_LLM_JSON_MODE` (`json_object` default, or `json_schema`).
    pub llm_json_mode: LlmJsonMode,
    pub embedding: RoleEndpoint,
    pub embedding_options: EmbeddingRoleSettings,
    /// Allow plain `http://` to a non-loopback endpoint (`MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP`).
    pub allow_insecure_http: bool,
    /// Startup embedding probe and identity fingerprint (`MEMCORE_PROVIDER_PROBE`, default on).
    pub probe: bool,
    /// Refuse to start unless the configuration has this tier (`MEMCORE_REQUIRE_QUALITY_TIER`).
    pub require_quality_tier: Option<QualityTier>,
    /// One small chat call at startup that must return valid JSON (`MEMCORE_LLM_STARTUP_PROBE`,
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
) -> MemcoreResult<()> {
    let invalid = |message: String| Err(MemcoreError::ValidationError(message));
    let parsed = match url::Url::parse(endpoint.base_url.trim()) {
        Ok(parsed) => parsed,
        Err(_) => {
            return invalid(format!(
                "MEMCORE_{role}_BASE_URL (or OPENAI_BASE_URL) is not a valid URL"
            ));
        }
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return invalid(format!("MEMCORE_{role}_BASE_URL must use http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return invalid(format!(
            "MEMCORE_{role}_BASE_URL must not embed credentials; use MEMCORE_{role}_API_KEY"
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return invalid(format!(
            "MEMCORE_{role}_BASE_URL must not contain a query or fragment"
        ));
    }
    let Some(host) = parsed.host_str() else {
        return invalid(format!("MEMCORE_{role}_BASE_URL has no host"));
    };
    let loopback = is_loopback_host(host);
    let has_key = endpoint
        .api_key
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty());
    if !has_key && !loopback && !allow_no_api_key {
        return invalid(format!(
            "MEMCORE_{role}_API_KEY (or OPENAI_API_KEY) is required for {}; set \
             MEMCORE_{role}_ALLOW_NO_API_KEY=true only for a server that needs no authentication",
            redacted_url(&endpoint.base_url)
        ));
    }
    if parsed.scheme() == "http" && !loopback && has_key && !allow_insecure_http {
        return invalid(format!(
            "refusing to send the {role} API key over plain http to {}; use https or set \
             MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP=true",
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
    /// `MEMCORE_ADMISSION_CONTROL` (`on` default | `off`). `off` is a rollback switch only; a
    /// release profile keeps it on (D-L5).
    pub enabled: bool,
    pub read: AdmissionClassLimits,
    pub write: AdmissionClassLimits,
    pub admin: AdmissionClassLimits,
    /// Longest wait for a slot before a 503 (`MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS`).
    pub queue_timeout_ms: u64,
    /// `Retry-After` of an overload 503 (`MEMCORE_ADMISSION_RETRY_AFTER_SECONDS`).
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

/// Multi-query mode for candidate lists (`MEMCORE_MULTI_QUERY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiQuerySetting {
    /// Default: the original query only.
    None,
    /// Clause sub-queries from the deterministic analyzer; no model call.
    Deterministic,
}

/// Reranker for candidate lists (`MEMCORE_RERANK_PROVIDER`). `local` and
/// `bedrock` also need a build with the `local-rerank` / `bedrock-rerank` feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankProviderSetting {
    None,
    /// bge-reranker-v2-m3 INT8 on ONNX Runtime, from local files verified by checksum.
    Local,
    /// Amazon Bedrock Rerank 1.0 (Agents Runtime `Rerank`, SigV4).
    Bedrock,
}

/// Query decomposition (`MEMCORE_QUERY_DECOMPOSITION`). Only generic plumbing exists:
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
    /// Whether the provider runs a model inside memcore (local embeddings). Local
    /// providers need `MEMCORE_LOCAL_EMBEDDING_MODEL_DIR` and the Qdrant backend, whose stores
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
    /// Maximum number of memcore backup files retained in `backup_dir`.
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
    /// (`MEMCORE_EPISODES_ENABLED`, default `false`): store every added message as an
    /// episode (source-message-level memory) before extraction, and serve episode search. Off keeps
    /// every existing behaviour.
    pub episodes_enabled: bool,
    /// (`MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED`, default `false`): serve the
    /// EXPERIMENTAL `POST /api/v1/memories/candidates` (facts and episodes as one E-merge-ordered
    /// candidate list; needs episodes). Not a stable retrieval contract; off leaves every existing
    /// endpoint unchanged.
    pub experimental_candidates_enabled: bool,
    /// (`MEMCORE_QUERY_ANALYSIS_ENABLED`, default `false`): run the deterministic query
    /// analyzer on candidate requests. Needs the candidates endpoint.
    pub query_analysis_enabled: bool,
    /// (`MEMCORE_TEMPORAL_RANKING_ENABLED`, default `false`): event-time ranking of
    /// candidate lists. Needs query analysis.
    pub temporal_ranking_enabled: bool,
    /// (`MEMCORE_TEMPORAL_BOOST`, default 0.05, range (0, 1]): the boost added to
    /// the raw score of facts whose event time overlaps the question's date range.
    pub temporal_boost: f64,
    /// (`MEMCORE_MULTI_QUERY`, default `none`).
    pub multi_query: MultiQuerySetting,
    /// (`MEMCORE_RERANK_PROVIDER`, default `none`).
    pub rerank_provider: RerankProviderSetting,
    /// (`MEMCORE_RERANK_CANDIDATES`, default 60, range 1-100): the rerank / temporal
    /// window.
    pub rerank_candidates: usize,
    /// (`MEMCORE_RERANK_TIMEOUT_MS`, default 30000, range 100-600000): bound on one reranker
    /// call; on expiry the prior order is kept.
    pub rerank_timeout_ms: u64,
    /// Local reranker files and checksums (`MEMCORE_RERANK_LOCAL_*`); required for `local`.
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
    /// (`MEMCORE_QUERY_DECOMPOSITION`, default `none`).
    pub query_decomposition: QueryDecompositionSetting,
    /// Bound on the one decomposition call (`MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS`, default 10000).
    pub query_decomposition_timeout_ms: u64,
    /// Qdrant collection for episode vectors (`MEMCORE_QDRANT_EPISODE_COLLECTION`), default
    /// `<MEMCORE_QDRANT_COLLECTION>_episodes`. Must differ from the fact collection.
    pub qdrant_episode_collection: String,
    /// Ensure the Qdrant payload indexes (keyword `org_id`, `user_id`, `fact_id`; `memory_type` on
    /// the fact collection) when a collection is opened or created
    /// (`MEMCORE_QDRANT_ENSURE_INDEXES`, default true). Only missing indexes are created; points are
    /// never rewritten. `false` is the escape hatch: no index is created or checked.
    pub qdrant_ensure_indexes: bool,
    /// Qdrant write mode (`MEMCORE_QDRANT_WRITE_MODE`, `optimized` (default) | `legacy`).
    /// `legacy` is the rollback to the earlier filtered pre-delete on every write.
    pub qdrant_write_mode: QdrantWriteModeSetting,
    /// Query-time HNSW beam of every Qdrant vector search, facts and episodes
    /// (`MEMCORE_QDRANT_SEARCH_HNSW_EF`). Unset (the application default) sends no
    /// search parameter, so Qdrant uses its own default, exactly the earlier request. The validated
    /// reference profile sets `REFERENCE_QDRANT_SEARCH_HNSW_EF` (256); unsetting it is the operational
    /// rollback (a restart, no reindex) and makes the configuration `compatible-unvalidated`.
    /// Changes no index, write or payload; only how many graph candidates a search explores.
    pub qdrant_search_hnsw_ef: Option<u64>,
    /// EXPERIMENTAL bounded exact search (`MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS`, launch
    /// No count, the historical search request. Set to N: before
    /// every fact / episode vector search, the tenant's points in the queried collection are counted
    /// (exact count, the same `org_id` + `user_id` filter); a tenant with <= N points is searched with
    /// Qdrant `params.exact = true`, a larger one with the unchanged approximate request. Never part of
    /// the launch reference (any value is `compatible-unvalidated`); unsetting it is the rollback.
    pub qdrant_exact_search_max_tenant_points: Option<u64>,
    /// Retention policy of the structured extraction prompt. Default `Retentive`, adopted after
    /// acceptance; `Atomic` is the original Part B prompt.
    pub structured_extraction_policy: StructuredExtractionPolicy,
    /// Consolidation mode (`MEMCORE_CONSOLIDATION_MODE`, default `legacy`). `llm_v2` also
    /// makes search and context leave out invalidated facts unless a request asks for history.
    pub consolidation_mode: ConsolidationMode,
    /// Legacy-path related-fact matcher (`MEMCORE_RELATED_FACT_MATCHER`, default `legacy`).
    pub related_fact_matcher: RelatedFactMatcher,
    /// Fact-classification instructions (`MEMCORE_CLASSIFICATION_PROMPT`, default `legacy`).
    pub classification_prompt: ClassificationPromptSetting,
    /// Importance threshold policy (`MEMCORE_IMPORTANCE_POLICY`, default `legacy`).
    pub importance_policy: ImportancePolicySetting,
    /// Dated-fact sentence rewrite (`MEMCORE_TEMPORAL_REWRITE`, default `legacy`).
    pub temporal_rewrite: TemporalRewriteSetting,
    /// Default packing of the server-side context endpoint (`MEMCORE_CONTEXT_DEFAULT_PACKING`, default
    /// `prefix`, the reference packing; `fill` / `evidence` opt in without a migration or data change).
    pub context_default_packing: ContextPacking,
    /// Related-fact candidates a v2 decision sees (`MEMCORE_CONSOLIDATION_CANDIDATES`, default 10,
    /// range 1-50).
    pub consolidation_candidates: usize,
    /// v2 fast path: with no candidate at or above this vector similarity the new fact is added
    /// without a model call (`MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR`, default 0.5, range 0-1).
    pub consolidation_similarity_floor: f32,
    /// Entity layer (`MEMCORE_ENTITIES_ENABLED`, default false): a separate entity
    /// annotation request per extraction window, deterministic resolution and entity storage. Off:
    /// no entity rows, no annotation or confirmation requests.
    pub entities_enabled: bool,
    /// Name-embedding similarity at or above which a new mention is a candidate match for a stored
    /// entity of a compatible type (`MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD`, default 0.70, range
    /// 0-1). Predeclared before any entity measurement.
    pub entity_name_similarity_threshold: f32,
    /// Ask the model to confirm a candidate match (at most one request per new entity,
    /// `MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED`, default true). Off: a candidate match that is not
    /// an exact name or alias creates a new entity.
    pub entity_llm_confirmation_enabled: bool,
    /// Retrieval strategy a search uses when the request names none (`MEMCORE_SEARCH_STRATEGY`).
    /// Default `Hybrid`, the original fused search.
    pub search_strategy: SearchStrategy,
    /// Reciprocal Rank Fusion constant `k` (`MEMCORE_RRF_K`, default 60): a candidate at rank r in
    /// a list contributes 1 / (k + r).
    pub rrf_k: f32,
    /// Highest search `limit` accepted (`MEMCORE_SEARCH_MAX_LIMIT`). Default 50, the public
    /// limit; configurable within 50–200. Requests up to 50 behave exactly as before; above 50 each
    /// retrieval leg fetches exactly the requested limit.
    pub search_max_limit: usize,
    /// (`MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY`, default 0 = off): how many search-query
    /// embeddings to keep in process, keyed by the embedding provider's identity, the organisation
    /// and the exact query (least recently read evicted; at most 1,000,000).
    pub query_embedding_cache_capacity: usize,
    /// `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED` (default false =
    /// the previous path): on the legacy ingest path, a candidate whose related-fact lookup returns
    /// no fact is added without the classification call (the call can only lead to an Add there);
    /// with related facts the classifier runs unchanged.
    pub classification_skip_when_no_related: bool,
    /// (`MEMCORE_INGEST_JOURNAL_ENABLED`, default true): every add request journals
    /// its fact operations before applying them and commits only when it succeeds, so a failed,
    /// cancelled or interrupted request is rolled back (SQL stores: migration 0016). `false` is a
    /// rollback switch to the V2 behaviour (a failed request's adds are compensated in-request).
    pub ingest_journal_enabled: bool,
    /// (`MEMCORE_INGEST_RECOVERY_STALE_SECS`, default 300): a journalled operation
    /// untouched this long is abandoned and the recovery sweep rolls it back. Must exceed the write
    /// admission deadline.
    pub ingest_recovery_stale_secs: u64,
    /// (`MEMCORE_INGEST_RECOVERY_INTERVAL_SECS`, default 60): how often the recovery
    /// sweep runs (also once at startup).
    pub ingest_recovery_interval_secs: u64,
    /// (`MEMCORE_INGEST_INFLIGHT_WAIT_MS`, default 10,000): longest a request waits for
    /// another request's uncommitted fact before answering `409 CONFLICT`.
    pub ingest_inflight_wait_ms: u64,
    /// closure (`MEMCORE_INGEST_RECEIPT_TTL_SECS`, default 86,400): how long the
    /// receipt of a successful add request with stable message ids is kept, so a retry of that exact
    /// request (e.g. after a lost response) returns the recorded result instead of running again.
    pub ingest_receipt_ttl_secs: u64,
    /// Records a complete export may hold (`MEMCORE_EXPORT_MAX_RECORDS`, default 100,000);
    /// a larger complete export is refused, never truncated.
    pub export_max_records: usize,
    /// Directory holding a local embedding model's pinned files
    /// (`MEMCORE_LOCAL_EMBEDDING_MODEL_DIR`); required by local providers. The files are verified
    /// against their SHA-256 at load; nothing is ever downloaded.
    pub local_embedding_model_dir: Option<String>,
    /// Pending local-inference jobs before requests are refused
    /// (`MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY`, default 16, 1-1024).
    pub local_embedding_queue_capacity: usize,
    /// RRF weight of the vector leg (`MEMCORE_RRF_WEIGHT_SEMANTIC`, default 1.0, >= 0).
    pub rrf_weight_semantic: f32,
    /// RRF weight of the full-text leg (`MEMCORE_RRF_WEIGHT_KEYWORD`, default 1.0, >= 0).
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
    /// process-local `memory` cache (`MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS`). A statement,
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
    /// At least one role uses the mock provider, explicitly allowed by `MEMCORE_ALLOW_MOCK=true`.
    MockAllowed,
}

const MOCK_PROVIDER_REFUSAL: &str = "mock providers return fake extractions and fake embeddings, \
     so stored memories are silently wrong (distinct facts collapse as duplicates)";

impl Settings {
    /// Server-startup provider check. The settings default to the mock LLM and embedding providers
    /// so tests need no credentials; a server must never run on them by accident.
    ///
    /// - A mock provider in any role is refused unless `MEMCORE_ALLOW_MOCK=true`.
    /// - A fallback order that mixes `mock` with a real provider is always refused: a failing real
    ///   provider would otherwise be replaced silently by the mock model.
    pub fn check_startup_providers(&self) -> MemcoreResult<ProviderStartupMode> {
        let roles: Vec<(&str, Vec<String>)> = if self.provider_fallback_enabled {
            vec![
                (MEMCORE_LLM_FALLBACK_ORDER, self.llm_fallback_order.clone()),
                (
                    MEMCORE_SUMMARIZER_FALLBACK_ORDER,
                    self.summarizer_fallback_order.clone(),
                ),
                (
                    MEMCORE_EMBEDDING_FALLBACK_ORDER,
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
                (MEMCORE_LLM_PROVIDER, vec![llm.to_string()]),
                (MEMCORE_EMBEDDING_PROVIDER, vec![embedding.to_string()]),
            ]
        };

        let mut mock_roles = Vec::new();
        for (key, names) in &roles {
            let is_mock = |name: &String| name.trim().eq_ignore_ascii_case("mock");
            let has_mock = names.iter().any(is_mock);
            let has_real = names.iter().any(|name| !is_mock(name));
            if has_mock && has_real {
                return Err(MemcoreError::ValidationError(format!(
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
        Err(MemcoreError::ValidationError(format!(
            "refusing to start: {} {} to the mock provider (the default when unset); \
             {MOCK_PROVIDER_REFUSAL}. Configure real providers (MEMCORE_LLM_PROVIDER / \
             MEMCORE_EMBEDDING_PROVIDER, OPENAI_BASE_URL, OPENAI_API_KEY), or set \
             MEMCORE_ALLOW_MOCK=true for tests and local development only.",
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
    /// Output-token limit for fact extraction: `MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS` when set,
    /// otherwise the provider ceiling `MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS`.
    pub fn effective_extraction_max_output_tokens(&self) -> usize {
        self.extraction_max_output_tokens
            .unwrap_or(self.provider_max_output_tokens)
    }

    /// Output-token limit for fact classification: `MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS` when
    /// set, otherwise `DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS`, never above the provider ceiling.
    pub fn effective_classification_max_output_tokens(&self) -> usize {
        self.classification_max_output_tokens
            .unwrap_or(DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS)
            .min(self.provider_max_output_tokens)
    }

    pub fn from_env() -> MemcoreResult<Self> {
        let defaults = Self::default();

        let environment = Environment::from_str(&read_env_or(MEMCORE_ENV, "development"))?;
        let host = read_env_or(MEMCORE_HOST, &defaults.host);
        let port = parse_u16(MEMCORE_PORT, defaults.port)?;
        let storage_mode = StorageMode::from_str(&read_env_or(MEMCORE_STORAGE_MODE, "embedded"))?;
        // Qdrant is the default. The legacy `lancedb` cargo
        // feature: MEMCORE_VECTOR_BACKEND=lancedb still parses but is refused at
        // startup. It was never a safe default: memcore upserts one vector per fact and LanceDB
        // is append-only, so every fact creates a new dataset version. Measured
        // in the baseline at 2,200 facts — 4,402 versions and 364 MB of
        // manifests against 19 MB of vectors, search p50 1,245 ms against
        // Qdrant's 19.4 ms on identical data, and an ingest rate that fell to 4.6%
        // of its starting value within a single load.
        let vector_backend =
            VectorBackend::from_str(&read_env_or(MEMCORE_VECTOR_BACKEND, "qdrant"))?;
        let fact_backend = FactBackend::from_str(&read_env_or(MEMCORE_FACT_BACKEND, "sqlite"))?;
        let event_backend = match read_env_optional(MEMCORE_EVENT_BACKEND) {
            Some(value) => EventBackend::from_str(&value)?,
            None => EventBackend::default_for_fact_backend(&fact_backend),
        };
        let database_url = read_env_or(MEMCORE_DATABASE_URL, &defaults.database_url);
        let postgres_url = read_env_optional(MEMCORE_POSTGRES_URL);
        let database_migrations_enabled = parse_bool(
            MEMCORE_DATABASE_MIGRATIONS_ENABLED,
            defaults.database_migrations_enabled,
        )?;
        let database_migration_mode = DatabaseMigrationMode::from_str(&read_env_or(
            MEMCORE_DATABASE_MIGRATION_MODE,
            defaults.database_migration_mode.as_str(),
        ))?;
        let database_require_clean_migrations = parse_bool(
            MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS,
            defaults.database_require_clean_migrations,
        )?;
        let backup_enabled = parse_bool(MEMCORE_BACKUP_ENABLED, defaults.backup_enabled)?;
        let backup_dir = read_env_or(MEMCORE_BACKUP_DIR, &defaults.backup_dir);
        let backup_max_files = parse_usize(MEMCORE_BACKUP_MAX_FILES, defaults.backup_max_files)?;
        let restore_enabled = parse_bool(MEMCORE_RESTORE_ENABLED, defaults.restore_enabled)?;
        let security_headers_enabled = parse_bool(
            MEMCORE_SECURITY_HEADERS_ENABLED,
            defaults.security_headers_enabled,
        )?;
        let max_request_body_bytes = parse_usize(
            MEMCORE_MAX_REQUEST_BODY_BYTES,
            defaults.max_request_body_bytes,
        )?;
        let cors_enabled = parse_bool(MEMCORE_CORS_ENABLED, defaults.cors_enabled)?;
        let cors_allowed_origins =
            parse_string_list(MEMCORE_CORS_ALLOWED_ORIGINS, &defaults.cors_allowed_origins);
        let cors_allowed_methods =
            parse_string_list(MEMCORE_CORS_ALLOWED_METHODS, &defaults.cors_allowed_methods);
        let cors_allowed_headers =
            parse_string_list(MEMCORE_CORS_ALLOWED_HEADERS, &defaults.cors_allowed_headers);
        let cors_exposed_headers =
            parse_string_list(MEMCORE_CORS_EXPOSED_HEADERS, &defaults.cors_exposed_headers);
        let cors_allow_credentials = parse_bool(
            MEMCORE_CORS_ALLOW_CREDENTIALS,
            defaults.cors_allow_credentials,
        )?;
        let qdrant_url = read_env_or(MEMCORE_QDRANT_URL, &defaults.qdrant_url);
        let qdrant_collection = read_env_or(MEMCORE_QDRANT_COLLECTION, &defaults.qdrant_collection);
        let lancedb_path = read_env_or(MEMCORE_LANCEDB_PATH, &defaults.lancedb_path);
        let lancedb_table = read_env_or(MEMCORE_LANCEDB_TABLE, &defaults.lancedb_table);
        let llm_provider = LlmProviderKind::from_str(&read_env_or(MEMCORE_LLM_PROVIDER, "mock"))?;
        let llm_model = read_env_or(MEMCORE_LLM_MODEL, &defaults.llm_model);
        let embedding_provider =
            EmbeddingProviderKind::from_str(&read_env_or(MEMCORE_EMBEDDING_PROVIDER, "mock"))?;
        let embedding_model = read_env_or(MEMCORE_EMBEDDING_MODEL, &defaults.embedding_model);
        let enable_pii_redaction =
            parse_bool(MEMCORE_ENABLE_PII_REDACTION, defaults.enable_pii_redaction)?;
        let structured_extraction_enabled = parse_bool(
            MEMCORE_STRUCTURED_EXTRACTION_ENABLED,
            defaults.structured_extraction_enabled,
        )?;
        let episodes_enabled = parse_bool(MEMCORE_EPISODES_ENABLED, defaults.episodes_enabled)?;
        let experimental_candidates_enabled = parse_bool(
            MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED,
            defaults.experimental_candidates_enabled,
        )?;
        let query_analysis_enabled = parse_bool(
            MEMCORE_QUERY_ANALYSIS_ENABLED,
            defaults.query_analysis_enabled,
        )?;
        let temporal_ranking_enabled = parse_bool(
            MEMCORE_TEMPORAL_RANKING_ENABLED,
            defaults.temporal_ranking_enabled,
        )?;
        let temporal_boost = match env::var(MEMCORE_TEMPORAL_BOOST) {
            Ok(value) => value.trim().parse::<f64>().map_err(|_| {
                MemcoreError::ValidationError(format!(
                    "{MEMCORE_TEMPORAL_BOOST} must be a valid floating-point number"
                ))
            })?,
            Err(_) => defaults.temporal_boost,
        };
        let multi_query = match read_env_or(MEMCORE_MULTI_QUERY, "none")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => MultiQuerySetting::None,
            "deterministic" => MultiQuerySetting::Deterministic,
            "llm" => {
                return Err(MemcoreError::ValidationError(format!(
                    "{MEMCORE_MULTI_QUERY}=llm is not available in this build"
                )));
            }
            other => {
                return Err(MemcoreError::ValidationError(format!(
                    "{MEMCORE_MULTI_QUERY} must be none or deterministic, got {other}"
                )));
            }
        };
        let rerank_provider = match read_env_or(MEMCORE_RERANK_PROVIDER, "none")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => RerankProviderSetting::None,
            "local" => RerankProviderSetting::Local,
            "bedrock" => RerankProviderSetting::Bedrock,
            other => {
                return Err(MemcoreError::ValidationError(format!(
                    "{MEMCORE_RERANK_PROVIDER} must be none, local or bedrock, got {other}"
                )));
            }
        };
        let rerank_candidates = parse_usize(MEMCORE_RERANK_CANDIDATES, defaults.rerank_candidates)?;
        let rerank_timeout_ms = parse_u64(MEMCORE_RERANK_TIMEOUT_MS, defaults.rerank_timeout_ms)?;
        let rerank_local_model_path = read_env_optional(MEMCORE_RERANK_LOCAL_MODEL_PATH);
        let rerank_local_model_sha256 = read_env_optional(MEMCORE_RERANK_LOCAL_MODEL_SHA256);
        let rerank_local_tokenizer_path = read_env_optional(MEMCORE_RERANK_LOCAL_TOKENIZER_PATH);
        let rerank_local_tokenizer_sha256 =
            read_env_optional(MEMCORE_RERANK_LOCAL_TOKENIZER_SHA256);
        let rerank_local_threads =
            parse_usize(MEMCORE_RERANK_LOCAL_THREADS, defaults.rerank_local_threads)?;
        let rerank_local_batch_size = parse_usize(
            MEMCORE_RERANK_LOCAL_BATCH_SIZE,
            defaults.rerank_local_batch_size,
        )?;
        let rerank_bedrock_region = read_env_or(
            MEMCORE_RERANK_BEDROCK_REGION,
            &defaults.rerank_bedrock_region,
        )
        .trim()
        .to_string();
        let rerank_bedrock_model_arn = read_env_optional(MEMCORE_RERANK_BEDROCK_MODEL_ARN)
            .map(|arn| arn.trim().to_string())
            .unwrap_or_else(|| amazon_rerank_model_arn(&rerank_bedrock_region));
        let rerank_bedrock_profile = read_env_optional(MEMCORE_RERANK_BEDROCK_PROFILE);
        let query_decomposition = match read_env_or(MEMCORE_QUERY_DECOMPOSITION, "none")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => QueryDecompositionSetting::None,
            "noop" => QueryDecompositionSetting::Noop,
            "llm" => QueryDecompositionSetting::Llm,
            other => {
                return Err(MemcoreError::ValidationError(format!(
                    "{MEMCORE_QUERY_DECOMPOSITION} must be none, noop or llm, got {other}"
                )));
            }
        };
        let query_decomposition_timeout_ms = parse_u64(
            MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS,
            defaults.query_decomposition_timeout_ms,
        )?;
        let qdrant_episode_collection = read_env_or(
            MEMCORE_QDRANT_EPISODE_COLLECTION,
            &format!("{qdrant_collection}_episodes"),
        );
        let qdrant_ensure_indexes = parse_bool(
            MEMCORE_QDRANT_ENSURE_INDEXES,
            defaults.qdrant_ensure_indexes,
        )?;
        let qdrant_write_mode = match std::env::var(MEMCORE_QDRANT_WRITE_MODE) {
            Ok(value) if !value.trim().is_empty() => QdrantWriteModeSetting::from_str(&value)?,
            _ => defaults.qdrant_write_mode,
        };
        let qdrant_search_hnsw_ef = match read_env_optional(MEMCORE_QDRANT_SEARCH_HNSW_EF) {
            None => defaults.qdrant_search_hnsw_ef,
            Some(value) => match value.trim().parse::<u64>() {
                Ok(ef) if (1..=MAX_QDRANT_SEARCH_HNSW_EF).contains(&ef) => Some(ef),
                _ => {
                    return Err(MemcoreError::ValidationError(format!(
                        "Invalid MEMCORE_QDRANT_SEARCH_HNSW_EF value: {value} (expected an integer \
                         1..={MAX_QDRANT_SEARCH_HNSW_EF}, or unset for Qdrant's default)"
                    )));
                }
            },
        };
        let qdrant_exact_search_max_tenant_points =
            match read_env_optional(MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS) {
                None => defaults.qdrant_exact_search_max_tenant_points,
                Some(value) => match value.trim().parse::<u64>() {
                    Ok(n) if (1..=MAX_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS).contains(&n) => {
                        Some(n)
                    }
                    _ => {
                        return Err(MemcoreError::ValidationError(format!(
                            "Invalid MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS value: {value} \
                             (expected an integer 1..={MAX_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS}, \
                             or unset for the historical approximate search)"
                        )));
                    }
                },
            };
        let structured_extraction_policy = StructuredExtractionPolicy::from_str(&read_env_or(
            MEMCORE_STRUCTURED_EXTRACTION_POLICY,
            "retentive",
        ))?;
        let consolidation_mode =
            ConsolidationMode::from_str(&read_env_or(MEMCORE_CONSOLIDATION_MODE, "legacy"))?;
        let related_fact_matcher =
            RelatedFactMatcher::from_str(&read_env_or(MEMCORE_RELATED_FACT_MATCHER, "legacy"))?;
        let classification_prompt = ClassificationPromptSetting::from_str(&read_env_or(
            MEMCORE_CLASSIFICATION_PROMPT,
            "legacy",
        ))?;
        let importance_policy =
            ImportancePolicySetting::from_str(&read_env_or(MEMCORE_IMPORTANCE_POLICY, "legacy"))?;
        let temporal_rewrite =
            TemporalRewriteSetting::from_str(&read_env_or(MEMCORE_TEMPORAL_REWRITE, "legacy"))?;
        let context_default_packing =
            ContextPacking::from_str(&read_env_or(MEMCORE_CONTEXT_DEFAULT_PACKING, "prefix"))?;
        let consolidation_candidates = parse_usize(
            MEMCORE_CONSOLIDATION_CANDIDATES,
            defaults.consolidation_candidates,
        )?;
        if !(1..=50).contains(&consolidation_candidates) {
            return Err(MemcoreError::ValidationError(format!(
                "MEMCORE_CONSOLIDATION_CANDIDATES must be between 1 and 50, got {consolidation_candidates}"
            )));
        }
        let consolidation_similarity_floor = parse_f32(
            MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR,
            defaults.consolidation_similarity_floor,
        )?;
        if !(0.0..=1.0).contains(&consolidation_similarity_floor) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR must be between 0.0 and 1.0".to_string(),
            ));
        }
        let entities_enabled = parse_bool(MEMCORE_ENTITIES_ENABLED, defaults.entities_enabled)?;
        let entity_name_similarity_threshold = parse_f32(
            MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD,
            defaults.entity_name_similarity_threshold,
        )?;
        if !(0.0..=1.0).contains(&entity_name_similarity_threshold) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD must be between 0.0 and 1.0".to_string(),
            ));
        }
        let entity_llm_confirmation_enabled = parse_bool(
            MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED,
            defaults.entity_llm_confirmation_enabled,
        )?;
        let search_strategy =
            SearchStrategy::from_str(&read_env_or(MEMCORE_SEARCH_STRATEGY, "hybrid"))?;
        let rrf_k = parse_f32(MEMCORE_RRF_K, defaults.rrf_k)?;
        let search_max_limit = parse_usize(MEMCORE_SEARCH_MAX_LIMIT, defaults.search_max_limit)?;
        let export_max_records =
            parse_usize(MEMCORE_EXPORT_MAX_RECORDS, defaults.export_max_records)?;
        let query_embedding_cache_capacity = parse_usize(
            MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY,
            defaults.query_embedding_cache_capacity,
        )?;
        let classification_skip_when_no_related = parse_bool(
            MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED,
            defaults.classification_skip_when_no_related,
        )?;
        let ingest_journal_enabled = parse_bool(
            MEMCORE_INGEST_JOURNAL_ENABLED,
            defaults.ingest_journal_enabled,
        )?;
        let ingest_recovery_stale_secs = parse_u64(
            MEMCORE_INGEST_RECOVERY_STALE_SECS,
            defaults.ingest_recovery_stale_secs,
        )?;
        let ingest_recovery_interval_secs = parse_u64(
            MEMCORE_INGEST_RECOVERY_INTERVAL_SECS,
            defaults.ingest_recovery_interval_secs,
        )?;
        let ingest_inflight_wait_ms = parse_u64(
            MEMCORE_INGEST_INFLIGHT_WAIT_MS,
            defaults.ingest_inflight_wait_ms,
        )?;
        let ingest_receipt_ttl_secs = parse_u64(
            MEMCORE_INGEST_RECEIPT_TTL_SECS,
            defaults.ingest_receipt_ttl_secs,
        )?;
        let local_embedding_model_dir = read_env_optional(MEMCORE_LOCAL_EMBEDDING_MODEL_DIR);
        let local_embedding_queue_capacity = parse_usize(
            MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY,
            defaults.local_embedding_queue_capacity,
        )?;
        let rrf_weight_semantic =
            parse_f32(MEMCORE_RRF_WEIGHT_SEMANTIC, defaults.rrf_weight_semantic)?;
        let rrf_weight_keyword =
            parse_f32(MEMCORE_RRF_WEIGHT_KEYWORD, defaults.rrf_weight_keyword)?;
        let extraction_window_turns = parse_usize(
            MEMCORE_EXTRACTION_WINDOW_TURNS,
            defaults.extraction_window_turns,
        )?;
        let extraction_window_overlap = parse_usize(
            MEMCORE_EXTRACTION_WINDOW_OVERLAP,
            defaults.extraction_window_overlap,
        )?;
        let extraction_concurrency = parse_usize(
            MEMCORE_EXTRACTION_CONCURRENCY,
            defaults.extraction_concurrency,
        )?;
        let min_importance = parse_f32(MEMCORE_MIN_IMPORTANCE, defaults.min_importance)?;
        let auth_enabled = parse_bool(MEMCORE_AUTH_ENABLED, defaults.auth_enabled)?;
        let auth_mode = AuthMode::from_str(&read_env_or(MEMCORE_AUTH_MODE, "dev"))?;
        let dev_api_key = read_env_or(MEMCORE_DEV_API_KEY, &defaults.dev_api_key);
        let api_key_pepper = read_env_optional(MEMCORE_API_KEY_PEPPER);
        let openai_api_key = read_env_optional(OPENAI_API_KEY);
        let openai_base_url = read_env_or(OPENAI_BASE_URL, &defaults.openai_base_url);
        let rate_limit_enabled =
            parse_bool(MEMCORE_RATE_LIMIT_ENABLED, defaults.rate_limit_enabled)?;
        let rate_limit_requests_per_minute = parse_u32(
            MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE,
            defaults.rate_limit_requests_per_minute,
        )?;
        let admission = parse_admission(&defaults.admission)?;
        let providers = parse_provider_roles(&defaults.providers)?;
        let log_format = LogFormat::from_str(&read_env_or(MEMCORE_LOG_FORMAT, "json"))?;
        let log_level = LogLevel::from_str(&read_env_or(MEMCORE_LOG_LEVEL, "info"))?;
        let request_id_header = read_env_or(MEMCORE_REQUEST_ID_HEADER, &defaults.request_id_header);
        let metrics_enabled = parse_bool(MEMCORE_METRICS_ENABLED, defaults.metrics_enabled)?;
        let metrics_path = read_env_or(MEMCORE_METRICS_PATH, &defaults.metrics_path);
        let metrics_require_auth =
            parse_bool(MEMCORE_METRICS_REQUIRE_AUTH, defaults.metrics_require_auth)?;
        let metrics_include_process = parse_bool(
            MEMCORE_METRICS_INCLUDE_PROCESS,
            defaults.metrics_include_process,
        )?;
        let retention_enabled = parse_bool(MEMCORE_RETENTION_ENABLED, defaults.retention_enabled)?;
        let fact_retention_days =
            parse_u32(MEMCORE_FACT_RETENTION_DAYS, defaults.fact_retention_days)?;
        let event_retention_days =
            parse_u32(MEMCORE_EVENT_RETENTION_DAYS, defaults.event_retention_days)?;
        // The legacy flag is still parsed (a malformed value fails); when both are set they must
        // agree (no silent precedence between contradictory settings).
        let legacy_cache_enabled = read_env_optional(MEMCORE_CONTEXT_CACHE_ENABLED)
            .map(|_| {
                parse_bool(
                    MEMCORE_CONTEXT_CACHE_ENABLED,
                    defaults.context_cache_enabled,
                )
            })
            .transpose()?;
        let context_cache_backend = match read_env_optional(MEMCORE_CONTEXT_CACHE_BACKEND) {
            Some(value) => {
                let backend = ContextCacheBackend::from_str(&value)?;
                if let Some(legacy) = legacy_cache_enabled
                    && legacy != (backend != ContextCacheBackend::Disabled)
                {
                    return Err(MemcoreError::ValidationError(format!(
                        "{MEMCORE_CONTEXT_CACHE_ENABLED} contradicts {MEMCORE_CONTEXT_CACHE_BACKEND}; \
                         remove {MEMCORE_CONTEXT_CACHE_ENABLED} or make them agree \
                         (the safe setting is {MEMCORE_CONTEXT_CACHE_BACKEND}=disabled)"
                    )));
                }
                backend
            }
            None if legacy_cache_enabled == Some(true) => ContextCacheBackend::Memory,
            None => ContextCacheBackend::Disabled,
        };
        // An unset MEMCORE_ENV defaults to development, which is not evidence of a local
        // single-process deployment: the memory cache needs it stated explicitly.
        if context_cache_backend == ContextCacheBackend::Memory
            && read_env_optional(MEMCORE_ENV).is_none()
        {
            return Err(MemcoreError::ValidationError(format!(
                "context cache refused: {MEMCORE_CONTEXT_CACHE_BACKEND}=memory needs {MEMCORE_ENV} \
                 set explicitly to development (local single-process development only); \
                 otherwise set {MEMCORE_CONTEXT_CACHE_BACKEND}=disabled"
            )));
        }
        let context_cache_enabled = context_cache_backend != ContextCacheBackend::Disabled;
        let context_cache_local_single_process = parse_bool(
            MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS,
            defaults.context_cache_local_single_process,
        )?;
        let context_cache_ttl_seconds = parse_u64(
            MEMCORE_CONTEXT_CACHE_TTL_SECONDS,
            defaults.context_cache_ttl_seconds,
        )?;
        let context_cache_max_entries = parse_usize(
            MEMCORE_CONTEXT_CACHE_MAX_ENTRIES,
            defaults.context_cache_max_entries,
        )?;
        let redis_url = read_env_optional(MEMCORE_REDIS_URL);
        let context_cache_key_prefix = read_env_or(
            MEMCORE_CONTEXT_CACHE_KEY_PREFIX,
            &defaults.context_cache_key_prefix,
        );
        let context_cache_stampede_protection_enabled = parse_bool(
            MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED,
            defaults.context_cache_stampede_protection_enabled,
        )?;
        let context_cache_lock_timeout_seconds = parse_u64(
            MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS,
            defaults.context_cache_lock_timeout_seconds,
        )?;
        let context_cache_stale_while_revalidate_enabled = parse_bool(
            MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED,
            defaults.context_cache_stale_while_revalidate_enabled,
        )?;
        let context_cache_stale_ttl_seconds = parse_u64(
            MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS,
            defaults.context_cache_stale_ttl_seconds,
        )?;
        let context_cache_metrics_enabled = parse_bool(
            MEMCORE_CONTEXT_CACHE_METRICS_ENABLED,
            defaults.context_cache_metrics_enabled,
        )?;
        let provider_timeout_seconds = parse_u64(
            MEMCORE_PROVIDER_TIMEOUT_SECONDS,
            defaults.provider_timeout_seconds,
        )?;
        let provider_max_retries =
            parse_usize(MEMCORE_PROVIDER_MAX_RETRIES, defaults.provider_max_retries)?;
        let provider_initial_backoff_ms = parse_u64(
            MEMCORE_PROVIDER_INITIAL_BACKOFF_MS,
            defaults.provider_initial_backoff_ms,
        )?;
        let provider_max_backoff_ms = parse_u64(
            MEMCORE_PROVIDER_MAX_BACKOFF_MS,
            defaults.provider_max_backoff_ms,
        )?;
        let provider_backoff_multiplier = parse_f32(
            MEMCORE_PROVIDER_BACKOFF_MULTIPLIER,
            defaults.provider_backoff_multiplier,
        )?;
        let provider_retry_jitter_enabled = parse_bool(
            MEMCORE_PROVIDER_RETRY_JITTER_ENABLED,
            defaults.provider_retry_jitter_enabled,
        )?;
        let provider_fallback_enabled = parse_bool(
            MEMCORE_PROVIDER_FALLBACK_ENABLED,
            defaults.provider_fallback_enabled,
        )?;
        let llm_fallback_order = parse_provider_fallback_order(
            MEMCORE_LLM_FALLBACK_ORDER,
            &defaults.llm_fallback_order,
        )?;
        let embedding_fallback_order = parse_provider_fallback_order(
            MEMCORE_EMBEDDING_FALLBACK_ORDER,
            &defaults.embedding_fallback_order,
        )?;
        let summarizer_fallback_order = parse_provider_fallback_order(
            MEMCORE_SUMMARIZER_FALLBACK_ORDER,
            &defaults.summarizer_fallback_order,
        )?;
        let provider_circuit_breaker_enabled = parse_bool(
            MEMCORE_PROVIDER_CIRCUIT_BREAKER_ENABLED,
            defaults.provider_circuit_breaker_enabled,
        )?;
        let provider_circuit_breaker_failure_threshold = parse_usize(
            MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD,
            defaults.provider_circuit_breaker_failure_threshold,
        )?;
        let provider_circuit_breaker_reset_timeout_seconds = parse_u64(
            MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS,
            defaults.provider_circuit_breaker_reset_timeout_seconds,
        )?;
        let provider_circuit_breaker_half_open_max_calls = parse_usize(
            MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS,
            defaults.provider_circuit_breaker_half_open_max_calls,
        )?;
        let provider_usage_metrics_enabled = parse_bool(
            MEMCORE_PROVIDER_USAGE_METRICS_ENABLED,
            defaults.provider_usage_metrics_enabled,
        )?;
        let provider_cost_tracking_enabled = parse_bool(
            MEMCORE_PROVIDER_COST_TRACKING_ENABLED,
            defaults.provider_cost_tracking_enabled,
        )?;
        let provider_usage_persistence_enabled = parse_bool(
            MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED,
            defaults.provider_usage_persistence_enabled,
        )?;
        let provider_usage_retention_days = parse_u32(
            MEMCORE_PROVIDER_USAGE_RETENTION_DAYS,
            defaults.provider_usage_retention_days,
        )?;
        let quotas_enabled = parse_bool(MEMCORE_QUOTAS_ENABLED, defaults.quotas_enabled)?;
        let max_users_per_org = parse_u64(MEMCORE_MAX_USERS_PER_ORG, defaults.max_users_per_org)?;
        let max_memories_per_user = parse_u64(
            MEMCORE_MAX_MEMORIES_PER_USER,
            defaults.max_memories_per_user,
        )?;
        let max_memories_per_org =
            parse_u64(MEMCORE_MAX_MEMORIES_PER_ORG, defaults.max_memories_per_org)?;
        let daily_provider_request_limit = parse_u64(
            MEMCORE_DAILY_PROVIDER_REQUEST_LIMIT,
            defaults.daily_provider_request_limit,
        )?;
        let daily_provider_token_limit = parse_u64(
            MEMCORE_DAILY_PROVIDER_TOKEN_LIMIT,
            defaults.daily_provider_token_limit,
        )?;
        let provider_guardrails_enabled = parse_bool(
            MEMCORE_PROVIDER_GUARDRAILS_ENABLED,
            defaults.provider_guardrails_enabled,
        )?;
        let real_provider_calls_enabled = parse_bool(
            MEMCORE_REAL_PROVIDER_CALLS_ENABLED,
            defaults.real_provider_calls_enabled,
        )?;
        let provider_test_mode = ProviderTestMode::from_str(&read_env_or(
            MEMCORE_PROVIDER_TEST_MODE,
            defaults.provider_test_mode.as_str(),
        ))?;
        let provider_max_calls_per_run = parse_usize(
            MEMCORE_PROVIDER_MAX_CALLS_PER_RUN,
            defaults.provider_max_calls_per_run,
        )?;
        let provider_max_input_chars = parse_usize(
            MEMCORE_PROVIDER_MAX_INPUT_CHARS,
            defaults.provider_max_input_chars,
        )?;
        let provider_max_output_tokens = parse_usize(
            MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS,
            defaults.provider_max_output_tokens,
        )?;
        let extraction_max_output_tokens =
            parse_optional_usize(MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS)?;
        let classification_max_output_tokens =
            parse_optional_usize(MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS)?;
        let provider_max_retries_per_call = parse_usize(
            MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL,
            defaults.provider_max_retries_per_call,
        )?;
        let allow_real_providers_during_load_tests = parse_bool(
            MEMCORE_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS,
            defaults.allow_real_providers_during_load_tests,
        )?;
        let background_jobs_allow_real_providers = parse_bool(
            MEMCORE_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS,
            defaults.background_jobs_allow_real_providers,
        )?;
        let multi_provider_validation_enabled = parse_bool(
            MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED,
            defaults.multi_provider_validation_enabled,
        )?;
        let multi_provider_validation_confirmation =
            read_env_optional(MEMCORE_MULTI_PROVIDER_VALIDATION_CONFIRMATION);
        let background_jobs_enabled = parse_bool(
            MEMCORE_BACKGROUND_JOBS_ENABLED,
            defaults.background_jobs_enabled,
        )?;
        let background_job_runner_interval_seconds = parse_u64(
            MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS,
            defaults.background_job_runner_interval_seconds,
        )?;
        let background_job_org_ids = parse_string_list(
            MEMCORE_BACKGROUND_JOB_ORG_IDS,
            &defaults.background_job_org_ids,
        );
        let memory_usage_snapshot_job_enabled = parse_bool(
            MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED,
            defaults.memory_usage_snapshot_job_enabled,
        )?;
        let memory_usage_snapshot_job_interval_seconds = parse_u64(
            MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS,
            defaults.memory_usage_snapshot_job_interval_seconds,
        )?;
        let provider_usage_retention_job_enabled = parse_bool(
            MEMCORE_PROVIDER_USAGE_RETENTION_JOB_ENABLED,
            defaults.provider_usage_retention_job_enabled,
        )?;
        let provider_usage_retention_job_interval_seconds = parse_u64(
            MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS,
            defaults.provider_usage_retention_job_interval_seconds,
        )?;
        let memory_retention_job_enabled = parse_bool(
            MEMCORE_MEMORY_RETENTION_JOB_ENABLED,
            defaults.memory_retention_job_enabled,
        )?;
        let memory_retention_job_interval_seconds = parse_u64(
            MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS,
            defaults.memory_retention_job_interval_seconds,
        )?;
        let background_job_history_enabled = parse_bool(
            MEMCORE_BACKGROUND_JOB_HISTORY_ENABLED,
            defaults.background_job_history_enabled,
        )?;
        let background_job_history_retention_days = parse_u32(
            MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS,
            defaults.background_job_history_retention_days,
        )?;
        let background_job_lock_enabled = parse_bool(
            MEMCORE_BACKGROUND_JOB_LOCK_ENABLED,
            defaults.background_job_lock_enabled,
        )?;
        let background_job_lock_backend = BackgroundJobLockBackend::from_str(&read_env_or(
            MEMCORE_BACKGROUND_JOB_LOCK_BACKEND,
            "database",
        ))?;
        let background_job_lock_ttl_seconds = parse_u64(
            MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS,
            defaults.background_job_lock_ttl_seconds,
        )?;
        let background_job_lock_owner_id = read_env_optional(MEMCORE_BACKGROUND_JOB_LOCK_OWNER_ID)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let background_job_retries_enabled = parse_bool(
            MEMCORE_BACKGROUND_JOB_RETRIES_ENABLED,
            defaults.background_job_retries_enabled,
        )?;
        let background_job_max_retries = parse_usize(
            MEMCORE_BACKGROUND_JOB_MAX_RETRIES,
            defaults.background_job_max_retries,
        )?;
        let background_job_initial_backoff_ms = parse_u64(
            MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS,
            defaults.background_job_initial_backoff_ms,
        )?;
        let background_job_max_backoff_ms = parse_u64(
            MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS,
            defaults.background_job_max_backoff_ms,
        )?;
        let background_job_backoff_multiplier = parse_f32(
            MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER,
            defaults.background_job_backoff_multiplier,
        )?;
        let background_job_retry_jitter_enabled = parse_bool(
            MEMCORE_BACKGROUND_JOB_RETRY_JITTER_ENABLED,
            defaults.background_job_retry_jitter_enabled,
        )?;
        let graceful_shutdown_timeout_seconds = parse_u64(
            MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS,
            defaults.graceful_shutdown_timeout_seconds,
        )?;
        let allow_mock = parse_bool(MEMCORE_ALLOW_MOCK, defaults.allow_mock)?;
        let startup_dependency_timeout_seconds = parse_u64(
            MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS,
            defaults.startup_dependency_timeout_seconds,
        )?;
        let background_job_shutdown_timeout_seconds = parse_u64(
            MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS,
            defaults.background_job_shutdown_timeout_seconds,
        )?;

        if !(0.0..=1.0).contains(&min_importance) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_MIN_IMPORTANCE must be between 0.0 and 1.0".to_string(),
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
        if read_env_optional(MEMCORE_INGEST_RECOVERY_STALE_SECS).is_none() {
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

    /// `MEMCORE_REQUIRE_QUALITY_TIER`: refuses a configuration whose tier differs (fail closed).
    pub fn check_required_quality_tier(&self) -> MemcoreResult<QualityTier> {
        let tier = self.quality_tier();
        match self.providers.require_quality_tier {
            Some(required) if required != tier => Err(MemcoreError::ValidationError(format!(
                "MEMCORE_REQUIRE_QUALITY_TIER={} but this configuration is {}{}",
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

    /// The extraction / chat endpoint: `MEMCORE_LLM_*`, else `OPENAI_*`.
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

    /// The embedding endpoint: `MEMCORE_EMBEDDING_*`, else `OPENAI_*`.
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
    fn validate_provider_roles(&self) -> MemcoreResult<()> {
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
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EMBEDDING_DIMENSIONS must be 1..=65536".to_string(),
            ));
        }
        if let Some(t) = e.dedup_threshold
            && !(t > 0.0 && t <= 1.0)
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EMBEDDING_DEDUP_THRESHOLD must be in (0, 1]".to_string(),
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
    /// - `memory` is allowed only for local single-process development: `MEMCORE_ENV` is
    ///   development (it must be set explicitly, checked when reading the environment),
    ///   `MEMCORE_STORAGE_MODE` is not production, neither the fact nor the event backend is
    ///   PostgreSQL (a shared database invites several API processes), and the operator opted in
    ///   with `MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true`. The opt-in states, it does not
    ///   enforce, a single process; it never overrides a production marker.
    ///
    /// Called by configuration validation (every `Settings::from_env` / `load_settings`, before
    /// logging, dependencies, stores or the listener) and again by `AppState` before a cache
    /// backend is built, for settings constructed in code. Errors name variables only.
    pub fn check_context_cache_policy(&self) -> MemcoreResult<()> {
        let refuse = |reason: String| {
            Err(MemcoreError::ValidationError(format!(
                "context cache refused: {reason}; set {MEMCORE_CONTEXT_CACHE_BACKEND}=disabled"
            )))
        };
        match self.context_cache_backend {
            ContextCacheBackend::Disabled => Ok(()),
            ContextCacheBackend::Redis => refuse(format!(
                "{MEMCORE_CONTEXT_CACHE_BACKEND}=redis is not supported: shared-cache invalidation \
                 across API processes is not verified as safe (AR-023)"
            )),
            ContextCacheBackend::Memory => {
                let production_markers = [
                    (
                        self.environment == Environment::Production,
                        "MEMCORE_ENV=production",
                    ),
                    (
                        self.storage_mode == StorageMode::Production,
                        "MEMCORE_STORAGE_MODE=production",
                    ),
                    (
                        self.fact_backend == FactBackend::Postgres,
                        "MEMCORE_FACT_BACKEND=postgres",
                    ),
                    (
                        self.event_backend == EventBackend::Postgres,
                        "MEMCORE_EVENT_BACKEND=postgres",
                    ),
                ];
                let markers: Vec<&str> = production_markers
                    .iter()
                    .filter(|(present, _)| *present)
                    .map(|(_, name)| *name)
                    .collect();
                if !markers.is_empty() {
                    return refuse(format!(
                        "{MEMCORE_CONTEXT_CACHE_BACKEND}=memory is a process-local cache, supported \
                         only for local single-process development, not with {}",
                        markers.join(", ")
                    ));
                }
                if !self.context_cache_local_single_process {
                    return refuse(format!(
                        "{MEMCORE_CONTEXT_CACHE_BACKEND}=memory needs \
                         {MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS}=true (local single-process \
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
    fn validate_ingest_journal(&self) -> MemcoreResult<()> {
        if !self.ingest_journal_enabled {
            return Ok(());
        }
        if self.ingest_recovery_interval_secs == 0 || self.ingest_recovery_interval_secs > 86_400 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_INGEST_RECOVERY_INTERVAL_SECS must be 1..=86400".to_string(),
            ));
        }
        if self.ingest_recovery_stale_secs == 0 || self.ingest_recovery_stale_secs > 604_800 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_INGEST_RECOVERY_STALE_SECS must be 1..=604800".to_string(),
            ));
        }
        if !(60..=2_592_000).contains(&self.ingest_receipt_ttl_secs) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_INGEST_RECEIPT_TTL_SECS must be 60..=2592000".to_string(),
            ));
        }
        if self.ingest_inflight_wait_ms > 600_000 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_INGEST_INFLIGHT_WAIT_MS must be 0..=600000".to_string(),
            ));
        }
        if self.admission.enabled
            && self.ingest_recovery_stale_secs.saturating_mul(1_000)
                <= self.admission.write.deadline_ms
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_INGEST_RECOVERY_STALE_SECS must exceed the write deadline (MEMCORE_ADMISSION_WRITE_DEADLINE_MS)"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> MemcoreResult<()> {
        // First, so an unsafe cache configuration is reported as such .
        self.check_context_cache_policy()?;
        if self.host.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_HOST cannot be empty".to_string(),
            ));
        }

        if self.database_url.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_DATABASE_URL cannot be empty".to_string(),
            ));
        }

        if self.backup_dir.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKUP_DIR cannot be empty".to_string(),
            ));
        }

        if self.backup_max_files == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKUP_MAX_FILES must be greater than 0".to_string(),
            ));
        }

        if self.max_request_body_bytes == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_MAX_REQUEST_BODY_BYTES must be greater than 0".to_string(),
            ));
        }

        self.validate_ingest_journal()?;

        validate_cors_settings(self)?;

        if self.lancedb_path.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_LANCEDB_PATH cannot be empty".to_string(),
            ));
        }

        if self.lancedb_table.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_LANCEDB_TABLE cannot be empty".to_string(),
            ));
        }

        if self.auth_enabled
            && self.auth_mode == AuthMode::Dev
            && self.dev_api_key.trim().is_empty()
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_DEV_API_KEY cannot be empty when MEMCORE_AUTH_ENABLED=true and MEMCORE_AUTH_MODE=dev"
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
            return Err(MemcoreError::ValidationError(
                "MEMCORE_API_KEY_PEPPER is required when MEMCORE_AUTH_MODE=database".to_string(),
            ));
        }

        if self.openai_base_url.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "OPENAI_BASE_URL cannot be empty".to_string(),
            ));
        }

        validate_admission(&self.admission)?;
        self.validate_provider_roles()?;

        if self.rate_limit_requests_per_minute == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE must be greater than 0".to_string(),
            ));
        }

        if self.request_id_header.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_REQUEST_ID_HEADER cannot be empty".to_string(),
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
            return Err(MemcoreError::ValidationError(
                "MEMCORE_POSTGRES_URL is required when MEMCORE_FACT_BACKEND=postgres".to_string(),
            ));
        }

        if self.event_backend == EventBackend::Postgres
            && self
                .postgres_url
                .as_ref()
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_POSTGRES_URL is required when MEMCORE_EVENT_BACKEND=postgres".to_string(),
            ));
        }

        if self.fact_retention_days > 365_000 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_FACT_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.event_retention_days > 365_000 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EVENT_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.provider_usage_retention_days > 365_000 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_USAGE_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.background_job_history_retention_days > 365_000 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS is unreasonably large".to_string(),
            ));
        }

        if self.background_job_runner_interval_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS must be greater than 0".to_string(),
            ));
        }

        if self.memory_usage_snapshot_job_interval_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.provider_usage_retention_job_interval_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.memory_retention_job_interval_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS must be greater than 0".to_string(),
            ));
        }

        if self.background_job_lock_ttl_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS must be greater than 0".to_string(),
            ));
        }

        if self.background_job_initial_backoff_ms == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS must be greater than 0".to_string(),
            ));
        }
        if self.background_job_max_backoff_ms < self.background_job_initial_backoff_ms {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS must be >= MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS"
                    .to_string(),
            ));
        }
        if self.background_job_backoff_multiplier < 1.0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER must be >= 1.0".to_string(),
            ));
        }
        if self.startup_dependency_timeout_seconds > 3_600 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS must be at most 3600".to_string(),
            ));
        }
        if self.graceful_shutdown_timeout_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS must be greater than 0".to_string(),
            ));
        }
        if self.background_job_shutdown_timeout_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }

        match self.context_cache_backend {
            ContextCacheBackend::Disabled => {}
            ContextCacheBackend::Memory => {
                if self.context_cache_ttl_seconds == 0 {
                    return Err(MemcoreError::ValidationError(
                        "MEMCORE_CONTEXT_CACHE_TTL_SECONDS must be greater than 0 when context cache backend is memory"
                            .to_string(),
                    ));
                }
                if self.context_cache_max_entries == 0 {
                    return Err(MemcoreError::ValidationError(
                        "MEMCORE_CONTEXT_CACHE_MAX_ENTRIES must be greater than 0 when context cache backend is memory"
                            .to_string(),
                    ));
                }
            }
            ContextCacheBackend::Redis => {
                if self.context_cache_ttl_seconds == 0 {
                    return Err(MemcoreError::ValidationError(
                        "MEMCORE_CONTEXT_CACHE_TTL_SECONDS must be greater than 0 when context cache backend is redis"
                            .to_string(),
                    ));
                }
                if self
                    .redis_url
                    .as_ref()
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
                {
                    return Err(MemcoreError::ValidationError(
                        "MEMCORE_REDIS_URL is required when MEMCORE_CONTEXT_CACHE_BACKEND=redis"
                            .to_string(),
                    ));
                }
            }
        }

        if self.context_cache_key_prefix.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_CONTEXT_CACHE_KEY_PREFIX cannot be empty".to_string(),
            ));
        }

        if self.context_cache_enabled
            && self.context_cache_stampede_protection_enabled
            && self.context_cache_lock_timeout_seconds == 0
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS must be greater than 0 when stampede protection is enabled"
                    .to_string(),
            ));
        }

        if self.context_cache_enabled
            && self.context_cache_stale_while_revalidate_enabled
            && self.context_cache_stale_ttl_seconds == 0
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS must be greater than 0 when stale-while-revalidate is enabled"
                    .to_string(),
            ));
        }

        if self.vector_backend == VectorBackend::Qdrant {
            if self.qdrant_url.trim().is_empty() {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_QDRANT_URL is required when MEMCORE_VECTOR_BACKEND=qdrant".to_string(),
                ));
            }
            if self.qdrant_collection.trim().is_empty() {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_QDRANT_COLLECTION cannot be empty when MEMCORE_VECTOR_BACKEND=qdrant"
                        .to_string(),
                ));
            }
        }

        if self.provider_timeout_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_TIMEOUT_SECONDS must be greater than 0".to_string(),
            ));
        }
        if self.provider_initial_backoff_ms == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_INITIAL_BACKOFF_MS must be greater than 0".to_string(),
            ));
        }
        if self.provider_max_backoff_ms < self.provider_initial_backoff_ms {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_BACKOFF_MS must be >= MEMCORE_PROVIDER_INITIAL_BACKOFF_MS"
                    .to_string(),
            ));
        }
        if self.provider_backoff_multiplier < 1.0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_BACKOFF_MULTIPLIER must be >= 1.0".to_string(),
            ));
        }

        if self.provider_circuit_breaker_failure_threshold == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD must be greater than 0"
                    .to_string(),
            ));
        }
        if self.provider_circuit_breaker_reset_timeout_seconds == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS must be greater than 0"
                    .to_string(),
            ));
        }
        if self.provider_circuit_breaker_half_open_max_calls == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS must be greater than 0"
                    .to_string(),
            ));
        }

        if self.provider_max_calls_per_run == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_CALLS_PER_RUN must be greater than 0".to_string(),
            ));
        }
        if self.provider_max_input_chars == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_INPUT_CHARS must be greater than 0".to_string(),
            ));
        }
        if self.provider_max_output_tokens == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS must be greater than 0".to_string(),
            ));
        }
        for (key, weight) in [
            ("MEMCORE_RRF_WEIGHT_SEMANTIC", self.rrf_weight_semantic),
            ("MEMCORE_RRF_WEIGHT_KEYWORD", self.rrf_weight_keyword),
        ] {
            if !(weight.is_finite() && weight >= 0.0) {
                return Err(MemcoreError::ValidationError(format!(
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
            return Err(MemcoreError::ValidationError(
                "the Phase 4 candidate flags need MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED"
                    .to_string(),
            ));
        }
        if (self.temporal_ranking_enabled || self.multi_query != MultiQuerySetting::None)
            && !self.query_analysis_enabled
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_TEMPORAL_RANKING_ENABLED and MEMCORE_MULTI_QUERY need MEMCORE_QUERY_ANALYSIS_ENABLED"
                    .to_string(),
            ));
        }
        if !(self.temporal_boost > 0.0 && self.temporal_boost <= 1.0) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_TEMPORAL_BOOST must be greater than 0 and at most 1".to_string(),
            ));
        }
        if !(1..=100).contains(&self.rerank_candidates) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_RERANK_CANDIDATES must be between 1 and 100".to_string(),
            ));
        }
        if !(100..=600_000).contains(&self.rerank_timeout_ms)
            || !(100..=600_000).contains(&self.query_decomposition_timeout_ms)
        {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_RERANK_TIMEOUT_MS and MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS must be between 100 and 600000"
                    .to_string(),
            ));
        }
        if self.rerank_provider == RerankProviderSetting::Local {
            if self.rerank_local_model_path.is_none()
                || self.rerank_local_model_sha256.is_none()
                || self.rerank_local_tokenizer_path.is_none()
                || self.rerank_local_tokenizer_sha256.is_none()
            {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_RERANK_PROVIDER=local needs MEMCORE_RERANK_LOCAL_MODEL_PATH, _MODEL_SHA256, _TOKENIZER_PATH and _TOKENIZER_SHA256"
                        .to_string(),
                ));
            }
            if !(1..=64).contains(&self.rerank_local_threads)
                || !(1..=64).contains(&self.rerank_local_batch_size)
            {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_RERANK_LOCAL_THREADS and MEMCORE_RERANK_LOCAL_BATCH_SIZE must be between 1 and 64"
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
            return Err(MemcoreError::ValidationError(
                "MEMCORE_RERANK_PROVIDER=bedrock needs MEMCORE_RERANK_BEDROCK_REGION, and                  MEMCORE_RERANK_BEDROCK_MODEL_ARN (if set) must be a Bedrock ARN in that region"
                    .to_string(),
            ));
        }
        if self.episodes_enabled && self.qdrant_episode_collection == self.qdrant_collection {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_QDRANT_EPISODE_COLLECTION must differ from MEMCORE_QDRANT_COLLECTION"
                    .to_string(),
            ));
        }
        if !(50..=200).contains(&self.search_max_limit) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_SEARCH_MAX_LIMIT must be between 50 and 200".to_string(),
            ));
        }
        if self.embedding_provider.is_local() {
            if self
                .local_embedding_model_dir
                .as_deref()
                .is_none_or(|dir| dir.trim().is_empty())
            {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_LOCAL_EMBEDDING_MODEL_DIR is required for a local embedding provider"
                        .to_string(),
                ));
            }
            if self.vector_backend != VectorBackend::Qdrant {
                return Err(MemcoreError::ValidationError(
                    "a local embedding provider requires MEMCORE_VECTOR_BACKEND=qdrant (local \
                     stores record and check their embedding identity)"
                        .to_string(),
                ));
            }
        }
        if !(1..=1024).contains(&self.local_embedding_queue_capacity) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY must be between 1 and 1024".to_string(),
            ));
        }
        if self.export_max_records == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EXPORT_MAX_RECORDS must be greater than 0".to_string(),
            ));
        }
        if self.query_embedding_cache_capacity > 1_000_000 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY must be at most 1000000".to_string(),
            ));
        }
        if !(self.rrf_k.is_finite() && self.rrf_k > 0.0) {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_RRF_K must be a positive number".to_string(),
            ));
        }
        if self.extraction_window_turns == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EXTRACTION_WINDOW_TURNS must be greater than 0".to_string(),
            ));
        }
        if self.extraction_window_overlap >= self.extraction_window_turns {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EXTRACTION_WINDOW_OVERLAP must be smaller than MEMCORE_EXTRACTION_WINDOW_TURNS"
                    .to_string(),
            ));
        }
        if self.extraction_concurrency == 0 {
            return Err(MemcoreError::ValidationError(
                "MEMCORE_EXTRACTION_CONCURRENCY must be greater than 0".to_string(),
            ));
        }
        for (key, value) in [
            (
                MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS,
                self.extraction_max_output_tokens,
            ),
            (
                MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS,
                self.classification_max_output_tokens,
            ),
        ] {
            // An explicit per-operation limit above the ceiling is rejected at startup rather than
            // silently clamped: a clamped extraction limit is exactly how responses got truncated.
            match value {
                Some(0) => {
                    return Err(MemcoreError::ValidationError(format!(
                        "{key} must be greater than 0"
                    )));
                }
                Some(limit) if limit > self.provider_max_output_tokens => {
                    return Err(MemcoreError::ValidationError(format!(
                        "{key} ({limit}) must not exceed MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS ({})",
                        self.provider_max_output_tokens
                    )));
                }
                _ => {}
            }
        }
        if self.provider_max_retries_per_call > MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES {
            return Err(MemcoreError::ValidationError(format!(
                "MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL cannot exceed {MAX_SAFE_PROVIDER_GUARDRAIL_RETRIES}"
            )));
        }
        if self.provider_test_mode == ProviderTestMode::MultiReal {
            if !self.multi_provider_validation_enabled {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED must be true when MEMCORE_PROVIDER_TEST_MODE=multi_real"
                        .to_string(),
                ));
            }
            let confirmed = self
                .multi_provider_validation_confirmation
                .as_deref()
                .map(str::trim)
                == Some(MULTI_PROVIDER_VALIDATION_CONFIRMATION);
            if !confirmed {
                return Err(MemcoreError::ValidationError(
                    "MEMCORE_MULTI_PROVIDER_VALIDATION_CONFIRMATION must be exactly I_UNDERSTAND_THIS_WILL_USE_PROVIDER_CREDITS for multi_real mode"
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
pub fn load_settings() -> MemcoreResult<Settings> {
    load_dotenv_if_present();
    Settings::from_env()
}

/// Loads `.env` when present. No-op when the file is missing.
fn load_dotenv_if_present() {
    let _ = dotenvy::dotenv();
}

fn read_env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
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
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        Ok(_) | Err(_) => None,
    }
}

fn parse_provider_fallback_order(key: &str, default: &[String]) -> MemcoreResult<Vec<String>> {
    match env::var(key) {
        Ok(value) => {
            let parsed = value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| part.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if parsed.is_empty() {
                return Err(MemcoreError::ValidationError(format!(
                    "{key} cannot be empty"
                )));
            }
            Ok(parsed)
        }
        Err(_) => Ok(default.to_vec()),
    }
}

fn parse_string_list(key: &str, default: &[String]) -> Vec<String> {
    match env::var(key) {
        Ok(value) => value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
            .collect(),
        Err(_) => default.to_vec(),
    }
}

fn validate_metrics_settings(settings: &Settings) -> MemcoreResult<()> {
    let path = settings.metrics_path.trim();
    if path.is_empty() {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_METRICS_PATH cannot be empty".to_string(),
        ));
    }
    if !path.starts_with('/') {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_METRICS_PATH must start with '/'".to_string(),
        ));
    }
    const RESERVED: &[&str] = &["/health", "/ready", "/api/v1/version"];
    if RESERVED.contains(&path) {
        return Err(MemcoreError::ValidationError(format!(
            "MEMCORE_METRICS_PATH cannot collide with reserved path {path}"
        )));
    }
    Ok(())
}

fn validate_cors_settings(settings: &Settings) -> MemcoreResult<()> {
    for origin in &settings.cors_allowed_origins {
        if !is_valid_cors_origin(origin) {
            return Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_CORS_ALLOWED_ORIGINS value: {origin}"
            )));
        }
    }

    if settings.cors_allow_credentials
        && settings
            .cors_allowed_origins
            .iter()
            .any(|origin| origin.trim() == "*")
    {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_CORS_ALLOWED_ORIGINS cannot include * when MEMCORE_CORS_ALLOW_CREDENTIALS=true"
                .to_string(),
        ));
    }

    for method in &settings.cors_allowed_methods {
        if !is_valid_http_method(method) {
            return Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_CORS_ALLOWED_METHODS value: {method}"
            )));
        }
    }

    for header in settings
        .cors_allowed_headers
        .iter()
        .chain(settings.cors_exposed_headers.iter())
    {
        if header.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "CORS header names cannot be empty".to_string(),
            ));
        }
        if header.chars().any(|ch| ch.is_whitespace()) {
            return Err(MemcoreError::ValidationError(format!(
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

fn parse_u16(key: &str, default: u16) -> MemcoreResult<u16> {
    match env::var(key) {
        Ok(value) => value
            .parse::<u16>()
            .map_err(|_| MemcoreError::ValidationError(format!("{key} must be a valid u16 port"))),
        Err(_) => Ok(default),
    }
}

fn parse_provider_roles(defaults: &ProviderRoleSettings) -> MemcoreResult<ProviderRoleSettings> {
    let text = |key: &str| read_env_optional(key).filter(|v| !v.trim().is_empty());
    let dimensions = match text(MEMCORE_EMBEDDING_DIMENSIONS) {
        None => None,
        Some(value) => Some(value.trim().parse::<usize>().map_err(|_| {
            MemcoreError::ValidationError(
                "MEMCORE_EMBEDDING_DIMENSIONS must be a positive integer".to_string(),
            )
        })?),
    };
    let dedup_threshold = match text(MEMCORE_EMBEDDING_DEDUP_THRESHOLD) {
        None => None,
        Some(value) => Some(value.trim().parse::<f32>().map_err(|_| {
            MemcoreError::ValidationError(
                "MEMCORE_EMBEDDING_DEDUP_THRESHOLD must be a number in (0, 1]".to_string(),
            )
        })?),
    };
    let probe = match text(MEMCORE_PROVIDER_PROBE) {
        None => defaults.probe,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => true,
            "off" | "false" | "0" => false,
            _ => {
                return Err(MemcoreError::ValidationError(format!(
                    "Invalid MEMCORE_PROVIDER_PROBE value: {value} (expected on|off)"
                )));
            }
        },
    };
    let llm_json_mode = match text(MEMCORE_LLM_JSON_MODE) {
        None => defaults.llm_json_mode,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "json_object" => LlmJsonMode::JsonObject,
            "json_schema" => LlmJsonMode::JsonSchema,
            "instruction" => LlmJsonMode::Instruction,
            _ => {
                return Err(MemcoreError::ValidationError(format!(
                    "Invalid MEMCORE_LLM_JSON_MODE value: {value} (expected json_object|json_schema|instruction)"
                )));
            }
        },
    };
    let require_quality_tier = match text(MEMCORE_REQUIRE_QUALITY_TIER) {
        None => defaults.require_quality_tier,
        Some(value) => Some(QualityTier::parse(&value).ok_or_else(|| {
            MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_REQUIRE_QUALITY_TIER value: {value} (expected reference-validated|compatible-unvalidated|development-mock)"
            ))
        })?),
    };
    Ok(ProviderRoleSettings {
        llm: RoleEndpoint {
            base_url: text(MEMCORE_LLM_BASE_URL),
            api_key: text(MEMCORE_LLM_API_KEY),
            allow_no_api_key: parse_bool(MEMCORE_LLM_ALLOW_NO_API_KEY, false)?,
        },
        llm_json_mode,
        embedding: RoleEndpoint {
            base_url: text(MEMCORE_EMBEDDING_BASE_URL),
            api_key: text(MEMCORE_EMBEDDING_API_KEY),
            allow_no_api_key: parse_bool(MEMCORE_EMBEDDING_ALLOW_NO_API_KEY, false)?,
        },
        embedding_options: EmbeddingRoleSettings {
            dimensions,
            send_dimensions: parse_bool(
                MEMCORE_EMBEDDING_SEND_DIMENSIONS,
                defaults.embedding_options.send_dimensions,
            )?,
            normalize: parse_bool(MEMCORE_EMBEDDING_NORMALIZE, false)?,
            query_prefix: read_env_optional(MEMCORE_EMBEDDING_QUERY_PREFIX)
                .filter(|v| !v.is_empty()),
            document_prefix: read_env_optional(MEMCORE_EMBEDDING_DOCUMENT_PREFIX)
                .filter(|v| !v.is_empty()),
            model_revision: text(MEMCORE_EMBEDDING_MODEL_REVISION),
            serving_variant: text(MEMCORE_EMBEDDING_SERVING_VARIANT),
            dedup_threshold,
        },
        allow_insecure_http: parse_bool(MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP, false)?,
        probe,
        require_quality_tier,
        llm_startup_probe: parse_bool(MEMCORE_LLM_STARTUP_PROBE, defaults.llm_startup_probe)?,
    })
}

fn parse_admission(defaults: &AdmissionSettings) -> MemcoreResult<AdmissionSettings> {
    let enabled = match read_env_optional(MEMCORE_ADMISSION_CONTROL) {
        None => defaults.enabled,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" => true,
            "off" | "false" | "0" => false,
            _ => {
                return Err(MemcoreError::ValidationError(format!(
                    "Invalid MEMCORE_ADMISSION_CONTROL value: {value} (expected on|off)"
                )));
            }
        },
    };
    let class = |keys: [&str; 3], d: AdmissionClassLimits| -> MemcoreResult<AdmissionClassLimits> {
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
                MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT,
                MEMCORE_ADMISSION_READ_MAX_QUEUE,
                MEMCORE_ADMISSION_READ_DEADLINE_MS,
            ],
            defaults.read,
        )?,
        write: class(
            [
                MEMCORE_ADMISSION_WRITE_MAX_IN_FLIGHT,
                MEMCORE_ADMISSION_WRITE_MAX_QUEUE,
                MEMCORE_ADMISSION_WRITE_DEADLINE_MS,
            ],
            defaults.write,
        )?,
        admin: class(
            [
                MEMCORE_ADMISSION_ADMIN_MAX_IN_FLIGHT,
                MEMCORE_ADMISSION_ADMIN_MAX_QUEUE,
                MEMCORE_ADMISSION_ADMIN_DEADLINE_MS,
            ],
            defaults.admin,
        )?,
        queue_timeout_ms: parse_u64(
            MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS,
            defaults.queue_timeout_ms,
        )?,
        retry_after_seconds: parse_u64(
            MEMCORE_ADMISSION_RETRY_AFTER_SECONDS,
            defaults.retry_after_seconds,
        )?,
        provider_chat_max_concurrency: parse_usize(
            MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY,
            defaults.provider_chat_max_concurrency,
        )?,
        provider_embedding_max_concurrency: parse_usize(
            MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY,
            defaults.provider_embedding_max_concurrency,
        )?,
        provider_permit_timeout_ms: parse_u64(
            MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS,
            defaults.provider_permit_timeout_ms,
        )?,
    })
}

/// Deterministic startup validation of the admission limits.
fn validate_admission(a: &AdmissionSettings) -> MemcoreResult<()> {
    let invalid = |message: String| Err(MemcoreError::ValidationError(message));
    for (name, c) in [("READ", a.read), ("WRITE", a.write), ("ADMIN", a.admin)] {
        if !(1..=100_000).contains(&c.max_in_flight) {
            return invalid(format!(
                "MEMCORE_ADMISSION_{name}_MAX_IN_FLIGHT must be 1..=100000"
            ));
        }
        if c.max_queue > 1_000_000 {
            return invalid(format!(
                "MEMCORE_ADMISSION_{name}_MAX_QUEUE must be 0..=1000000"
            ));
        }
        if c.deadline_ms <= a.queue_timeout_ms {
            return invalid(format!(
                "MEMCORE_ADMISSION_{name}_DEADLINE_MS must be greater than MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS"
            ));
        }
    }
    if a.queue_timeout_ms == 0 {
        return invalid("MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS must be greater than 0".to_string());
    }
    if !(1..=3_600).contains(&a.retry_after_seconds) {
        return invalid("MEMCORE_ADMISSION_RETRY_AFTER_SECONDS must be 1..=3600".to_string());
    }
    if a.provider_chat_max_concurrency == 0 || a.provider_embedding_max_concurrency == 0 {
        return invalid(
            "MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY and MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY must be greater than 0"
                .to_string(),
        );
    }
    if a.provider_permit_timeout_ms == 0 {
        return invalid("MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS must be greater than 0".to_string());
    }
    Ok(())
}

fn parse_u32(key: &str, default: u32) -> MemcoreResult<u32> {
    match env::var(key) {
        Ok(value) => value.parse::<u32>().map_err(|_| {
            MemcoreError::ValidationError(format!("{key} must be a valid unsigned integer"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_f32(key: &str, default: f32) -> MemcoreResult<f32> {
    match env::var(key) {
        Ok(value) => value.parse::<f32>().map_err(|_| {
            MemcoreError::ValidationError(format!("{key} must be a valid floating-point number"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_u64(key: &str, default: u64) -> MemcoreResult<u64> {
    match env::var(key) {
        Ok(value) => value.parse::<u64>().map_err(|_| {
            MemcoreError::ValidationError(format!("{key} must be a valid unsigned integer"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_optional_usize(key: &str) -> MemcoreResult<Option<usize>> {
    read_env_optional(key)
        .map(|value| {
            value.trim().parse::<usize>().map_err(|_| {
                MemcoreError::ValidationError(format!("{key} must be a valid unsigned integer"))
            })
        })
        .transpose()
}

fn parse_usize(key: &str, default: usize) -> MemcoreResult<usize> {
    match env::var(key) {
        Ok(value) => value.parse::<usize>().map_err(|_| {
            MemcoreError::ValidationError(format!("{key} must be a valid unsigned integer"))
        }),
        Err(_) => Ok(default),
    }
}

fn parse_bool(key: &str, default: bool) -> MemcoreResult<bool> {
    match env::var(key) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            _ => Err(MemcoreError::ValidationError(format!(
                "{key} must be one of: true, false, 1, 0, yes, no"
            ))),
        },
        Err(_) => Ok(default),
    }
}

impl FromStr for Environment {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "production" | "prod" => Ok(Self::Production),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_ENV value: {value}"
            ))),
        }
    }
}

impl FromStr for StorageMode {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "embedded" => Ok(Self::Embedded),
            "production" => Ok(Self::Production),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_STORAGE_MODE value: {value}"
            ))),
        }
    }
}

impl FromStr for VectorBackend {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "lancedb" => Ok(Self::LanceDb),
            "qdrant" => Ok(Self::Qdrant),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_VECTOR_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for FactBackend {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_FACT_BACKEND value: {value}"
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
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_EVENT_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for LlmProviderKind {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "openai" => Ok(Self::OpenAi),
            "openrouter" => Ok(Self::OpenRouter),
            "anthropic" => Ok(Self::Anthropic),
            "groq" => Ok(Self::Groq),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_LLM_PROVIDER value: {value}"
            ))),
        }
    }
}

impl FromStr for EmbeddingProviderKind {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            "openai" => Ok(Self::OpenAi),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_EMBEDDING_PROVIDER value: {value}"
            ))),
        }
    }
}

impl FromStr for ContextCacheBackend {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "memory" => Ok(Self::Memory),
            "redis" => Ok(Self::Redis),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_CONTEXT_CACHE_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for BackgroundJobLockBackend {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "database" => Ok(Self::Database),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_BACKGROUND_JOB_LOCK_BACKEND value: {value}"
            ))),
        }
    }
}

impl FromStr for DatabaseMigrationMode {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "validate_only" | "validate-only" => Ok(Self::ValidateOnly),
            "disabled" => Ok(Self::Disabled),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_DATABASE_MIGRATION_MODE value: {value}"
            ))),
        }
    }
}

impl FromStr for AuthMode {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dev" => Ok(Self::Dev),
            "database" => Ok(Self::Database),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_AUTH_MODE value: {value}"
            ))),
        }
    }
}

impl FromStr for ProviderTestMode {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mock_only" => Ok(Self::MockOnly),
            "single_real" => Ok(Self::SingleReal),
            "multi_real" => Ok(Self::MultiReal),
            "production" => Ok(Self::Production),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_PROVIDER_TEST_MODE value: {value} (expected mock_only|single_real|multi_real|production)"
            ))),
        }
    }
}

impl FromStr for QdrantWriteModeSetting {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "optimized" => Ok(Self::Optimized),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_QDRANT_WRITE_MODE value: {value} (expected legacy|optimized)"
            ))),
        }
    }
}

impl FromStr for ConsolidationMode {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "llm_v2" => Ok(Self::LlmV2),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_CONSOLIDATION_MODE value: {value} (expected legacy|llm_v2)"
            ))),
        }
    }
}

impl FromStr for RelatedFactMatcher {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "semantic" => Ok(Self::Semantic),
            "semantic_v23" => Ok(Self::SemanticV23),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_RELATED_FACT_MATCHER value: {value} (expected legacy|semantic|semantic_v23)"
            ))),
        }
    }
}

impl FromStr for TemporalRewriteSetting {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "preserving" => Ok(Self::Preserving),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_TEMPORAL_REWRITE value: {value} (expected legacy|preserving)"
            ))),
        }
    }
}

impl FromStr for ImportancePolicySetting {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "durable" => Ok(Self::Durable),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_IMPORTANCE_POLICY value: {value} (expected legacy|durable)"
            ))),
        }
    }
}

impl FromStr for ClassificationPromptSetting {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" => Ok(Self::Legacy),
            "operations_v1" => Ok(Self::OperationsV1),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_CLASSIFICATION_PROMPT value: {value} (expected legacy|operations_v1)"
            ))),
        }
    }
}

impl FromStr for ContextPacking {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "prefix" => Ok(Self::Prefix),
            "fill" => Ok(Self::Fill),
            "evidence" => Ok(Self::Evidence),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_CONTEXT_DEFAULT_PACKING value: {value} (expected prefix|fill|evidence)"
            ))),
        }
    }
}

impl FromStr for StructuredExtractionPolicy {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "atomic" => Ok(Self::Atomic),
            "retentive" => Ok(Self::Retentive),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_STRUCTURED_EXTRACTION_POLICY value: {value}"
            ))),
        }
    }
}

impl FromStr for LogFormat {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "pretty" => Ok(Self::Pretty),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_LOG_FORMAT value: {value}"
            ))),
        }
    }
}

impl FromStr for LogLevel {
    type Err = MemcoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(MemcoreError::ValidationError(format!(
                "Invalid MEMCORE_LOG_LEVEL value: {value}"
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
        "MEMCORE_ENV",
        "MEMCORE_HOST",
        "MEMCORE_PORT",
        "MEMCORE_STORAGE_MODE",
        "MEMCORE_VECTOR_BACKEND",
        "MEMCORE_FACT_BACKEND",
        "MEMCORE_EVENT_BACKEND",
        "MEMCORE_DATABASE_URL",
        "MEMCORE_POSTGRES_URL",
        "MEMCORE_DATABASE_MIGRATIONS_ENABLED",
        "MEMCORE_DATABASE_MIGRATION_MODE",
        "MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS",
        "MEMCORE_BACKUP_ENABLED",
        "MEMCORE_BACKUP_DIR",
        "MEMCORE_BACKUP_MAX_FILES",
        "MEMCORE_RESTORE_ENABLED",
        "MEMCORE_SECURITY_HEADERS_ENABLED",
        "MEMCORE_MAX_REQUEST_BODY_BYTES",
        "MEMCORE_CORS_ENABLED",
        "MEMCORE_CORS_ALLOWED_ORIGINS",
        "MEMCORE_CORS_ALLOWED_METHODS",
        "MEMCORE_CORS_ALLOWED_HEADERS",
        "MEMCORE_CORS_EXPOSED_HEADERS",
        "MEMCORE_CORS_ALLOW_CREDENTIALS",
        "MEMCORE_QDRANT_URL",
        "MEMCORE_QDRANT_COLLECTION",
        "MEMCORE_LANCEDB_PATH",
        "MEMCORE_LANCEDB_TABLE",
        "MEMCORE_LLM_PROVIDER",
        "MEMCORE_LLM_MODEL",
        "MEMCORE_EMBEDDING_PROVIDER",
        "MEMCORE_EMBEDDING_MODEL",
        "MEMCORE_ENABLE_PII_REDACTION",
        "MEMCORE_STRUCTURED_EXTRACTION_ENABLED",
        "MEMCORE_EPISODES_ENABLED",
        "MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED",
        "MEMCORE_QUERY_ANALYSIS_ENABLED",
        "MEMCORE_TEMPORAL_RANKING_ENABLED",
        "MEMCORE_TEMPORAL_BOOST",
        "MEMCORE_MULTI_QUERY",
        "MEMCORE_RERANK_PROVIDER",
        "MEMCORE_RERANK_CANDIDATES",
        "MEMCORE_RERANK_TIMEOUT_MS",
        "MEMCORE_RERANK_LOCAL_MODEL_PATH",
        "MEMCORE_RERANK_LOCAL_MODEL_SHA256",
        "MEMCORE_RERANK_LOCAL_TOKENIZER_PATH",
        "MEMCORE_RERANK_LOCAL_TOKENIZER_SHA256",
        "MEMCORE_RERANK_LOCAL_THREADS",
        "MEMCORE_RERANK_LOCAL_BATCH_SIZE",
        "MEMCORE_RERANK_BEDROCK_REGION",
        "MEMCORE_RERANK_BEDROCK_MODEL_ARN",
        "MEMCORE_RERANK_BEDROCK_PROFILE",
        "MEMCORE_QUERY_DECOMPOSITION",
        "MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS",
        "MEMCORE_QDRANT_EPISODE_COLLECTION",
        "MEMCORE_QDRANT_ENSURE_INDEXES",
        "MEMCORE_QDRANT_WRITE_MODE",
        "MEMCORE_QDRANT_SEARCH_HNSW_EF",
        "MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS",
        "MEMCORE_STRUCTURED_EXTRACTION_POLICY",
        "MEMCORE_CONSOLIDATION_MODE",
        "MEMCORE_RELATED_FACT_MATCHER",
        "MEMCORE_TEMPORAL_REWRITE",
        "MEMCORE_IMPORTANCE_POLICY",
        "MEMCORE_CLASSIFICATION_PROMPT",
        "MEMCORE_CONTEXT_DEFAULT_PACKING",
        "MEMCORE_CONSOLIDATION_CANDIDATES",
        "MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR",
        "MEMCORE_ENTITIES_ENABLED",
        "MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD",
        "MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED",
        "MEMCORE_SEARCH_STRATEGY",
        "MEMCORE_RRF_K",
        "MEMCORE_SEARCH_MAX_LIMIT",
        "MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY",
        "MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED",
        "MEMCORE_INGEST_JOURNAL_ENABLED",
        "MEMCORE_INGEST_RECOVERY_STALE_SECS",
        "MEMCORE_INGEST_RECOVERY_INTERVAL_SECS",
        "MEMCORE_INGEST_INFLIGHT_WAIT_MS",
        "MEMCORE_INGEST_RECEIPT_TTL_SECS",
        "MEMCORE_EXPORT_MAX_RECORDS",
        "MEMCORE_LOCAL_EMBEDDING_MODEL_DIR",
        "MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY",
        "MEMCORE_RRF_WEIGHT_SEMANTIC",
        "MEMCORE_RRF_WEIGHT_KEYWORD",
        "MEMCORE_EXTRACTION_WINDOW_TURNS",
        "MEMCORE_EXTRACTION_WINDOW_OVERLAP",
        "MEMCORE_EXTRACTION_CONCURRENCY",
        "MEMCORE_MIN_IMPORTANCE",
        "MEMCORE_AUTH_ENABLED",
        "MEMCORE_AUTH_MODE",
        "MEMCORE_DEV_API_KEY",
        "MEMCORE_API_KEY_PEPPER",
        "MEMCORE_RATE_LIMIT_ENABLED",
        "MEMCORE_LLM_BASE_URL",
        "MEMCORE_LLM_API_KEY",
        "MEMCORE_LLM_ALLOW_NO_API_KEY",
        "MEMCORE_EMBEDDING_BASE_URL",
        "MEMCORE_EMBEDDING_API_KEY",
        "MEMCORE_EMBEDDING_ALLOW_NO_API_KEY",
        "MEMCORE_EMBEDDING_DIMENSIONS",
        "MEMCORE_EMBEDDING_SEND_DIMENSIONS",
        "MEMCORE_EMBEDDING_NORMALIZE",
        "MEMCORE_EMBEDDING_QUERY_PREFIX",
        "MEMCORE_EMBEDDING_DOCUMENT_PREFIX",
        "MEMCORE_EMBEDDING_MODEL_REVISION",
        "MEMCORE_EMBEDDING_SERVING_VARIANT",
        "MEMCORE_EMBEDDING_DEDUP_THRESHOLD",
        "MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP",
        "MEMCORE_PROVIDER_PROBE",
        "MEMCORE_REQUIRE_QUALITY_TIER",
        "MEMCORE_LLM_STARTUP_PROBE",
        "MEMCORE_LLM_JSON_MODE",
        "MEMCORE_ADMISSION_CONTROL",
        "MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS",
        "MEMCORE_ADMISSION_RETRY_AFTER_SECONDS",
        "MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY",
        "MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY",
        "MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS",
        "MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT",
        "MEMCORE_ADMISSION_READ_MAX_QUEUE",
        "MEMCORE_ADMISSION_READ_DEADLINE_MS",
        "MEMCORE_ADMISSION_WRITE_MAX_IN_FLIGHT",
        "MEMCORE_ADMISSION_WRITE_MAX_QUEUE",
        "MEMCORE_ADMISSION_WRITE_DEADLINE_MS",
        "MEMCORE_ADMISSION_ADMIN_MAX_IN_FLIGHT",
        "MEMCORE_ADMISSION_ADMIN_MAX_QUEUE",
        "MEMCORE_ADMISSION_ADMIN_DEADLINE_MS",
        "MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE",
        "MEMCORE_LOG_FORMAT",
        "MEMCORE_LOG_LEVEL",
        "MEMCORE_REQUEST_ID_HEADER",
        "MEMCORE_METRICS_ENABLED",
        "MEMCORE_METRICS_PATH",
        "MEMCORE_METRICS_REQUIRE_AUTH",
        "MEMCORE_METRICS_INCLUDE_PROCESS",
        "MEMCORE_RETENTION_ENABLED",
        "MEMCORE_FACT_RETENTION_DAYS",
        "MEMCORE_EVENT_RETENTION_DAYS",
        "MEMCORE_CONTEXT_CACHE_ENABLED",
        "MEMCORE_CONTEXT_CACHE_BACKEND",
        "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS",
        "MEMCORE_CONTEXT_CACHE_TTL_SECONDS",
        "MEMCORE_CONTEXT_CACHE_MAX_ENTRIES",
        "MEMCORE_CONTEXT_CACHE_KEY_PREFIX",
        "MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED",
        "MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS",
        "MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED",
        "MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS",
        "MEMCORE_CONTEXT_CACHE_METRICS_ENABLED",
        "MEMCORE_PROVIDER_TIMEOUT_SECONDS",
        "MEMCORE_PROVIDER_MAX_RETRIES",
        "MEMCORE_PROVIDER_INITIAL_BACKOFF_MS",
        "MEMCORE_PROVIDER_MAX_BACKOFF_MS",
        "MEMCORE_PROVIDER_BACKOFF_MULTIPLIER",
        "MEMCORE_PROVIDER_RETRY_JITTER_ENABLED",
        "MEMCORE_PROVIDER_FALLBACK_ENABLED",
        "MEMCORE_LLM_FALLBACK_ORDER",
        "MEMCORE_EMBEDDING_FALLBACK_ORDER",
        "MEMCORE_SUMMARIZER_FALLBACK_ORDER",
        "MEMCORE_PROVIDER_CIRCUIT_BREAKER_ENABLED",
        "MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD",
        "MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS",
        "MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS",
        "MEMCORE_PROVIDER_USAGE_METRICS_ENABLED",
        "MEMCORE_PROVIDER_COST_TRACKING_ENABLED",
        "MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED",
        "MEMCORE_PROVIDER_USAGE_RETENTION_DAYS",
        "MEMCORE_QUOTAS_ENABLED",
        "MEMCORE_MAX_USERS_PER_ORG",
        "MEMCORE_MAX_MEMORIES_PER_USER",
        "MEMCORE_MAX_MEMORIES_PER_ORG",
        "MEMCORE_DAILY_PROVIDER_REQUEST_LIMIT",
        "MEMCORE_DAILY_PROVIDER_TOKEN_LIMIT",
        "MEMCORE_PROVIDER_GUARDRAILS_ENABLED",
        "MEMCORE_REAL_PROVIDER_CALLS_ENABLED",
        "MEMCORE_PROVIDER_TEST_MODE",
        "MEMCORE_PROVIDER_MAX_CALLS_PER_RUN",
        "MEMCORE_PROVIDER_MAX_INPUT_CHARS",
        "MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS",
        "MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS",
        "MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS",
        "MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL",
        "MEMCORE_ALLOW_REAL_PROVIDERS_DURING_LOAD_TESTS",
        "MEMCORE_BACKGROUND_JOBS_ALLOW_REAL_PROVIDERS",
        "MEMCORE_MULTI_PROVIDER_VALIDATION_ENABLED",
        "MEMCORE_MULTI_PROVIDER_VALIDATION_CONFIRMATION",
        "MEMCORE_BACKGROUND_JOBS_ENABLED",
        "MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS",
        "MEMCORE_BACKGROUND_JOB_ORG_IDS",
        "MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED",
        "MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS",
        "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_ENABLED",
        "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS",
        "MEMCORE_MEMORY_RETENTION_JOB_ENABLED",
        "MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS",
        "MEMCORE_BACKGROUND_JOB_HISTORY_ENABLED",
        "MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS",
        "MEMCORE_BACKGROUND_JOB_LOCK_ENABLED",
        "MEMCORE_BACKGROUND_JOB_LOCK_BACKEND",
        "MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS",
        "MEMCORE_BACKGROUND_JOB_LOCK_OWNER_ID",
        "MEMCORE_BACKGROUND_JOB_RETRIES_ENABLED",
        "MEMCORE_BACKGROUND_JOB_MAX_RETRIES",
        "MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS",
        "MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS",
        "MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER",
        "MEMCORE_BACKGROUND_JOB_RETRY_JITTER_ENABLED",
        "MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS",
        "MEMCORE_ALLOW_MOCK",
        "MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS",
        "MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS",
        "MEMCORE_REDIS_URL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ];

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let previous = ENV_KEYS
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
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
        for key in ENV_KEYS {
            // SAFETY: tests mutate env only while holding the process-wide mutex.
            unsafe { std::env::remove_var(key) };
        }
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
            std::env::set_var("MEMCORE_SECURITY_HEADERS_ENABLED", "false");
            std::env::set_var("MEMCORE_MAX_REQUEST_BODY_BYTES", "2048");
            std::env::set_var("MEMCORE_CORS_ENABLED", "true");
            std::env::set_var(
                "MEMCORE_CORS_ALLOWED_ORIGINS",
                "https://app.example.com,https://admin.example.com",
            );
            std::env::set_var("MEMCORE_CORS_ALLOWED_METHODS", "GET,POST,OPTIONS");
            std::env::set_var(
                "MEMCORE_CORS_ALLOWED_HEADERS",
                "Authorization,Content-Type,X-Organization-ID,X-Request-ID",
            );
            std::env::set_var("MEMCORE_CORS_EXPOSED_HEADERS", "X-Request-ID");
            std::env::set_var("MEMCORE_CORS_ALLOW_CREDENTIALS", "true");
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
            std::env::set_var("MEMCORE_MAX_REQUEST_BODY_BYTES", "0");
        }

        let error = Settings::from_env().expect_err("zero body limit should fail");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_MAX_REQUEST_BODY_BYTES must be greater than 0")
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
            std::env::set_var("MEMCORE_CORS_ALLOW_CREDENTIALS", "true");
            std::env::set_var("MEMCORE_CORS_ALLOWED_ORIGINS", "*");
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
            std::env::set_var("MEMCORE_CORS_ALLOWED_METHODS", "GET,TRACE");
        }

        let error = Settings::from_env().expect_err("invalid method should fail");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_CORS_ALLOWED_METHODS value")
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
            std::env::set_var("MEMCORE_CORS_ALLOWED_HEADERS", "Authorization, Bad Header");
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
            std::env::set_var("MEMCORE_BACKUP_ENABLED", "true");
            std::env::set_var("MEMCORE_BACKUP_DIR", "./tmp/backups");
            std::env::set_var("MEMCORE_BACKUP_MAX_FILES", "3");
            std::env::set_var("MEMCORE_RESTORE_ENABLED", "true");
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
            std::env::set_var("MEMCORE_BACKUP_DIR", "   ");
        }

        let error = Settings::from_env().expect_err("empty backup dir should fail");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_BACKUP_DIR cannot be empty")
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
            std::env::set_var("MEMCORE_BACKUP_MAX_FILES", "0");
        }

        let error = Settings::from_env().expect_err("zero backup max files should fail");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_BACKUP_MAX_FILES must be greater than 0")
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
            std::env::set_var("MEMCORE_DATABASE_MIGRATIONS_ENABLED", "false");
            std::env::set_var("MEMCORE_DATABASE_MIGRATION_MODE", "validate_only");
            std::env::set_var("MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS", "false");
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
            std::env::set_var("MEMCORE_BACKGROUND_JOBS_ENABLED", "true");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS", "5");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_ORG_IDS", "org_a, org_b,,org_c");
            std::env::set_var("MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_ENABLED", "true");
            std::env::set_var("MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS", "10");
            std::env::set_var("MEMCORE_PROVIDER_USAGE_RETENTION_JOB_ENABLED", "true");
            std::env::set_var(
                "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS",
                "20",
            );
            std::env::set_var("MEMCORE_MEMORY_RETENTION_JOB_ENABLED", "true");
            std::env::set_var("MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS", "30");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_HISTORY_ENABLED", "false");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS", "45");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_LOCK_ENABLED", "true");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_LOCK_BACKEND", "database");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS", "120");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_LOCK_OWNER_ID", "instance-a");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_RETRIES_ENABLED", "false");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_MAX_RETRIES", "0");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS", "100");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS", "500");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER", "1.5");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_RETRY_JITTER_ENABLED", "false");
            std::env::set_var("MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS", "20");
            std::env::set_var("MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS", "15");
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
            "MEMCORE_BACKGROUND_JOB_RUNNER_INTERVAL_SECONDS",
            "MEMCORE_MEMORY_USAGE_SNAPSHOT_JOB_INTERVAL_SECONDS",
            "MEMCORE_PROVIDER_USAGE_RETENTION_JOB_INTERVAL_SECONDS",
            "MEMCORE_MEMORY_RETENTION_JOB_INTERVAL_SECONDS",
            "MEMCORE_BACKGROUND_JOB_LOCK_TTL_SECONDS",
            "MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS",
            "MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS",
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
                "MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS",
                "0",
                "MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS must be greater than 0",
            ),
            (
                "MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS",
                "100",
                "MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS must be >= MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS",
            ),
            (
                "MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER",
                "0.5",
                "MEMCORE_BACKGROUND_JOB_BACKOFF_MULTIPLIER must be >= 1.0",
            ),
        ] {
            let _lock = env_test_lock()
                .lock()
                .expect("env test lock should not be poisoned");
            let _guard = EnvGuard::new();
            clear_env();

            unsafe {
                std::env::set_var("MEMCORE_BACKGROUND_JOB_INITIAL_BACKOFF_MS", "500");
                std::env::set_var("MEMCORE_BACKGROUND_JOB_MAX_BACKOFF_MS", "5000");
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
            std::env::set_var("MEMCORE_BACKGROUND_JOB_LOCK_BACKEND", "redis");
        }

        let error = Settings::from_env().expect_err("invalid backend should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_BACKGROUND_JOB_LOCK_BACKEND value")
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
            std::env::set_var("MEMCORE_ENV", "development");
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
                std::env::set_var("MEMCORE_CONTEXT_DEFAULT_PACKING", value);
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
            std::env::set_var("MEMCORE_CONTEXT_DEFAULT_PACKING", "greedy");
        }
        let err = Settings::from_env().expect_err("an unknown packing is refused at startup");
        assert!(
            err.to_string().contains("MEMCORE_CONTEXT_DEFAULT_PACKING"),
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled");
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "redis");
            std::env::set_var("MEMCORE_REDIS_URL", "redis://localhost:6379");
        }

        let error = Settings::from_env().expect_err("redis backend must be refused");
        assert_eq!(error.code(), "validation_error");
        let message = error.to_string();
        assert!(message.contains("MEMCORE_CONTEXT_CACHE_BACKEND=redis is not supported"));
        assert!(message.contains("MEMCORE_CONTEXT_CACHE_BACKEND=disabled"));
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "redis");
        }

        let error = Settings::from_env().expect_err("redis without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_CONTEXT_CACHE_BACKEND=redis is not supported")
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "not-a-backend");
        }

        let error = Settings::from_env().expect_err("invalid backend should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_CONTEXT_CACHE_BACKEND value")
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_ENABLED", "true");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
        }

        let settings = Settings::from_env().expect("legacy enabled flag should load");
        assert!(settings.context_cache_enabled);
        assert_eq!(
            settings.context_cache_backend,
            super::ContextCacheBackend::Memory
        );
    }

    /// `Settings::from_env` with exactly `vars` set (others cleared).
    fn cache_gate_from_env(vars: &[(&str, &str)]) -> super::MemcoreResult<Settings> {
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
        ("MEMCORE_CONTEXT_CACHE_BACKEND", "memory"),
        ("MEMCORE_ENV", "development"),
        ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
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

    fn refused(result: super::MemcoreResult<Settings>, expected: &str) -> String {
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
            vec![("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled")],
            vec![("MEMCORE_CONTEXT_CACHE_BACKEND", " Disabled ")],
            vec![("MEMCORE_CONTEXT_CACHE_ENABLED", "false")],
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMCORE_CONTEXT_CACHE_ENABLED", "false"),
            ],
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMCORE_ENV", "production"),
            ],
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMCORE_STORAGE_MODE", "production"),
            ],
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
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
            vec![("MEMCORE_CONTEXT_CACHE_BACKEND", "redis")],
            vec![("MEMCORE_CONTEXT_CACHE_BACKEND", " REDIS ")],
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "redis"),
                (
                    "MEMCORE_REDIS_URL",
                    "redis://:s3cret-pass@cache.internal:6379/0",
                ),
                ("MEMCORE_ENV", "development"),
                ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
            ],
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "redis"),
                (
                    "MEMCORE_REDIS_URL",
                    "redis://:s3cret-pass@cache.internal:6379/0",
                ),
                ("MEMCORE_ENV", "production"),
            ],
        ] {
            let message = refused(
                cache_gate_from_env(&vars),
                "MEMCORE_CONTEXT_CACHE_BACKEND=redis is not supported",
            );
            assert!(message.contains("set MEMCORE_CONTEXT_CACHE_BACKEND=disabled"));
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
                    ("MEMCORE_ENV", env),
                    ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", ack),
                ],
            ))
            .expect("accepted spellings");
        }
        // SQLite (embedded) is the supported local store.
        cache_gate_from_env(&with(
            LOCAL_MEMORY,
            &[
                ("MEMCORE_FACT_BACKEND", "sqlite"),
                ("MEMCORE_EVENT_BACKEND", "sqlite"),
                ("MEMCORE_DATABASE_URL", "sqlite::memory:"),
            ],
        ))
        .expect("local sqlite memory cache");

        // CG5: no opt-in / opt-in false.
        let without: Vec<_> = LOCAL_MEMORY
            .iter()
            .filter(|(k, _)| *k != "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS")
            .copied()
            .collect();
        refused(
            cache_gate_from_env(&without),
            "needs MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
        );
        refused(
            cache_gate_from_env(&with(
                LOCAL_MEMORY,
                &[("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "false")],
            )),
            "needs MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
        );
        // CG7 (ambiguous): MEMCORE_ENV unset, even with the opt-in.
        let ambiguous: Vec<_> = LOCAL_MEMORY
            .iter()
            .filter(|(k, _)| *k != "MEMCORE_ENV")
            .copied()
            .collect();
        refused(
            cache_gate_from_env(&ambiguous),
            "needs MEMCORE_ENV set explicitly to development",
        );
    }

    #[test]
    fn cache_gate_memory_is_refused_with_any_production_marker_despite_the_opt_in() {
        for (extra, marker) in [
            (
                vec![("MEMCORE_ENV", "production")],
                "MEMCORE_ENV=production",
            ),
            (vec![("MEMCORE_ENV", "prod")], "MEMCORE_ENV=production"),
            (
                vec![("MEMCORE_STORAGE_MODE", "production")],
                "MEMCORE_STORAGE_MODE=production",
            ),
            (
                vec![
                    ("MEMCORE_FACT_BACKEND", "postgres"),
                    (
                        "MEMCORE_POSTGRES_URL",
                        "postgres://user:pg-pass@db.internal/memcore",
                    ),
                ],
                "MEMCORE_FACT_BACKEND=postgres",
            ),
            (
                vec![
                    ("MEMCORE_EVENT_BACKEND", "postgres"),
                    (
                        "MEMCORE_POSTGRES_URL",
                        "postgres://user:pg-pass@db.internal/memcore",
                    ),
                ],
                "MEMCORE_EVENT_BACKEND=postgres",
            ),
        ] {
            let message = refused(cache_gate_from_env(&with(LOCAL_MEMORY, &extra)), marker);
            assert!(message.contains("process-local cache"), "{message}");
            assert!(!message.contains("pg-pass") && !message.contains("db.internal"));
        }
        // The legacy flag selects memory and is held to the same policy.
        refused(
            cache_gate_from_env(&[
                ("MEMCORE_CONTEXT_CACHE_ENABLED", "true"),
                ("MEMCORE_ENV", "production"),
                ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true"),
            ]),
            "MEMCORE_ENV=production",
        );
        refused(
            cache_gate_from_env(&[
                ("MEMCORE_CONTEXT_CACHE_ENABLED", "true"),
                ("MEMCORE_ENV", "development"),
            ]),
            "needs MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
        );
    }

    #[test]
    fn cache_gate_unknown_malformed_and_contradictory_values_are_refused() {
        refused(
            cache_gate_from_env(&[("MEMCORE_CONTEXT_CACHE_BACKEND", "memcached")]),
            "Invalid MEMCORE_CONTEXT_CACHE_BACKEND value",
        );
        refused(
            cache_gate_from_env(&with(
                LOCAL_MEMORY,
                &[("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "sure")],
            )),
            "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS must be one of",
        );
        refused(
            cache_gate_from_env(&[("MEMCORE_CONTEXT_CACHE_ENABLED", "maybe")]),
            "MEMCORE_CONTEXT_CACHE_ENABLED must be one of",
        );
        for vars in [
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "disabled"),
                ("MEMCORE_CONTEXT_CACHE_ENABLED", "true"),
            ],
            with(LOCAL_MEMORY, &[("MEMCORE_CONTEXT_CACHE_ENABLED", "false")]),
            vec![
                ("MEMCORE_CONTEXT_CACHE_BACKEND", "redis"),
                ("MEMCORE_CONTEXT_CACHE_ENABLED", "false"),
            ],
        ] {
            refused(
                cache_gate_from_env(&vars),
                "MEMCORE_CONTEXT_CACHE_ENABLED contradicts MEMCORE_CONTEXT_CACHE_BACKEND",
            );
        }
        // Agreeing values are fine.
        cache_gate_from_env(&with(
            LOCAL_MEMORY,
            &[("MEMCORE_CONTEXT_CACHE_ENABLED", "true")],
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED", "false");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS", "45");
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var(
                "MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED",
                "true",
            );
            std::env::set_var("MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS", "90");
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var(
                "MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED",
                "true",
            );
            std::env::set_var("MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS", "0");
        }

        let error = Settings::from_env().expect_err("zero stale ttl should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS must be greater than 0")
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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED", "true");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS", "0");
        }

        let error = Settings::from_env().expect_err("zero lock timeout should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS must be greater than 0")
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
            std::env::set_var("MEMCORE_PROVIDER_TIMEOUT_SECONDS", "45");
            std::env::set_var("MEMCORE_PROVIDER_MAX_RETRIES", "0");
            std::env::set_var("MEMCORE_PROVIDER_INITIAL_BACKOFF_MS", "100");
            std::env::set_var("MEMCORE_PROVIDER_MAX_BACKOFF_MS", "500");
            std::env::set_var("MEMCORE_PROVIDER_BACKOFF_MULTIPLIER", "1.5");
            std::env::set_var("MEMCORE_PROVIDER_RETRY_JITTER_ENABLED", "false");
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
            std::env::set_var("MEMCORE_PROVIDER_FALLBACK_ENABLED", "true");
            std::env::set_var("MEMCORE_LLM_FALLBACK_ORDER", "mock,openai");
            std::env::set_var("MEMCORE_EMBEDDING_FALLBACK_ORDER", "mock");
            std::env::set_var("MEMCORE_SUMMARIZER_FALLBACK_ORDER", "mock");
            std::env::set_var("MEMCORE_PROVIDER_CIRCUIT_BREAKER_ENABLED", "false");
            std::env::set_var("MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD", "3");
            std::env::set_var(
                "MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS",
                "30",
            );
            std::env::set_var("MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS", "2");
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
            std::env::set_var("MEMCORE_PROVIDER_CIRCUIT_BREAKER_FAILURE_THRESHOLD", "0");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var(
                "MEMCORE_PROVIDER_CIRCUIT_BREAKER_RESET_TIMEOUT_SECONDS",
                "0",
            );
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMCORE_PROVIDER_CIRCUIT_BREAKER_HALF_OPEN_MAX_CALLS", "0");
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
            std::env::set_var("MEMCORE_PROVIDER_TIMEOUT_SECONDS", "0");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMCORE_PROVIDER_INITIAL_BACKOFF_MS", "0");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMCORE_PROVIDER_INITIAL_BACKOFF_MS", "500");
            std::env::set_var("MEMCORE_PROVIDER_MAX_BACKOFF_MS", "100");
        }
        assert!(Settings::from_env().is_err());

        clear_env();
        unsafe {
            std::env::set_var("MEMCORE_PROVIDER_BACKOFF_MULTIPLIER", "0.5");
        }
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn redis_password_not_exposed_in_sanitized_url_output() {
        use memcore_common::sanitize_redis_url_for_display;

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
            std::env::set_var("MEMCORE_CONTEXT_CACHE_BACKEND", "memory");
            std::env::set_var("MEMCORE_ENV", "development");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", "true");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_TTL_SECONDS", "120");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_MAX_ENTRIES", "50");
            std::env::set_var("MEMCORE_CONTEXT_CACHE_KEY_PREFIX", "custom");
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
            std::env::set_var("MEMCORE_RETENTION_ENABLED", "true");
            std::env::set_var("MEMCORE_FACT_RETENTION_DAYS", "365");
            std::env::set_var("MEMCORE_EVENT_RETENTION_DAYS", "90");
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
            std::env::set_var("MEMCORE_FACT_RETENTION_DAYS", "-1");
        }

        let error = Settings::from_env().expect_err("negative retention days should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_FACT_RETENTION_DAYS must be a valid unsigned integer")
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
            std::env::set_var("MEMCORE_PROVIDER_USAGE_RETENTION_DAYS", "90");
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
            std::env::set_var("MEMCORE_PROVIDER_USAGE_RETENTION_DAYS", "-1");
        }

        let error =
            Settings::from_env().expect_err("negative provider usage retention days should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_PROVIDER_USAGE_RETENTION_DAYS must be a valid unsigned integer")
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
            std::env::set_var("MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS", "-1");
        }

        let error = Settings::from_env()
            .expect_err("negative background job history retention days should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains(
            "MEMCORE_BACKGROUND_JOB_HISTORY_RETENTION_DAYS must be a valid unsigned integer"
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
            std::env::set_var("MEMCORE_QUOTAS_ENABLED", "true");
            std::env::set_var("MEMCORE_MAX_USERS_PER_ORG", "0");
            std::env::set_var("MEMCORE_MAX_MEMORIES_PER_USER", "0");
            std::env::set_var("MEMCORE_MAX_MEMORIES_PER_ORG", "0");
            std::env::set_var("MEMCORE_DAILY_PROVIDER_REQUEST_LIMIT", "0");
            std::env::set_var("MEMCORE_DAILY_PROVIDER_TOKEN_LIMIT", "0");
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
            std::env::set_var("MEMCORE_QUOTAS_ENABLED", "true");
            std::env::set_var("MEMCORE_MAX_USERS_PER_ORG", "10");
            std::env::set_var("MEMCORE_MAX_MEMORIES_PER_USER", "20");
            std::env::set_var("MEMCORE_MAX_MEMORIES_PER_ORG", "30");
            std::env::set_var("MEMCORE_DAILY_PROVIDER_REQUEST_LIMIT", "40");
            std::env::set_var("MEMCORE_DAILY_PROVIDER_TOKEN_LIMIT", "50");
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
            std::env::set_var("MEMCORE_MAX_MEMORIES_PER_ORG", "-1");
        }

        let error = Settings::from_env().expect_err("negative quota should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_MAX_MEMORIES_PER_ORG must be a valid unsigned integer")
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
            std::env::set_var("MEMCORE_FACT_BACKEND", "postgres");
            std::env::set_var("MEMCORE_POSTGRES_URL", "postgres://localhost:5432/memcore");
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
            std::env::set_var("MEMCORE_AUTH_MODE", "database");
        }

        let error = Settings::from_env().expect_err("database auth without pepper should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_API_KEY_PEPPER is required when MEMCORE_AUTH_MODE=database")
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
            std::env::set_var("MEMCORE_AUTH_MODE", "invalid-mode");
        }

        let error = Settings::from_env().expect_err("invalid auth mode should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_AUTH_MODE value")
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
            std::env::set_var("MEMCORE_AUTH_ENABLED", "false");
            std::env::set_var("MEMCORE_DEV_API_KEY", "custom_dev_key");
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
            std::env::set_var("MEMCORE_ENV", "production");
            std::env::set_var("MEMCORE_PORT", "9090");
            std::env::set_var("MEMCORE_STORAGE_MODE", "production");
            std::env::set_var("MEMCORE_VECTOR_BACKEND", "qdrant");
            std::env::set_var("MEMCORE_FACT_BACKEND", "postgres");
            std::env::set_var("MEMCORE_POSTGRES_URL", "postgres://localhost:5432/memcore");
            std::env::set_var("MEMCORE_MIN_IMPORTANCE", "0.8");
            std::env::set_var("MEMCORE_ENABLE_PII_REDACTION", "false");
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
            std::env::set_var("MEMCORE_VECTOR_BACKEND", "lancedb");
        }

        let settings = Settings::from_env().expect("lancedb should still load");
        assert_eq!(settings.vector_backend, VectorBackend::LanceDb);
        settings
            .validate()
            .expect("lancedb needs no extra configuration");
    }

    #[test]
    fn extraction_output_limit_defaults_to_the_provider_ceiling() {
        // Extraction used to hardcode 300 and ignore MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS entirely.
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
                    .contains("must not exceed MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS"),
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
            std::env::set_var("MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS", "1800");
            std::env::set_var("MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS", "150");
        }

        let settings = Settings::from_env().expect("settings should load");
        assert_eq!(settings.extraction_max_output_tokens, Some(1_800));
        assert_eq!(settings.classification_max_output_tokens, Some(150));
        assert_eq!(settings.effective_extraction_max_output_tokens(), 1_800);

        unsafe {
            std::env::set_var("MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS", "lots");
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
        // compiled with (e.g. MEMCORE_VECTOR_BACKEND=mock).
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
        unsafe { std::env::set_var("MEMCORE_PORT", "not-a-port") };

        let error = Settings::from_env().expect_err("invalid port should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_PORT must be a valid u16 port")
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
        unsafe { std::env::set_var("MEMCORE_MIN_IMPORTANCE", "1.5") };

        let error = Settings::from_env().expect_err("out-of-range min importance should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_MIN_IMPORTANCE must be between 0.0 and 1.0")
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
            std::env::set_var("MEMCORE_LLM_PROVIDER", "openai");
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
            .expect_err("mock providers must be refused without MEMCORE_ALLOW_MOCK");
        let message = error.to_string();
        assert!(message.contains("MEMCORE_LLM_PROVIDER and MEMCORE_EMBEDDING_PROVIDER"));
        assert!(message.contains("MEMCORE_ALLOW_MOCK=true"));
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
        assert!(message.contains("MEMCORE_EMBEDDING_PROVIDER resolves to the mock provider"));
        assert!(!message.contains("MEMCORE_LLM_PROVIDER resolves"));
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
        assert!(message.contains("MEMCORE_LLM_FALLBACK_ORDER mixes `mock` with a real provider"));
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
            .expect_err("a mock-only embedding chain needs MEMCORE_ALLOW_MOCK")
            .to_string();
        assert!(message.contains("MEMCORE_EMBEDDING_FALLBACK_ORDER resolves to the mock provider"));
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
            std::env::set_var("MEMCORE_ALLOW_MOCK", "true");
            std::env::set_var("MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS", "5");
        }
        let settings = Settings::from_env().expect("settings load");
        assert!(settings.allow_mock);
        assert_eq!(settings.startup_dependency_timeout_seconds, 5);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS", "3601") };
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
            std::env::set_var("MEMCORE_RATE_LIMIT_ENABLED", "false");
            std::env::set_var("MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE", "120");
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
        unsafe { std::env::set_var("MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE", "0") };

        let error = Settings::from_env().expect_err("zero rate limit should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE must be greater than 0")
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
        unsafe { std::env::set_var("MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE", "not-a-number") };

        let error = Settings::from_env().expect_err("invalid rate limit should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error.to_string().contains(
                "MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE must be a valid unsigned integer"
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
            std::env::set_var("MEMCORE_FACT_BACKEND", "mock");
            std::env::set_var("MEMCORE_EVENT_BACKEND", "postgres");
        }

        let error = Settings::from_env().expect_err("postgres event without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_POSTGRES_URL is required when MEMCORE_EVENT_BACKEND=postgres")
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
            std::env::set_var("MEMCORE_EVENT_BACKEND", "not-a-backend");
        }

        let error = Settings::from_env().expect_err("invalid event backend should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_EVENT_BACKEND value")
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
            std::env::set_var("MEMCORE_FACT_BACKEND", "postgres");
        }

        let error = Settings::from_env().expect_err("postgres without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_POSTGRES_URL is required when MEMCORE_FACT_BACKEND=postgres")
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
        unsafe { std::env::set_var("MEMCORE_LOG_FORMAT", "yaml") };

        let error = Settings::from_env().expect_err("invalid log format should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_LOG_FORMAT value")
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
            std::env::set_var("MEMCORE_EPISODES_ENABLED", "true");
            std::env::set_var("MEMCORE_QDRANT_COLLECTION", "facts_a");
        }
        let settings = Settings::from_env().unwrap();
        assert!(settings.episodes_enabled);
        assert_eq!(settings.qdrant_episode_collection, "facts_a_episodes");

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_QDRANT_EPISODE_COLLECTION", "facts_a") };
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
        unsafe { std::env::set_var("MEMCORE_QDRANT_ENSURE_INDEXES", "false") };
        assert!(!Settings::from_env().unwrap().qdrant_ensure_indexes);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_QDRANT_ENSURE_INDEXES", "sometimes") };
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
            std::env::set_var("MEMCORE_LLM_PROVIDER", "openai");
            std::env::set_var("MEMCORE_EMBEDDING_PROVIDER", "openai");
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
            std::env::set_var("MEMCORE_EMBEDDING_BASE_URL", "http://127.0.0.1:8090/v1");
            std::env::set_var("MEMCORE_EMBEDDING_API_KEY", "sk-emb-INVENTED");
            std::env::set_var("MEMCORE_LLM_BASE_URL", "https://llm.example.test/v1");
            std::env::set_var("MEMCORE_LLM_API_KEY", "sk-llm-INVENTED");
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
        let profile = compose_env_block(&base, "x-memcore-env: &memcore-env", 2);
        let memcore_service = &fallback[fallback
            .find(
                "
  memcore:",
            )
            .expect("memcore service")..];
        let fallback_env = compose_env_block(memcore_service, "    environment:", 6);
        assert!(fallback_env.len() >= 6, "{fallback_env:?}");

        let load = |extra: &[(String, String)]| {
            clear_env();
            for (key, value) in profile.iter().chain(extra.iter()) {
                assert!(ENV_KEYS.contains(&key.as_str()), "unknown setting {key}");
                assert!(
                    !key.starts_with("AWS_")
                        && ![
                            "OPENAI_API_KEY",
                            "MEMCORE_LLM_API_KEY",
                            "MEMCORE_EMBEDDING_API_KEY"
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

    /// `MEMCORE_QDRANT_SEARCH_HNSW_EF` is unset by default (the application default is
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
            unsafe { std::env::set_var("MEMCORE_QDRANT_SEARCH_HNSW_EF", value) };
            assert_eq!(
                Settings::from_env().unwrap().qdrant_search_hnsw_ef,
                expected,
                "{value}"
            );
        }
        for bad in ["0", "4097", "-1", "abc", "12.5"] {
            // SAFETY: as above.
            unsafe { std::env::set_var("MEMCORE_QDRANT_SEARCH_HNSW_EF", bad) };
            let error = Settings::from_env().expect_err(bad).to_string();
            assert!(error.contains("MEMCORE_QDRANT_SEARCH_HNSW_EF"), "{error}");
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
    /// `reference-validated` only with `MEMCORE_QDRANT_SEARCH_HNSW_EF=256`. Unset (the rollback) and
    /// any other beam are `compatible-unvalidated`, and
    /// `MEMCORE_REQUIRE_QUALITY_TIER=reference-validated` refuses them, naming the setting; a
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
            unsafe { std::env::set_var("MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS", value) };
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
            unsafe { std::env::set_var("MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS", bad) };
            let error = Settings::from_env().expect_err(bad).to_string();
            assert!(
                error.contains("MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS"),
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
            std::env::set_var("MEMCORE_REQUIRE_QUALITY_TIER", "reference-validated");
            std::env::set_var("MEMCORE_LLM_STARTUP_PROBE", "true");
            std::env::set_var("MEMCORE_LLM_JSON_MODE", "instruction");
        }
        let s = Settings::from_env().unwrap();
        assert_eq!(
            s.providers.require_quality_tier,
            Some(QualityTier::ReferenceValidated)
        );
        assert!(s.providers.llm_startup_probe);
        assert_eq!(s.providers.llm_json_mode, LlmJsonMode::Instruction);
        unsafe { std::env::set_var("MEMCORE_REQUIRE_QUALITY_TIER", "gold") };
        assert!(Settings::from_env().is_err());
        clear_env();
    }

    /// `MEMCORE_LLM_JSON_MODE` defaults to `json_object` (request bodies unchanged).
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
            unsafe { std::env::set_var("MEMCORE_LLM_JSON_MODE", value) };
            assert_eq!(
                Settings::from_env().unwrap().providers.llm_json_mode,
                expected
            );
        }
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_LLM_JSON_MODE", "grammar") };
        assert!(Settings::from_env().is_err());
    }

    #[test]
    fn byok_endpoint_validation_refuses_unsafe_combinations() {
        let _lock = env_test_lock()
            .lock()
            .expect("env test lock should not be poisoned");
        let _guard = EnvGuard::new();
        let base = [
            ("MEMCORE_LLM_PROVIDER", "mock"),
            ("MEMCORE_ALLOW_MOCK", "true"),
            ("MEMCORE_EMBEDDING_PROVIDER", "openai"),
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
        assert!(case(&[("MEMCORE_EMBEDDING_BASE_URL", "http://localhost:8080/v1")]).is_ok());
        assert!(case(&[("MEMCORE_EMBEDDING_BASE_URL", "http://[::1]:8080")]).is_ok());
        let error = case(&[("MEMCORE_EMBEDDING_BASE_URL", "https://emb.example.test/v1")])
            .expect_err("remote without key");
        assert!(
            error.to_string().contains("MEMCORE_EMBEDDING_API_KEY"),
            "{error}"
        );
        assert!(
            case(&[
                ("MEMCORE_EMBEDDING_BASE_URL", "https://emb.example.test/v1"),
                ("MEMCORE_EMBEDDING_ALLOW_NO_API_KEY", "true"),
            ])
            .is_ok()
        );
        // A key over plain http to a remote host is refused unless explicitly allowed.
        let error = case(&[
            ("MEMCORE_EMBEDDING_BASE_URL", "http://emb.example.test/v1"),
            ("MEMCORE_EMBEDDING_API_KEY", "sk-INVENTED-secret"),
        ])
        .expect_err("plain http with a key");
        assert!(!error.to_string().contains("sk-INVENTED-secret"), "{error}");
        assert!(
            case(&[
                ("MEMCORE_EMBEDDING_BASE_URL", "http://emb.example.test/v1"),
                ("MEMCORE_EMBEDDING_API_KEY", "sk-INVENTED-secret"),
                ("MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP", "true"),
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
                ("MEMCORE_EMBEDDING_BASE_URL", bad),
                ("MEMCORE_EMBEDDING_API_KEY", "sk-INVENTED"),
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
                ("MEMCORE_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
                ("MEMCORE_EMBEDDING_DIMENSIONS", "0")
            ])
            .is_err()
        );
        assert!(
            case(&[
                ("MEMCORE_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
                ("MEMCORE_EMBEDDING_DEDUP_THRESHOLD", "1.5")
            ])
            .is_err()
        );
        assert!(
            case(&[
                ("MEMCORE_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
                ("MEMCORE_PROVIDER_PROBE", "sometimes")
            ])
            .is_err()
        );
        let s = case(&[
            ("MEMCORE_EMBEDDING_BASE_URL", "http://127.0.0.1:1"),
            ("MEMCORE_EMBEDDING_DIMENSIONS", "384"),
            ("MEMCORE_EMBEDDING_DEDUP_THRESHOLD", "0.95"),
            ("MEMCORE_PROVIDER_PROBE", "off"),
            ("MEMCORE_EMBEDDING_SEND_DIMENSIONS", "false"),
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
            std::env::set_var("MEMCORE_ADMISSION_CONTROL", "off");
            std::env::set_var("MEMCORE_ADMISSION_WRITE_MAX_IN_FLIGHT", "3");
            std::env::set_var("MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY", "2");
        }
        let a = Settings::from_env().unwrap().admission;
        assert!(!a.enabled);
        assert_eq!(a.write.max_in_flight, 3);
        assert_eq!(a.provider_chat_max_concurrency, 2);

        for (key, value) in [
            ("MEMCORE_ADMISSION_CONTROL", "maybe"),
            ("MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT", "0"),
            ("MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT", "-1"),
            ("MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS", "0"),
            // Below the 2,000 ms default queue timeout.
            ("MEMCORE_ADMISSION_READ_DEADLINE_MS", "1000"),
            ("MEMCORE_ADMISSION_RETRY_AFTER_SECONDS", "0"),
            ("MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY", "0"),
            ("MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS", "0"),
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
            unsafe { std::env::set_var("MEMCORE_QDRANT_WRITE_MODE", value) };
            assert_eq!(Settings::from_env().unwrap().qdrant_write_mode, expected);
        }
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_QDRANT_WRITE_MODE", "fast") };
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
        set("MEMCORE_QUERY_ANALYSIS_ENABLED", "true");
        assert!(Settings::from_env().is_err());
        set("MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED", "true");
        assert!(Settings::from_env().is_ok());
        // Temporal ranking and multi-query need analysis.
        set("MEMCORE_QUERY_ANALYSIS_ENABLED", "false");
        set("MEMCORE_TEMPORAL_RANKING_ENABLED", "true");
        assert!(Settings::from_env().is_err());
        set("MEMCORE_QUERY_ANALYSIS_ENABLED", "true");
        set("MEMCORE_MULTI_QUERY", "deterministic");
        set("MEMCORE_TEMPORAL_BOOST", "0.1");
        let settings = Settings::from_env().unwrap();
        assert_eq!(settings.multi_query, MultiQuerySetting::Deterministic);
        assert_eq!(settings.temporal_boost, 0.1);
        // Bedrock needs no credentials at settings time; local needs its four file settings.
        // No default region; the model ARN defaults to Amazon Rerank 1.0 in the configured
        // region and must name that region.
        set("MEMCORE_RERANK_PROVIDER", "bedrock");
        assert!(Settings::from_env().is_err(), "bedrock needs a region");
        set("MEMCORE_RERANK_BEDROCK_REGION", "eu-central-1");
        let settings = Settings::from_env().unwrap();
        assert_eq!(settings.rerank_provider, RerankProviderSetting::Bedrock);
        assert_eq!(settings.rerank_bedrock_region, "eu-central-1");
        assert_eq!(
            settings.rerank_bedrock_model_arn,
            "arn:aws:bedrock:eu-central-1::foundation-model/amazon.rerank-v1:0"
        );
        set("MEMCORE_RERANK_BEDROCK_MODEL_ARN", "amazon.rerank-v1:0");
        assert!(
            Settings::from_env().is_err(),
            "a bare model id is not an ARN"
        );
        set(
            "MEMCORE_RERANK_BEDROCK_MODEL_ARN",
            "arn:aws:bedrock:us-west-2::foundation-model/amazon.rerank-v1:0",
        );
        assert!(
            Settings::from_env().is_err(),
            "the ARN must name the configured region"
        );
        set("MEMCORE_RERANK_BEDROCK_REGION", "us-west-2");
        assert!(Settings::from_env().is_ok(), "any region can be configured");
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("MEMCORE_RERANK_BEDROCK_MODEL_ARN");
            std::env::remove_var("MEMCORE_RERANK_BEDROCK_REGION");
        }
        set("MEMCORE_RERANK_PROVIDER", "local");
        assert!(Settings::from_env().is_err());
        for (key, value) in [
            ("MEMCORE_RERANK_LOCAL_MODEL_PATH", "m.onnx"),
            ("MEMCORE_RERANK_LOCAL_MODEL_SHA256", "ab"),
            ("MEMCORE_RERANK_LOCAL_TOKENIZER_PATH", "t.json"),
            ("MEMCORE_RERANK_LOCAL_TOKENIZER_SHA256", "cd"),
        ] {
            set(key, value);
        }
        assert_eq!(
            Settings::from_env().unwrap().rerank_provider,
            RerankProviderSetting::Local
        );
        set("MEMCORE_QUERY_DECOMPOSITION", "llm");
        assert_eq!(
            Settings::from_env().unwrap().query_decomposition,
            QueryDecompositionSetting::Llm
        );
        set("MEMCORE_QUERY_DECOMPOSITION", "noop");
        assert_eq!(
            Settings::from_env().unwrap().query_decomposition,
            QueryDecompositionSetting::Noop
        );
        for (key, bad) in [
            ("MEMCORE_TEMPORAL_BOOST", "0"),
            ("MEMCORE_TEMPORAL_BOOST", "1.5"),
            ("MEMCORE_RERANK_CANDIDATES", "0"),
            ("MEMCORE_MULTI_QUERY", "llm"),
            ("MEMCORE_RERANK_PROVIDER", "cohere"),
            ("MEMCORE_RERANK_TIMEOUT_MS", "5"),
            ("MEMCORE_RERANK_LOCAL_BATCH_SIZE", "0"),
            ("MEMCORE_QUERY_DECOMPOSITION", "nova-lite"),
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
        unsafe { std::env::set_var("MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED", "true") };
        assert!(
            Settings::from_env()
                .unwrap()
                .experimental_candidates_enabled
        );
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED", "maybe") };
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
            unsafe { std::env::set_var("MEMCORE_SEARCH_MAX_LIMIT", value) };
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
            unsafe { std::env::set_var("MEMCORE_LOCAL_EMBEDDING_QUEUE_CAPACITY", value) };
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
        unsafe { std::env::set_var("MEMCORE_INGEST_JOURNAL_ENABLED", "false") };
        assert!(!Settings::from_env().unwrap().ingest_journal_enabled);
        // SAFETY: as above. The stale window must exceed the write deadline (default 120 s).
        unsafe { std::env::set_var("MEMCORE_INGEST_JOURNAL_ENABLED", "true") };
        unsafe { std::env::set_var("MEMCORE_INGEST_RECOVERY_STALE_SECS", "120") };
        assert!(Settings::from_env().is_err());
        unsafe { std::env::set_var("MEMCORE_INGEST_RECOVERY_STALE_SECS", "121") };
        assert!(Settings::from_env().is_ok());
        unsafe { std::env::set_var("MEMCORE_INGEST_RECOVERY_INTERVAL_SECS", "0") };
        assert!(Settings::from_env().is_err());
        // SAFETY: as above. Unset, the window follows a longer write deadline (2x, at least 300 s).
        unsafe { std::env::remove_var("MEMCORE_INGEST_RECOVERY_INTERVAL_SECS") };
        unsafe { std::env::remove_var("MEMCORE_INGEST_RECOVERY_STALE_SECS") };
        unsafe { std::env::set_var("MEMCORE_ADMISSION_WRITE_DEADLINE_MS", "900000") };
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
        unsafe { std::env::set_var("MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED", "true") };
        assert!(
            Settings::from_env()
                .unwrap()
                .classification_skip_when_no_related
        );
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED", "sometimes") };
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
            unsafe { std::env::set_var("MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY", value) };
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
            std::env::set_var("MEMCORE_CONSOLIDATION_MODE", "LLM_V2");
            std::env::set_var("MEMCORE_CONSOLIDATION_CANDIDATES", "20");
            std::env::set_var("MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR", "0.3");
        };
        let settings = Settings::from_env().expect("v2 should load");
        assert_eq!(settings.consolidation_mode, super::ConsolidationMode::LlmV2);
        assert_eq!(settings.consolidation_candidates, 20);
        assert_eq!(settings.consolidation_similarity_floor, 0.3);

        for (key, value) in [
            ("MEMCORE_CONSOLIDATION_MODE", "v2"),
            ("MEMCORE_CONSOLIDATION_CANDIDATES", "0"),
            ("MEMCORE_CONSOLIDATION_CANDIDATES", "51"),
            ("MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR", "1.5"),
            ("MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR", "-0.1"),
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
            std::env::set_var("MEMCORE_ENTITIES_ENABLED", "true");
            std::env::set_var("MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD", "0.8");
            std::env::set_var("MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED", "false");
        };
        let settings = Settings::from_env().expect("entities should load");
        assert!(settings.entities_enabled);
        assert_eq!(settings.entity_name_similarity_threshold, 0.8);
        assert!(!settings.entity_llm_confirmation_enabled);

        for value in ["1.5", "-0.1"] {
            clear_env();
            // SAFETY: as above.
            unsafe { std::env::set_var("MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD", value) };
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
            std::env::set_var("MEMCORE_SEARCH_STRATEGY", "RRF");
            std::env::set_var("MEMCORE_RRF_K", "20");
        };
        let settings = Settings::from_env().expect("rrf should load");
        assert_eq!(settings.search_strategy, super::SearchStrategy::Rrf);
        assert_eq!(settings.rrf_k, 20.0);

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_RRF_K", "0") };
        assert!(Settings::from_env().is_err(), "k must be positive");
        // SAFETY: as above.
        unsafe {
            std::env::set_var("MEMCORE_RRF_K", "60");
            std::env::set_var("MEMCORE_SEARCH_STRATEGY", "bm25");
        };
        assert!(Settings::from_env().is_err(), "unknown strategy must fail");

        // SAFETY: as above.
        unsafe {
            std::env::set_var("MEMCORE_SEARCH_STRATEGY", "rrf");
            std::env::set_var("MEMCORE_RRF_WEIGHT_KEYWORD", "0.5");
        };
        let settings = Settings::from_env().expect("weights should load");
        assert_eq!(settings.rrf_weight_keyword, 0.5);
        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_RRF_WEIGHT_KEYWORD", "-1") };
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
            std::env::set_var("MEMCORE_STRUCTURED_EXTRACTION_POLICY", "Atomic");
            std::env::set_var("MEMCORE_STRUCTURED_EXTRACTION_ENABLED", "false");
        };
        let settings = Settings::from_env().expect("atomic should load");
        assert!(!settings.structured_extraction_enabled);
        assert_eq!(
            settings.structured_extraction_policy,
            super::StructuredExtractionPolicy::Atomic
        );

        // SAFETY: as above.
        unsafe { std::env::set_var("MEMCORE_STRUCTURED_EXTRACTION_POLICY", "verbose") };
        let error = Settings::from_env().expect_err("unknown policy should fail");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_STRUCTURED_EXTRACTION_POLICY value")
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
        unsafe { std::env::set_var("MEMCORE_LOG_LEVEL", "verbose") };

        let error = Settings::from_env().expect_err("invalid log level should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_LOG_LEVEL value")
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
            std::env::set_var("MEMCORE_LOG_FORMAT", "pretty");
            std::env::set_var("MEMCORE_LOG_LEVEL", "debug");
            std::env::set_var("MEMCORE_REQUEST_ID_HEADER", "X-Correlation-ID");
            std::env::set_var("MEMCORE_METRICS_ENABLED", "true");
            std::env::set_var("MEMCORE_METRICS_PATH", "/internal/metrics");
            std::env::set_var("MEMCORE_METRICS_REQUIRE_AUTH", "false");
            std::env::set_var("MEMCORE_METRICS_INCLUDE_PROCESS", "true");
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
            std::env::set_var("MEMCORE_METRICS_PATH", "metrics");
        }

        let error = Settings::from_env().expect_err("invalid metrics path should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains("MEMCORE_METRICS_PATH"));
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
            std::env::set_var("MEMCORE_METRICS_PATH", "/health");
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

        let dir = std::env::temp_dir().join(format!("memcore-dotenv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        std::fs::write(dir.join(".env"), "MEMCORE_PORT=9123\n").expect(".env should be written");

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
            "memcore-dotenv-override-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        std::fs::write(dir.join(".env"), "MEMCORE_PORT=9123\n").expect(".env should be written");

        // SAFETY: tests mutate env only while holding the process-wide mutex.
        unsafe { std::env::set_var("MEMCORE_PORT", "7777") };

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
            std::env::set_var("MEMCORE_VECTOR_BACKEND", "qdrant");
            std::env::set_var("MEMCORE_QDRANT_URL", "   ");
        }

        let error = Settings::from_env().expect_err("qdrant without url should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("MEMCORE_QDRANT_URL is required when MEMCORE_VECTOR_BACKEND=qdrant")
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
            std::env::set_var("MEMCORE_VECTOR_BACKEND", "qdrant");
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
            std::env::set_var("MEMCORE_VECTOR_BACKEND", "qdrant");
            std::env::set_var("MEMCORE_QDRANT_COLLECTION", "custom_vectors");
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
        unsafe { std::env::set_var("MEMCORE_VECTOR_BACKEND", "bad-backend") };

        let error = Settings::from_env().expect_err("invalid enum should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(
            error
                .to_string()
                .contains("Invalid MEMCORE_VECTOR_BACKEND value")
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
            std::env::set_var("MEMCORE_PROVIDER_TEST_MODE", "unsafe_all");
        }
        let error = Settings::from_env().expect_err("invalid test mode should fail");
        assert_eq!(error.code(), "validation_error");
        assert!(error.to_string().contains("MEMCORE_PROVIDER_TEST_MODE"));
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
            std::env::set_var("MEMCORE_PROVIDER_TEST_MODE", "single_real");
            std::env::set_var("MEMCORE_REAL_PROVIDER_CALLS_ENABLED", "true");
            std::env::set_var("MEMCORE_PROVIDER_MAX_CALLS_PER_RUN", "6");
            std::env::set_var("MEMCORE_PROVIDER_MAX_INPUT_CHARS", "500");
            std::env::set_var("MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS", "256");
            std::env::set_var("MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL", "0");
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
