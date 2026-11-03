pub mod admin;
pub mod audit;
pub mod backup;
pub mod candidates;
pub mod consolidation;
pub mod context;
pub mod d76_context;
pub mod deadline;
pub mod dedup;
pub mod engine;
pub mod entities;
pub mod export;
pub mod import;
pub mod importance;
pub mod jobs;
pub mod lifecycle;
pub mod models;
pub mod observability;
pub mod org;
pub mod pagination;
pub mod ports;
pub mod privacy;
pub mod query;
pub mod quota;
pub mod ranking;
pub mod retention;
pub mod search;
pub mod shutdown;
pub mod temporal;
/// Tenant-safe vector verification and repair (`verify-vectors`).
pub mod verify;

pub use admin::{
    CreateMemoryUsageSnapshotInput, CreateMemoryUsageSnapshotOutput, DEFAULT_LIST_ORG_USERS_LIMIT,
    DEFAULT_MEMORY_USAGE_SNAPSHOT_LIMIT, DEFAULT_ORG_USAGE_DASHBOARD_DAYS,
    DEFAULT_SEARCH_ORG_MEMORY_EVENTS_LIMIT, ListOrgUsersInput, ListOrgUsersOutput,
    MAX_LIST_ORG_USERS_LIMIT, MAX_MEMORY_USAGE_SNAPSHOT_LIMIT, MAX_ORG_USAGE_DASHBOARD_DAYS,
    MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT, MemoryUsageLatestSnapshot, MemoryUsageSnapshot,
    OrgMemoryUsageSummary, OrgSummaryInput, OrgSummaryOutput, OrgUsageDashboardInput,
    OrgUsageDashboardOutput, ProviderUsageDailyBucket, ProviderUsageDailyInput,
    ProviderUsageDailyOutput, ProviderUsageDashboardSummary, QueryMemoryUsageSnapshotsInput,
    QueryMemoryUsageSnapshotsOutput, SearchOrgMemoryEventsInput, SearchOrgMemoryEventsOutput,
    empty_provider_usage_summary, resolve_org_usage_window, validate_memory_usage_snapshot_limit,
    validate_org_usage_days,
};
pub use backup::{
    BACKUP_FILE_PREFIX, BACKUP_FILE_SUFFIX, BackupBackend, BackupRequest, BackupResult,
    RestoreConfirmation, RestoreResult, RestoreValidationResult, build_sqlite_backup_filename,
    file_sha256_hex, is_memcore_sqlite_backup_filename, resolve_backup_path,
    rotate_memcore_sqlite_backups, sanitize_backup_label, validate_backup_output_dir,
};
pub use candidates::{
    Candidate, CandidateKind, CandidatePipelineConfig, DEFAULT_CANDIDATE_EPISODE_LIMIT,
    DEFAULT_CANDIDATE_FACT_LIMIT, DEFAULT_CANDIDATE_WINDOW, DEFAULT_TEMPORAL_BOOST, MultiQueryMode,
    SearchCandidatesInput, SearchCandidatesOutput, merge_by_vector_score, merge_multi_query,
};
pub use consolidation::{
    ConsolidationConfig, DEFAULT_CONSOLIDATION_CANDIDATES, DEFAULT_CONSOLIDATION_SIMILARITY_FLOOR,
};
pub use context::{
    AssembledContext, BuildContextInput, BuildContextOutput, CachedContextEntry, ContextBudget,
    ContextBudgetUsage, ContextCache, ContextCacheConfig, ContextCacheCoordinator, ContextCacheKey,
    ContextCacheMetricsRecorder, ContextCacheMetricsSnapshot, ContextCacheStampedeConfig,
    ContextCacheUsage, ContextCompressionMode, ContextCompressionOptions, ContextCompressionUsage,
    ContextFormat, ContextFormatOptions, ContextFormatter, ContextMemoryItem, ContextSummarizer,
    DEFAULT_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS, DEFAULT_CONTEXT_CACHE_MAX_ENTRIES,
    DEFAULT_CONTEXT_CACHE_TTL_SECONDS, DEFAULT_CONTEXT_MAX_MEMORIES, DEFAULT_CONTEXT_MAX_TOKENS,
    DEFAULT_CONTEXT_RESERVED_TOKENS, DEFAULT_SUMMARY_MAX_TOKENS, EMPTY_CONTEXT_MESSAGE,
    FormattedContext, InMemoryContextCache, InMemoryContextCacheMetrics, LlmContextSummarizer,
    MAX_CONTEXT_MAX_MEMORIES, MAX_CONTEXT_MAX_TOKENS, MAX_SUMMARY_MAX_TOKENS,
    NoopContextCacheMetrics, SimpleContextCompressor, SimpleContextSummarizer,
    SimpleTokenEstimator, TokenEstimator, apply_provider_compression_summary, assemble_context,
    assemble_context_with_budget, build_context_cache_key, cached_entry_from_output,
    cached_entry_with_ttl, context_cache_metrics_recorder, effective_summary_budget,
    stable_sha256_hex, summarize_skipped_memories,
};
pub use dedup::{
    DEFAULT_EMBEDDING_DEDUP_SEARCH_LIMIT, DEFAULT_EMBEDDING_DEDUP_SIMILARITY_THRESHOLD,
    DeduplicationDecision, EXACT_DUPLICATE_THRESHOLD, EmbeddingDeduplicationConfig, ExactDedupRule,
    HIGH_SIMILARITY_DUPLICATE_THRESHOLD, MODERATE_SIMILARITY_THRESHOLD, detect_duplicate,
    detect_embedding_duplicate, exact_dedup_key, find_existing_facts_for_dedup, normalize_content,
};
pub use engine::{
    AddMemoryInput, AddMemoryOutput, CompleteExportInput, DEFAULT_LIST_MEMORIES_LIMIT,
    DEFAULT_LIST_MEMORY_EVENTS_LIMIT, DEFAULT_MIN_IMPORTANCE, DEFAULT_SEARCH_LIMIT,
    DeleteMemoryInput, DeleteMemoryOutput, EpisodeComponents, EpisodeLifecycle,
    EpisodeSearchResult, ExportUserDataInput, FactDiscardCounts, FactDiscardReason,
    ForgetUserInput, ForgetUserOutput, HISTORY_MARKER, HistoricalResult, ListMemoriesInput,
    ListMemoriesOutput, ListMemoryEventsInput, ListMemoryEventsOutput,
    MAX_CONFIGURABLE_SEARCH_LIMIT, MAX_LIST_MEMORIES_LIMIT, MAX_LIST_MEMORY_EVENTS_LIMIT,
    MAX_SEARCH_LIMIT, MemoryEngine, MemoryOperationSummary, MemorySession, QueryEmbeddingCache,
    QueryEmbeddingCacheStats, RetrievalMode, SearchEpisodesInput, SearchEpisodesOutput,
    SearchMemoryInput, SearchMemoryOutput, SearchOptions, StructuredExtractionConfig,
    TemporalRewrite, extraction_windows,
};
pub use engine::{
    DEFAULT_ENTITY_LIST_LIMIT, EntityComponents, EntityDetail, EntityFactView, EntityRelationView,
    ListEntitiesInput, ListEntitiesOutput, MAX_ENTITY_LIST_LIMIT,
};
pub use engine::{
    DEFAULT_INGEST_INFLIGHT_WAIT, DEFAULT_INGEST_RECEIPT_TTL, DEFAULT_INGEST_RECOVERY_BATCH,
    DEFAULT_INGEST_RECOVERY_STALE_AFTER, IngestJournalComponents, IngestRecoveryReport,
    IngestRecoveryStatus,
};
pub use entities::annotation::{
    AnnotatedWindow, AnnotationAttemptStatus, AnnotationAudit, annotate_window, ground_annotation,
};
pub use entities::{
    DEFAULT_ENTITY_NAME_SIMILARITY_THRESHOLD, EntityConfig, EntityProcessingSummary,
};
pub use export::{
    COMPLETE_EXPORT_ATTEMPTS, COMPLETE_EXPORT_DEFAULT_MAX_RECORDS,
    COMPLETE_EXPORT_DEFAULT_PAGE_SIZE, COMPLETE_EXPORT_MAX_PAGE_SIZE, EXPORT_COMPLETE_MODE,
    EXPORT_EPISODES_LIMIT, EXPORT_EVENTS_LIMIT, EXPORT_FACTS_LIMIT, EntityExport,
    ExportCompleteness, ExportSectionCounts, ExportSectionTruncation, USER_EXPORT_FORMAT_VERSION,
    USER_EXPORT_FORMAT_VERSION_V2, USER_EXPORT_FORMAT_VERSION_V3, UserMemoryExport,
};
pub use import::{
    DUPLICATE_FACT_ID, ImportMode, ImportUserDataInput, ImportUserDataOutput,
    ImportValidationIssue, ImportValidationSummary, derived_import_fact_id,
};
pub use importance::{DURABLE_FACT_IMPORTANCE_FLOOR, ImportancePolicy, ImportanceScorer};
pub use jobs::{
    BackgroundJob, BackgroundJobDefinition, BackgroundJobKind, BackgroundJobRetryPolicy,
    BackgroundJobRetryState, BackgroundJobRun, BackgroundJobRunner, BackgroundJobSnapshot,
    BackgroundJobStatus, InMemoryBackgroundJobState, MemoryRetentionJob, MemoryUsageSnapshotJob,
    ProviderUsageRetentionJob, calculate_background_job_backoff,
    execute_background_job_with_retries, is_retryable_job_error,
};
pub use lifecycle::RelatedFactMatcher;
pub use models::{
    AliasSource, ApiKeyRecord, ApiKeyScope, CandidateFact, Entity, EntityAlias, EntityRelation,
    EntityRole, EntityType, Episode, Fact, FactEntityLink, FactOperation, FactOperationDecision,
    FactStructure, MIN_EMBEDDED_EPISODE_CHARS, MemoryEvent, MemoryEventOperation,
    MemorySearchResult, MemorySource, MemoryType, TemporalGranularity, TemporalValue,
    TenantContext, message_role_from_str, message_role_to_str, normalize_entity_name,
};
pub use org::{OrgPlanConfig, OrgPlanLimits, OrgPlanTier, validate_org_plan_metadata};
pub use pagination::{
    Page, PageCursor, build_page, decode_cursor, encode_cursor, is_after_cursor_in_desc_order,
    page_fetch_limit, parse_optional_cursor,
};
pub use ports::{
    AcquiredJobLock, ApiKeyListQuery, ApiKeyStore, BackgroundJobLockStore, BackgroundJobRunQuery,
    BackgroundJobRunQueryResult, BackgroundJobRunStore, CandidateSkipCounts, CandidateSkipReason,
    ConsolidationDecision, ConsolidationInput, ConsolidationOperation,
    DEFAULT_BACKGROUND_JOB_RUN_LIMIT, DEFAULT_MEMORY_EVENT_LIST_LIMIT,
    DEFAULT_PROVIDER_USAGE_LIMIT, EmbeddingIdentity, EmbeddingProvider, EpisodeStore,
    ExtractionMode, FactClassificationInput, FactExtractionInput, FactExtractionOutput,
    FactSearchQuery, FactStore, JobLockKey, JobLockRecord, LlmProvider,
    MAX_BACKGROUND_JOB_RUN_LIMIT, MAX_MEMORY_EVENT_LIST_LIMIT, MAX_PROVIDER_USAGE_LIMIT,
    MemoryEventQuery, MemoryEventStore, MemoryMessage, MemoryUsageSnapshotQuery,
    MemoryUsageSnapshotQueryResult, MemoryUsageSnapshotStore, MessageRole, OrgMemoryEventQuery,
    OrgPlanStore, OrgUserListQuery, OrgUserSummary, ProviderCallStatus, ProviderUsageAttribution,
    ProviderUsageAttributionSlot, ProviderUsageCapability, ProviderUsageDailyQuery,
    ProviderUsageEventRecord, ProviderUsagePersistedSummary, ProviderUsageQuery,
    ProviderUsageQueryResult, ProviderUsageStore, RetentionPruneResult, StoredBackgroundJobRun,
    StoredFactState, SummarizationInput, VectorRecord, VectorSearchQuery, VectorSearchResult,
    VectorStore, VectorWriteIntent, lock_until_from_ttl, sanitize_background_job_error_message,
    validate_background_job_run_limit, validate_event_date_range, validate_provider_usage_limit,
};
pub use privacy::PiiRedactor;
pub use query::{QueryAnalysis, QuestionType, TemporalConstraint, TemporalOrdinal, analyze};
pub use quota::{
    CheckMemoryWriteQuotaInput, CheckProviderQuotaInput, GetOrgQuotaStatusInput, OrgQuotaLimits,
    OrgQuotaUsage, QuotaCheckResult, QuotaLimitKind, QuotaLimitSource, QuotaService,
    QuotaViolation, ResolvedOrgQuotaLimits, utc_day_window,
};
pub use ranking::{
    MemoryRanker, RankedCandidate, RankingConfig, RankingSource, RrfConfig, apply_ranking, clamp01,
    freshness_score, memory_type_boost, reciprocal_rank_fusion, weighted_score_for,
};
pub use retention::{
    ApplyProviderUsageRetentionInput, ApplyProviderUsageRetentionOutput, ApplyRetentionInput,
    ApplyRetentionOutput, RetentionPolicy,
};
pub use shutdown::ShutdownToken;
