use std::sync::Arc;
use std::time::Duration;

use crate::metrics::MetricsExporter;
use crate::middleware::RateLimiter;
use crate::observability::Metrics;
use crate::provider_guardrails::create_provider_guardrails;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_config::{
    AuthMode, BackgroundJobLockBackend, ContextCacheBackend, DatabaseMigrationMode,
    EmbeddingProviderKind, EventBackend, FactBackend, LlmProviderKind, Settings, VectorBackend,
};
use memcore_core::{
    ApiKeyStore, BackgroundJob, BackgroundJobLockStore, BackgroundJobRetryPolicy,
    BackgroundJobRunStore, BackgroundJobRunner, ContextCacheConfig, EmbeddingProvider,
    EpisodeComponents, EpisodeStore, FactStore, InMemoryContextCache, LlmProvider, MemoryEngine,
    MemoryEventStore, MemoryRetentionJob, MemoryUsageSnapshotJob, MemoryUsageSnapshotStore,
    OrgPlanStore, OrgQuotaLimits, ProviderUsageAttributionSlot, ProviderUsageRetentionJob,
    ProviderUsageStore, ShutdownToken, StructuredExtractionConfig, VectorStore,
};
use memcore_providers::{
    CircuitBreakerConfig, MockEmbeddingProvider, MockLlmProvider, OpenAiClient,
    OpenAiEmbeddingProvider, OpenAiLlmProvider, OutputTokenLimits, PersistentProviderUsageRecorder,
    ProviderCallSource, ProviderCallSourceSlot, ProviderCandidate, ProviderCapability,
    ProviderCircuitBreaker, ProviderExecutionPolicy, ProviderGuardrailEnforcer, ProviderId,
    ProviderRoutingMetrics, ProviderUsageRecorder,
    build_resilient_embedding_from_candidates_with_guardrails,
    build_resilient_llm_from_candidates_with_guardrails, default_embedding_dimensions_for_model,
    new_token_usage_slot, provider_usage_recorder, validate_embedding_provider_name,
    validate_llm_provider_name, validate_provider_fallback_order,
    validate_summarizer_provider_name,
};
#[cfg(feature = "qdrant")]
use memcore_storage::QdrantVectorStore;
#[cfg(feature = "redis-cache")]
use memcore_storage::RedisContextCache;
use memcore_storage::SqliteProviderUsageStore;
use memcore_storage::{
    MockApiKeyStore, MockBackgroundJobLockStore, MockBackgroundJobRunStore, MockEpisodeStore,
    MockFactStore, MockMemoryEventStore, MockMemoryUsageSnapshotStore, MockOrgPlanStore,
    MockProviderUsageStore, MockVectorStore, SqliteApiKeyStore, SqliteBackgroundJobLockStore,
    SqliteBackgroundJobRunStore, SqliteEpisodeStore, SqliteFactStore, SqliteMemoryEventStore,
    SqliteMemoryUsageSnapshotStore, SqliteOrgPlanStore,
};
#[cfg(feature = "postgres")]
use memcore_storage::{
    PostgresApiKeyStore, PostgresBackgroundJobLockStore, PostgresBackgroundJobRunStore,
    PostgresEpisodeStore, PostgresFactStore, PostgresMemoryEventStore,
    PostgresMemoryUsageSnapshotStore, PostgresOrgPlanStore, PostgresProviderUsageStore,
};
use memcore_storage::{
    StorageMigrationMode, StorageStartupCheckReport, check_sqlite_startup, connect_sqlite_pool,
};
#[cfg(feature = "postgres")]
use memcore_storage::{check_postgres_startup, connect_postgres_pool};

/// Default embedding dimensions for the mock embedding provider.
const MOCK_EMBEDDING_DIMENSIONS: usize = 8;

struct ProviderRuntime {
    circuit_breaker: Arc<ProviderCircuitBreaker>,
    metrics: Arc<ProviderRoutingMetrics>,
    policy: ProviderExecutionPolicy,
}

/// Wires provider usage recording, optional persistence, and tenant attribution.
pub struct ProviderWiring {
    pub usage_recorder: Arc<dyn ProviderUsageRecorder>,
    pub usage_store: Option<Arc<dyn ProviderUsageStore>>,
    pub attribution_slot: Arc<ProviderUsageAttributionSlot>,
    pub guardrails: Arc<ProviderGuardrailEnforcer>,
    pub call_source_slot: Arc<ProviderCallSourceSlot>,
}

impl ProviderWiring {
    pub async fn from_settings(settings: &Settings) -> MemcoreResult<Self> {
        let inner = provider_usage_recorder(settings.provider_usage_metrics_enabled);
        let usage_store = if settings.provider_usage_persistence_enabled {
            Some(create_provider_usage_store(settings).await?)
        } else {
            None
        };
        let usage_recorder: Arc<dyn ProviderUsageRecorder> =
            if settings.provider_usage_persistence_enabled {
                PersistentProviderUsageRecorder::new(inner, usage_store.clone())
            } else {
                inner
            };
        let usage_recorder = crate::metrics::PrometheusProviderUsageRecorder::wrap(usage_recorder);
        let (guardrails, call_source_slot) = create_provider_guardrails(settings)?;

        Ok(Self {
            usage_recorder,
            usage_store,
            attribution_slot: Arc::new(ProviderUsageAttributionSlot::new()),
            guardrails,
            call_source_slot,
        })
    }

    pub fn for_tests(usage_recorder: Arc<dyn ProviderUsageRecorder>) -> Self {
        let settings = Settings::default();
        let (guardrails, call_source_slot) =
            create_provider_guardrails(&settings).expect("default provider guardrails");
        Self {
            usage_recorder,
            usage_store: None,
            attribution_slot: Arc::new(ProviderUsageAttributionSlot::new()),
            guardrails,
            call_source_slot,
        }
    }

    /// Synchronous wiring for mock-backend tests (no database I/O).
    pub fn for_mock_tests(settings: &Settings) -> Self {
        let inner = provider_usage_recorder(settings.provider_usage_metrics_enabled);
        let (guardrails, call_source_slot) =
            create_provider_guardrails(settings).expect("provider guardrails for mock tests");
        if settings.provider_usage_persistence_enabled {
            let store = Arc::new(MockProviderUsageStore::new());
            let usage_recorder = PersistentProviderUsageRecorder::new(inner, Some(store.clone()));
            Self {
                usage_recorder: crate::metrics::PrometheusProviderUsageRecorder::wrap(
                    usage_recorder,
                ),
                usage_store: Some(store),
                attribution_slot: Arc::new(ProviderUsageAttributionSlot::new()),
                guardrails,
                call_source_slot,
            }
        } else {
            Self {
                usage_recorder: crate::metrics::PrometheusProviderUsageRecorder::wrap(inner),
                usage_store: None,
                attribution_slot: Arc::new(ProviderUsageAttributionSlot::new()),
                guardrails,
                call_source_slot,
            }
        }
    }
}

async fn create_provider_usage_store(
    settings: &Settings,
) -> MemcoreResult<Arc<dyn ProviderUsageStore>> {
    if settings.fact_backend == FactBackend::Postgres {
        #[cfg(feature = "postgres")]
        {
            let url = require_postgres_url(settings)?;
            let store = PostgresProviderUsageStore::new(connect_postgres_pool(&url).await?);
            return Ok(Arc::new(store));
        }
        #[cfg(not(feature = "postgres"))]
        {
            return Err(MemcoreError::ValidationError(
                "provider usage persistence with postgres requires the `postgres` cargo feature"
                    .to_string(),
            ));
        }
    }

    if settings.fact_backend == FactBackend::Sqlite {
        let store = if is_sqlite_memory_database(settings) {
            SqliteProviderUsageStore::connect(&settings.database_url).await?
        } else {
            SqliteProviderUsageStore::new(connect_sqlite_pool(&settings.database_url).await?)
        };
        return Ok(Arc::new(store));
    }

    Ok(Arc::new(MockProviderUsageStore::new()))
}

async fn create_background_job_run_store(
    settings: &Settings,
) -> MemcoreResult<Option<Arc<dyn BackgroundJobRunStore>>> {
    if !settings.background_job_history_enabled {
        return Ok(None);
    }

    if settings.fact_backend == FactBackend::Postgres {
        #[cfg(feature = "postgres")]
        {
            let url = require_postgres_url(settings)?;
            let store = PostgresBackgroundJobRunStore::new(connect_postgres_pool(&url).await?);
            return Ok(Some(Arc::new(store)));
        }
        #[cfg(not(feature = "postgres"))]
        {
            return Err(MemcoreError::ValidationError(
                "background job history with postgres requires the `postgres` cargo feature"
                    .to_string(),
            ));
        }
    }

    if settings.fact_backend == FactBackend::Sqlite {
        let store = if is_sqlite_memory_database(settings) {
            SqliteBackgroundJobRunStore::connect(&settings.database_url).await?
        } else {
            SqliteBackgroundJobRunStore::new(connect_sqlite_pool(&settings.database_url).await?)
        };
        return Ok(Some(Arc::new(store)));
    }

    Ok(Some(Arc::new(MockBackgroundJobRunStore::new())))
}

async fn create_background_job_lock_store(
    settings: &Settings,
) -> MemcoreResult<Option<Arc<dyn BackgroundJobLockStore>>> {
    if !settings.background_job_lock_enabled {
        return Ok(None);
    }

    match settings.background_job_lock_backend {
        BackgroundJobLockBackend::Database => {}
    }

    if settings.fact_backend == FactBackend::Postgres {
        #[cfg(feature = "postgres")]
        {
            let url = require_postgres_url(settings)?;
            let store = PostgresBackgroundJobLockStore::new(connect_postgres_pool(&url).await?);
            return Ok(Some(Arc::new(store)));
        }
        #[cfg(not(feature = "postgres"))]
        {
            return Err(MemcoreError::ValidationError(
                "background job locks with postgres require the `postgres` cargo feature"
                    .to_string(),
            ));
        }
    }

    if settings.fact_backend == FactBackend::Sqlite {
        let store = if is_sqlite_memory_database(settings) {
            SqliteBackgroundJobLockStore::connect(&settings.database_url).await?
        } else {
            SqliteBackgroundJobLockStore::new(connect_sqlite_pool(&settings.database_url).await?)
        };
        return Ok(Some(Arc::new(store)));
    }

    Ok(Some(Arc::new(MockBackgroundJobLockStore::new())))
}

fn background_job_lock_owner_id(settings: &Settings) -> String {
    settings
        .background_job_lock_owner_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn provider_runtime(settings: &Settings) -> MemcoreResult<ProviderRuntime> {
    Ok(ProviderRuntime {
        circuit_breaker: Arc::new(ProviderCircuitBreaker::new(
            CircuitBreakerConfig::from_config(
                settings.provider_circuit_breaker_enabled,
                settings.provider_circuit_breaker_failure_threshold,
                settings.provider_circuit_breaker_reset_timeout_seconds,
                settings.provider_circuit_breaker_half_open_max_calls,
            )?,
        )),
        metrics: ProviderRoutingMetrics::new(),
        policy: provider_execution_policy(settings)?,
    })
}

fn llm_provider_kind_name(kind: &LlmProviderKind) -> &'static str {
    match kind {
        LlmProviderKind::Mock => "mock",
        LlmProviderKind::OpenAi => "openai",
        LlmProviderKind::OpenRouter => "openrouter",
        LlmProviderKind::Anthropic => "anthropic",
        LlmProviderKind::Groq => "groq",
    }
}

fn embedding_provider_kind_name(kind: &EmbeddingProviderKind) -> &'static str {
    match kind {
        EmbeddingProviderKind::Mock => "mock",
        EmbeddingProviderKind::OpenAi => "openai",
    }
}

fn llm_model_for_name(name: &str, settings: &Settings) -> String {
    match name {
        "mock" => settings.llm_model.clone(),
        "openai" => settings.llm_model.clone(),
        _ => settings.llm_model.clone(),
    }
}

fn embedding_model_for_name(name: &str, settings: &Settings) -> String {
    match name {
        "mock" => settings.embedding_model.clone(),
        "openai" => settings.embedding_model.clone(),
        _ => settings.embedding_model.clone(),
    }
}

/// Per-operation output-token limits, resolved from settings. Validation has already rejected any
/// explicit limit above `MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS`.
fn output_token_limits(settings: &Settings) -> OutputTokenLimits {
    let to_u32 = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    OutputTokenLimits {
        extraction: to_u32(settings.effective_extraction_max_output_tokens()),
        classification: to_u32(settings.effective_classification_max_output_tokens()),
    }
}

fn structured_extraction_policy(
    settings: &Settings,
) -> memcore_providers::StructuredExtractionPolicy {
    match settings.structured_extraction_policy {
        memcore_config::StructuredExtractionPolicy::Atomic => {
            memcore_providers::StructuredExtractionPolicy::Atomic
        }
        memcore_config::StructuredExtractionPolicy::Retentive => {
            memcore_providers::StructuredExtractionPolicy::Retentive
        }
    }
}

/// Reciprocal Rank Fusion: `MEMCORE_RRF_K` and the per-leg weights.
fn rrf_config(settings: &Settings) -> memcore_core::RrfConfig {
    memcore_core::RrfConfig {
        k: settings.rrf_k,
        semantic_weight: settings.rrf_weight_semantic,
        keyword_weight: settings.rrf_weight_keyword,
    }
}

/// The strategy for searches whose request names none (`MEMCORE_SEARCH_STRATEGY`).
/// The candidate-list transformations; all off unless their flags are set. The
/// rerank provider is `none` in this build (settings refuse any other value).
fn candidate_pipeline_config(settings: &Settings) -> memcore_core::CandidatePipelineConfig {
    memcore_core::CandidatePipelineConfig {
        query_analysis: settings.query_analysis_enabled,
        temporal_ranking: settings.temporal_ranking_enabled,
        temporal_boost: settings.temporal_boost,
        multi_query: match settings.multi_query {
            memcore_config::MultiQuerySetting::None => memcore_core::MultiQueryMode::None,
            memcore_config::MultiQuerySetting::Deterministic => {
                memcore_core::MultiQueryMode::Deterministic
            }
        },
        window: settings.rerank_candidates,
        rerank_timeout: std::time::Duration::from_millis(settings.rerank_timeout_ms),
        decomposition_timeout: std::time::Duration::from_millis(
            settings.query_decomposition_timeout_ms,
        ),
    }
}

type RerankProviderHandle = Arc<dyn memcore_core::candidates::rerank::RerankProvider>;

/// The configured reranker, or `None` (the default). `local` and `bedrock`
/// need a build with the `local-rerank` / `bedrock-rerank` feature; without it startup fails with a
/// clear message rather than silently not reranking.
async fn create_rerank_provider(
    settings: &Settings,
) -> MemcoreResult<Option<RerankProviderHandle>> {
    match settings.rerank_provider {
        memcore_config::RerankProviderSetting::None => Ok(None),
        memcore_config::RerankProviderSetting::Local => local_rerank_provider(settings),
        memcore_config::RerankProviderSetting::Bedrock => bedrock_rerank_provider(settings).await,
    }
}

#[cfg(feature = "local-rerank")]
fn local_rerank_provider(settings: &Settings) -> MemcoreResult<Option<RerankProviderHandle>> {
    use memcore_providers::rerank::local::{LocalRerankConfig, LocalRerankProvider};
    let required = |value: &Option<String>| value.clone().unwrap_or_default();
    let provider = LocalRerankProvider::load(&LocalRerankConfig {
        model_path: required(&settings.rerank_local_model_path).into(),
        tokenizer_path: required(&settings.rerank_local_tokenizer_path).into(),
        model_sha256: required(&settings.rerank_local_model_sha256),
        tokenizer_sha256: required(&settings.rerank_local_tokenizer_sha256),
        intra_threads: settings.rerank_local_threads,
        batch_size: settings.rerank_local_batch_size,
    })?;
    tracing::info!(manifest = ?provider.manifest(), "local reranker loaded");
    Ok(Some(Arc::new(provider)))
}

#[cfg(not(feature = "local-rerank"))]
fn local_rerank_provider(_settings: &Settings) -> MemcoreResult<Option<RerankProviderHandle>> {
    Err(MemcoreError::ValidationError(
        "MEMCORE_RERANK_PROVIDER=local needs a build with the `local-rerank` feature".to_string(),
    ))
}

#[cfg(feature = "bedrock-rerank")]
async fn bedrock_rerank_provider(
    settings: &Settings,
) -> MemcoreResult<Option<RerankProviderHandle>> {
    use memcore_providers::rerank::bedrock::{BedrockRerankConfig, BedrockRerankProvider};
    let provider = BedrockRerankProvider::new(&BedrockRerankConfig {
        region: settings.rerank_bedrock_region.clone(),
        model_arn: settings.rerank_bedrock_model_arn.clone(),
        profile: settings.rerank_bedrock_profile.clone(),
    })
    .await;
    Ok(Some(Arc::new(provider)))
}

#[cfg(not(feature = "bedrock-rerank"))]
async fn bedrock_rerank_provider(
    _settings: &Settings,
) -> MemcoreResult<Option<RerankProviderHandle>> {
    Err(MemcoreError::ValidationError(
        "MEMCORE_RERANK_PROVIDER=bedrock needs a build with the `bedrock-rerank` feature"
            .to_string(),
    ))
}

/// The model-free decomposition provider (`noop`). `llm` is wired separately by
/// [`llm_decomposition_identity`], because it uses the engine's own LLM provider.
fn decomposition_provider(
    settings: &Settings,
) -> Option<Arc<dyn memcore_core::candidates::decomposition::DecompositionProvider>> {
    match settings.query_decomposition {
        memcore_config::QueryDecompositionSetting::Noop => Some(Arc::new(
            memcore_core::candidates::decomposition::NoopDecompositionProvider,
        )),
        memcore_config::QueryDecompositionSetting::None
        | memcore_config::QueryDecompositionSetting::Llm => None,
    }
}

/// With `MEMCORE_QUERY_DECOMPOSITION=llm`, the configured LLM provider and model,
/// which enter the decomposition fingerprint.
fn llm_decomposition_identity(
    settings: &Settings,
) -> Option<memcore_core::candidates::decomposition::DecompositionIdentity> {
    (settings.query_decomposition == memcore_config::QueryDecompositionSetting::Llm).then(|| {
        memcore_core::candidates::decomposition::DecompositionIdentity {
            provider: llm_provider_kind_name(&settings.llm_provider).to_string(),
            model: settings.llm_model.clone(),
        }
    })
}

fn default_retrieval_mode(settings: &Settings) -> memcore_core::RetrievalMode {
    match settings.search_strategy {
        memcore_config::SearchStrategy::Hybrid => memcore_core::RetrievalMode::Hybrid,
        memcore_config::SearchStrategy::Semantic => memcore_core::RetrievalMode::Semantic,
        memcore_config::SearchStrategy::Fulltext => memcore_core::RetrievalMode::Fulltext,
        memcore_config::SearchStrategy::Rrf => memcore_core::RetrievalMode::Rrf,
    }
}

/// The structured extraction settings, or `None` when `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` is
/// off (the default), which keeps the legacy extraction request unchanged.
fn structured_extraction_config(settings: &Settings) -> Option<StructuredExtractionConfig> {
    settings
        .structured_extraction_enabled
        .then_some(StructuredExtractionConfig {
            window_turns: settings.extraction_window_turns,
            window_overlap: settings.extraction_window_overlap,
            concurrency: settings.extraction_concurrency,
        })
}

/// The canonical identifier of the configured embedding model, `<provider>/<model>`.
fn embedding_model_id(settings: &Settings) -> String {
    format!(
        "{}/{}",
        embedding_provider_kind_name(&settings.embedding_provider),
        settings.embedding_model
    )
}

/// The ingest journal's timing from settings.
fn ingest_journal_components(
    store: Arc<dyn memcore_core::ports::IngestJournalStore>,
    settings: &Settings,
) -> memcore_core::IngestJournalComponents {
    memcore_core::IngestJournalComponents {
        store,
        stale_after: std::time::Duration::from_secs(settings.ingest_recovery_stale_secs),
        inflight_wait: std::time::Duration::from_millis(settings.ingest_inflight_wait_ms),
        receipt_ttl: std::time::Duration::from_secs(settings.ingest_receipt_ttl_secs),
    }
}

/// (`MEMCORE_INGEST_JOURNAL_ENABLED`): the ingest journal in the fact database
/// (migration 0016); `None` when switched off.
async fn create_ingest_journal(
    settings: &Settings,
) -> MemcoreResult<Option<memcore_core::IngestJournalComponents>> {
    if !settings.ingest_journal_enabled {
        return Ok(None);
    }
    let store: Arc<dyn memcore_core::ports::IngestJournalStore> = match settings.fact_backend {
        FactBackend::Mock => Arc::new(memcore_storage::MockIngestJournalStore::new()),
        FactBackend::Sqlite => {
            if is_sqlite_memory_database(settings) {
                Arc::new(
                    memcore_storage::SqliteIngestJournalStore::connect(&settings.database_url)
                        .await?,
                )
            } else {
                Arc::new(memcore_storage::SqliteIngestJournalStore::new(
                    connect_sqlite_pool(&settings.database_url).await?,
                ))
            }
        }
        #[cfg(feature = "postgres")]
        FactBackend::Postgres => {
            let postgres_url = require_postgres_url(settings)?;
            Arc::new(memcore_storage::PostgresIngestJournalStore::new(
                connect_postgres_pool(&postgres_url).await?,
            ))
        }
        #[cfg(not(feature = "postgres"))]
        FactBackend::Postgres => {
            return Err(MemcoreError::ValidationError(
                "Postgres storage requires the `postgres` cargo feature".to_string(),
            ));
        }
    };
    Ok(Some(ingest_journal_components(store, settings)))
}

/// Rolls back abandoned ingestion operations once at startup (a previous process may
/// have stopped mid-request) and then every `interval`, until shutdown. Any number of processes
/// may run it: operations are claimed atomically.
fn spawn_ingest_recovery(
    engine: Arc<MemoryEngine>,
    interval: std::time::Duration,
    shutdown: ShutdownToken,
) {
    tokio::spawn(async move {
        loop {
            if let Err(error) = engine
                .recover_stale_ingest_operations(memcore_core::DEFAULT_INGEST_RECOVERY_BATCH)
                .await
            {
                tracing::warn!(
                    event = "ingest_recovery_sweep_failed",
                    error_code = error.code(),
                    "the ingest recovery sweep failed; it runs again at the next interval"
                );
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(interval) => {}
            }
        }
    });
}

/// Entity settings (`MEMCORE_ENTITIES_ENABLED` is checked by the caller).
fn entity_config(settings: &Settings) -> memcore_core::EntityConfig {
    memcore_core::EntityConfig {
        name_similarity_threshold: settings.entity_name_similarity_threshold,
        llm_confirmation: settings.entity_llm_confirmation_enabled,
        embedding_model: embedding_model_id(settings),
    }
}

/// Entity storage. With `MEMCORE_ENTITIES_ENABLED` the layer is on; with it off the same store is
/// still returned (the tables exist through migration 0014) for lifecycle operations only.
async fn create_entity_components(
    settings: &Settings,
) -> MemcoreResult<(
    Option<memcore_core::EntityComponents>,
    Option<Arc<dyn memcore_core::ports::EntityStore>>,
)> {
    let store: Arc<dyn memcore_core::ports::EntityStore> = match settings.fact_backend {
        FactBackend::Mock => Arc::new(memcore_storage::MockEntityStore::new()),
        FactBackend::Sqlite => {
            if is_sqlite_memory_database(settings) {
                Arc::new(memcore_storage::SqliteEntityStore::connect(&settings.database_url).await?)
            } else {
                Arc::new(memcore_storage::SqliteEntityStore::new(
                    connect_sqlite_pool(&settings.database_url).await?,
                ))
            }
        }
        #[cfg(feature = "postgres")]
        FactBackend::Postgres => {
            let postgres_url = require_postgres_url(settings)?;
            Arc::new(memcore_storage::PostgresEntityStore::new(
                connect_postgres_pool(&postgres_url).await?,
            ))
        }
        #[cfg(not(feature = "postgres"))]
        FactBackend::Postgres => {
            return Err(MemcoreError::ValidationError(
                "Postgres storage requires the `postgres` cargo feature".to_string(),
            ));
        }
    };
    Ok(if settings.entities_enabled {
        (
            Some(memcore_core::EntityComponents {
                store,
                config: entity_config(settings),
            }),
            None,
        )
    } else {
        (None, Some(store))
    })
}

/// With episodes off, the episode store and a lazily opened episode vector store, so
/// forget-user, retention, export and import reach persisted episodes. Nothing is created at
/// startup.
async fn create_episode_lifecycle(
    settings: &Settings,
    dimensions: usize,
    identity: Option<memcore_core::EmbeddingIdentity>,
    probe: Option<Arc<dyn EmbeddingProvider>>,
) -> MemcoreResult<Option<memcore_core::EpisodeLifecycle>> {
    #[cfg(not(feature = "qdrant"))]
    let _ = (&identity, &probe);
    if settings.episodes_enabled {
        return Ok(None);
    }
    let store: Arc<dyn EpisodeStore> = match settings.fact_backend {
        FactBackend::Mock => Arc::new(MockEpisodeStore::new()),
        FactBackend::Sqlite => {
            if is_sqlite_memory_database(settings) {
                Arc::new(SqliteEpisodeStore::connect(&settings.database_url).await?)
            } else {
                Arc::new(SqliteEpisodeStore::new(
                    connect_sqlite_pool(&settings.database_url).await?,
                ))
            }
        }
        #[cfg(feature = "postgres")]
        FactBackend::Postgres => {
            let postgres_url = require_postgres_url(settings)?;
            Arc::new(PostgresEpisodeStore::new(
                connect_postgres_pool(&postgres_url).await?,
            ))
        }
        #[cfg(not(feature = "postgres"))]
        FactBackend::Postgres => {
            return Err(MemcoreError::ValidationError(
                "Postgres storage requires the `postgres` cargo feature".to_string(),
            ));
        }
    };
    // The episode collection (Qdrant) or table (LanceDB) is opened only if it exists
    // and created only by the first write, i.e. an explicit import; starting memcore never creates
    // it.
    let vectors: Arc<dyn VectorStore> = match settings.vector_backend {
        VectorBackend::Mock => Arc::new(MockVectorStore::new()),
        VectorBackend::LanceDb => {
            // LanceDB support was removed. Unreachable in practice: the fact vector store refuses
            // this backend first.
            let _ = dimensions;
            Arc::new(MockVectorStore::new())
        }
        VectorBackend::Qdrant => {
            #[cfg(feature = "qdrant")]
            {
                let url = require_qdrant_url(settings)?;
                let collection = settings.qdrant_episode_collection.clone();
                let ensure_indexes = settings.qdrant_ensure_indexes;
                let write_mode = qdrant_write_mode(settings);
                // The episode store searches with the same query-time hnsw_ef.
                let search_hnsw_ef = settings.qdrant_search_hnsw_ef;
                // The same policy on episodes.
                let exact_max = settings.qdrant_exact_search_max_tenant_points;
                let (open_url, open_collection) = (url.clone(), collection.clone());
                let (open_identity, open_probe) = (identity.clone(), probe.clone());
                let (delete_url, delete_collection) = (url.clone(), collection.clone());
                Arc::new(
                    memcore_storage::LazyVectorStore::new(
                        move || {
                            let (url, collection) = (open_url.clone(), open_collection.clone());
                            let (identity, probe) = (open_identity.clone(), open_probe.clone());
                            Box::pin(async move {
                                let Some(store) = QdrantVectorStore::connect_existing(
                                    &url,
                                    &collection,
                                    dimensions,
                                )
                                .await?
                                else {
                                    return Ok(None);
                                };
                                // An existing episode store is verified like any other.
                                store
                                    .verify_identity(identity.as_ref(), probe.as_deref())
                                    .await?;
                                let store = store
                                    .with_write_mode(write_mode)
                                    .with_search_hnsw_ef(search_hnsw_ef)
                                    .with_exact_search_max_tenant_points(exact_max);
                                ensure_qdrant_indexes(
                                    &store,
                                    &collection,
                                    ensure_indexes,
                                    memcore_storage::EPISODE_PAYLOAD_INDEXES,
                                )
                                .await?;
                                Ok(Some(Arc::new(store) as Arc<dyn VectorStore>))
                            })
                        },
                        move || {
                            let (url, collection) = (url.clone(), collection.clone());
                            let (identity, probe) = (identity.clone(), probe.clone());
                            Box::pin(async move {
                                let store = QdrantVectorStore::connect_verified(
                                    &url,
                                    &collection,
                                    dimensions,
                                    identity.as_ref(),
                                    probe.as_deref(),
                                )
                                .await?
                                .with_write_mode(write_mode)
                                .with_search_hnsw_ef(search_hnsw_ef)
                                .with_exact_search_max_tenant_points(exact_max);
                                ensure_qdrant_indexes(
                                    &store,
                                    &collection,
                                    ensure_indexes,
                                    memcore_storage::EPISODE_PAYLOAD_INDEXES,
                                )
                                .await?;
                                Ok(Arc::new(store) as Arc<dyn VectorStore>)
                            })
                        },
                    )
                    // Forget-user and retention reach this configured collection
                    // even when its embedding identity no longer matches; tenant-filtered deletes only.
                    .with_deletion_opener(move || {
                        let (url, collection) = (delete_url.clone(), delete_collection.clone());
                        Box::pin(async move {
                            let Some(store) =
                                QdrantVectorStore::connect_existing(&url, &collection, dimensions)
                                    .await?
                            else {
                                return Ok(None);
                            };
                            // Keyword indexes keep the tenant-filtered deletes cheap; they
                            // change no vector.
                            ensure_qdrant_indexes(
                                &store,
                                &collection,
                                ensure_indexes,
                                memcore_storage::EPISODE_PAYLOAD_INDEXES,
                            )
                            .await?;
                            Ok(Some(Arc::new(store) as Arc<dyn VectorStore>))
                        })
                    }),
                )
            }
            #[cfg(not(feature = "qdrant"))]
            {
                // Unreachable in practice: the fact vector store refuses this backend first.
                let _ = dimensions;
                Arc::new(MockVectorStore::new())
            }
        }
    };
    Ok(Some(memcore_core::EpisodeLifecycle { store, vectors }))
}

/// (`MEMCORE_RELATED_FACT_MATCHER`, default `legacy`).
fn related_fact_matcher(settings: &Settings) -> memcore_core::RelatedFactMatcher {
    match settings.related_fact_matcher {
        memcore_config::RelatedFactMatcher::Legacy => memcore_core::RelatedFactMatcher::Legacy,
        memcore_config::RelatedFactMatcher::Semantic => memcore_core::RelatedFactMatcher::Semantic,
        memcore_config::RelatedFactMatcher::SemanticV23 => {
            memcore_core::RelatedFactMatcher::SemanticV23
        }
    }
}

/// (`MEMCORE_TEMPORAL_REWRITE`, default `legacy`).
fn temporal_rewrite(settings: &Settings) -> memcore_core::TemporalRewrite {
    match settings.temporal_rewrite {
        memcore_config::TemporalRewriteSetting::Legacy => memcore_core::TemporalRewrite::Legacy,
        memcore_config::TemporalRewriteSetting::Preserving => {
            memcore_core::TemporalRewrite::Preserving
        }
    }
}

/// (`MEMCORE_IMPORTANCE_POLICY`, default `legacy`).
fn importance_policy(settings: &Settings) -> memcore_core::ImportancePolicy {
    match settings.importance_policy {
        memcore_config::ImportancePolicySetting::Legacy => memcore_core::ImportancePolicy::Legacy,
        memcore_config::ImportancePolicySetting::Durable => memcore_core::ImportancePolicy::Durable,
    }
}

/// (`MEMCORE_CLASSIFICATION_PROMPT`, default `legacy`).
fn classification_prompt(settings: &Settings) -> memcore_providers::ClassificationPrompt {
    match settings.classification_prompt {
        memcore_config::ClassificationPromptSetting::Legacy => {
            memcore_providers::ClassificationPrompt::Legacy
        }
        memcore_config::ClassificationPromptSetting::OperationsV1 => {
            memcore_providers::ClassificationPrompt::OperationsV1
        }
    }
}

/// Consolidation v2 (`MEMCORE_CONSOLIDATION_MODE=llm_v2`); `None` keeps the legacy path.
fn consolidation_config(settings: &Settings) -> Option<memcore_core::ConsolidationConfig> {
    (settings.consolidation_mode == memcore_config::ConsolidationMode::LlmV2).then_some(
        memcore_core::ConsolidationConfig {
            candidates: settings.consolidation_candidates,
            similarity_floor: settings.consolidation_similarity_floor,
        },
    )
}

fn build_llm_provider_by_name(
    name: &str,
    settings: &Settings,
) -> MemcoreResult<Arc<dyn LlmProvider>> {
    match name {
        "mock" => Ok(Arc::new(MockLlmProvider::new())),
        "openai" => {
            // The extraction / chat role's own endpoint (falls back to OPENAI_*);
            // validated at startup (`Settings::validate`).
            let endpoint = settings.llm_endpoint();
            // Also enforced here for settings built without `Settings::validate` (fail closed).
            memcore_config::validate_provider_endpoint(
                "LLM",
                &endpoint,
                settings.providers.llm.allow_no_api_key,
                settings.providers.allow_insecure_http,
            )?;
            let client = OpenAiClient::with_optional_key_and_timeout(
                endpoint.api_key,
                &endpoint.base_url,
                Some(Duration::from_secs(settings.provider_timeout_seconds)),
            )
            .map_err(|err| provider_init_error("OpenAI LLM", err))?;
            Ok(Arc::new(
                OpenAiLlmProvider::for_base_url(client, settings.llm_model.clone())
                    .with_json_mode(match settings.providers.llm_json_mode {
                        memcore_config::LlmJsonMode::JsonObject => {
                            memcore_providers::ChatJsonMode::JsonObject
                        }
                        memcore_config::LlmJsonMode::JsonSchema => {
                            memcore_providers::ChatJsonMode::JsonSchema
                        }
                        memcore_config::LlmJsonMode::Instruction => {
                            memcore_providers::ChatJsonMode::Instruction
                        }
                    })
                    .with_output_token_limits(output_token_limits(settings))
                    .with_structured_extraction_policy(structured_extraction_policy(settings))
                    .with_classification_prompt(classification_prompt(settings)),
            ))
        }
        "openrouter" => Err(MemcoreError::ValidationError(
            "openrouter LLM provider is not wired into the API yet".to_string(),
        )),
        "anthropic" => Err(MemcoreError::ValidationError(
            "anthropic LLM provider is not wired into the API yet".to_string(),
        )),
        "groq" => Err(MemcoreError::ValidationError(
            "groq LLM provider is not wired into the API yet".to_string(),
        )),
        _ => Err(MemcoreError::ValidationError(format!(
            "unknown LLM provider in fallback order: {name}"
        ))),
    }
}

fn build_embedding_provider_by_name(
    name: &str,
    settings: &Settings,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    match name {
        "mock" => Ok(Arc::new(MockEmbeddingProvider::new(
            MOCK_EMBEDDING_DIMENSIONS,
        ))),
        "openai" => {
            // The embedding role's own endpoint and options (falls back to OPENAI_*).
            let endpoint = settings.embedding_endpoint();
            memcore_config::validate_provider_endpoint(
                "EMBEDDING",
                &endpoint,
                settings.providers.embedding.allow_no_api_key,
                settings.providers.allow_insecure_http,
            )?;
            let client = OpenAiClient::with_optional_key_and_timeout(
                endpoint.api_key,
                &endpoint.base_url,
                Some(Duration::from_secs(settings.provider_timeout_seconds)),
            )
            .map_err(|err| provider_init_error("OpenAI embedding", err))?;
            let options = &settings.providers.embedding_options;
            let dimensions = options.dimensions.unwrap_or_else(|| {
                default_embedding_dimensions_for_model(&settings.embedding_model)
            });
            let provider =
                OpenAiEmbeddingProvider::new(client, settings.embedding_model.clone(), dimensions)
                    .map_err(|err| provider_init_error("OpenAI embedding", err))?
                    .with_options(memcore_providers::EmbeddingOptions {
                        send_dimensions: options.send_dimensions,
                        normalize: options.normalize,
                        query_prefix: options.query_prefix.clone(),
                        document_prefix: options.document_prefix.clone(),
                        revision: options.model_revision.clone(),
                        serving_variant: options.serving_variant.clone(),
                        dedup_policy: embedding_dedup_policy(settings).1,
                    });
            Ok(Arc::new(provider))
        }
        _ => Err(MemcoreError::ValidationError(format!(
            "unknown embedding provider in fallback order: {name}"
        ))),
    }
}

fn llm_candidates_from_names(
    names: &[String],
    capability: ProviderCapability,
    settings: &Settings,
) -> MemcoreResult<Vec<ProviderCandidate<Arc<dyn LlmProvider>>>> {
    names
        .iter()
        .map(|name| {
            let usage_slot = new_token_usage_slot();
            Ok(ProviderCandidate::new(
                ProviderId::new(name.clone(), capability),
                build_llm_provider_by_name(name, settings)?,
                Some(llm_model_for_name(name, settings)),
                Some(usage_slot),
            ))
        })
        .collect()
}

fn embedding_candidates_from_names(
    names: &[String],
    settings: &Settings,
) -> MemcoreResult<Vec<ProviderCandidate<Arc<dyn EmbeddingProvider>>>> {
    names
        .iter()
        .map(|name| {
            let usage_slot = new_token_usage_slot();
            Ok(ProviderCandidate::new(
                ProviderId::new(name.clone(), ProviderCapability::Embedding),
                build_embedding_provider_by_name(name, settings)?,
                Some(embedding_model_for_name(name, settings)),
                Some(usage_slot),
            ))
        })
        .collect()
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub started_at: DateTime<Utc>,
    pub memory_engine: Arc<MemoryEngine>,
    pub api_key_store: Arc<dyn ApiKeyStore>,
    pub rate_limiter: Arc<RateLimiter>,
    pub metrics: Arc<Metrics>,
    pub metrics_exporter: MetricsExporter,
    pub provider_usage: Arc<dyn ProviderUsageRecorder>,
    pub provider_usage_store: Option<Arc<dyn ProviderUsageStore>>,
    pub provider_guardrails: Arc<ProviderGuardrailEnforcer>,
    pub call_source_slot: Arc<ProviderCallSourceSlot>,
    pub org_plan_store: Arc<dyn OrgPlanStore>,
    pub background_jobs: Arc<BackgroundJobRunner>,
    pub background_job_run_store: Option<Arc<dyn BackgroundJobRunStore>>,
    pub background_job_lock_store: Option<Arc<dyn BackgroundJobLockStore>>,
    pub background_job_lock_owner_id: Option<String>,
    pub shutdown_token: ShutdownToken,
    pub storage_startup: StorageStartupCheckReport,
    /// Live database check for `/health/ready` (PostgreSQL; none for SQLite / mock).
    pub database_readiness: memcore_storage::DatabaseReadinessProbe,
    /// Admission control; `None` when `MEMCORE_ADMISSION_CONTROL=off`.
    pub admission: Option<Arc<crate::admission::Admission>>,
    /// Runtime quality tier; refined by `run` after the startup checks.
    pub quality: crate::quality::QualityStatus,
}

impl AppState {
    /// Builds application state using configured storage and providers.
    pub async fn initialize(settings: Settings) -> MemcoreResult<Self> {
        Self::initialize_with_shutdown(settings, ShutdownToken::new()).await
    }

    /// Builds application state using a caller-provided shutdown token.
    pub async fn initialize_with_shutdown(
        settings: Settings,
        shutdown_token: ShutdownToken,
    ) -> MemcoreResult<Self> {
        let storage_startup = run_storage_startup_checks(&settings).await?;
        let database_readiness = database_readiness_probe(&settings)?;
        let wiring = ProviderWiring::from_settings(&settings).await?;
        let org_plan_store = create_org_plan_store(&settings).await?;
        let memory_usage_snapshot_store = create_memory_usage_snapshot_store(&settings).await?;
        let background_job_run_store = create_background_job_run_store(&settings).await?;
        let background_job_lock_store = create_background_job_lock_store(&settings).await?;
        let background_job_lock_owner_id = settings
            .background_job_lock_enabled
            .then(|| background_job_lock_owner_id(&settings));
        let memory_engine = Arc::new(
            create_memory_engine(
                &settings,
                &wiring,
                org_plan_store.clone(),
                memory_usage_snapshot_store,
            )
            .await?,
        );
        let background_jobs = Arc::new(create_background_job_runner(
            &settings,
            memory_engine.clone(),
            background_job_run_store.clone(),
            background_job_lock_store.clone(),
            background_job_lock_owner_id.clone(),
            shutdown_token.child_token(),
            wiring.call_source_slot.clone(),
        ));
        if settings.background_jobs_enabled {
            let runner = background_jobs.clone();
            tokio::spawn(async move {
                runner.run_forever().await;
            });
        }
        if memory_engine.ingest_journal_enabled() {
            spawn_ingest_recovery(
                memory_engine.clone(),
                std::time::Duration::from_secs(settings.ingest_recovery_interval_secs),
                shutdown_token.child_token(),
            );
        }
        let api_key_store = create_api_key_store(&settings).await?;
        Ok(Self {
            settings: settings.clone(),
            started_at: Utc::now(),
            admission: crate::admission::Admission::from_settings(&settings),
            memory_engine: memory_engine.clone(),
            api_key_store,
            rate_limiter: create_rate_limiter(&settings),
            metrics: Arc::new(Metrics::default()),
            metrics_exporter: MetricsExporter::from_settings(&settings),
            provider_usage: wiring.usage_recorder,
            provider_usage_store: wiring.usage_store,
            provider_guardrails: wiring.guardrails,
            call_source_slot: wiring.call_source_slot,
            org_plan_store,
            background_jobs,
            background_job_run_store,
            background_job_lock_store,
            background_job_lock_owner_id,
            shutdown_token,
            storage_startup,
            database_readiness,
            quality: crate::quality::QualityStatus::from_settings(&settings),
        })
    }

    /// Synchronous helper for tests when fact, event, and vector backends are all mock.
    ///
    /// For SQLite or Postgres backends, call [`Self::initialize`] from async code instead.
    pub fn new(settings: Settings) -> Self {
        if settings.fact_backend == FactBackend::Mock
            && settings.event_backend == EventBackend::Mock
            && settings.vector_backend == VectorBackend::Mock
        {
            let wiring = ProviderWiring::for_mock_tests(&settings);
            let org_plan_store = Arc::new(MockOrgPlanStore::new());
            let memory_usage_snapshot_store = Arc::new(MockMemoryUsageSnapshotStore::new());
            let background_job_run_store = if settings.background_job_history_enabled {
                Some(Arc::new(MockBackgroundJobRunStore::new()) as Arc<dyn BackgroundJobRunStore>)
            } else {
                None
            };
            let background_job_lock_store = if settings.background_job_lock_enabled {
                Some(Arc::new(MockBackgroundJobLockStore::new()) as Arc<dyn BackgroundJobLockStore>)
            } else {
                None
            };
            let background_job_lock_owner_id = settings
                .background_job_lock_enabled
                .then(|| background_job_lock_owner_id(&settings));
            let memory_engine = Arc::new(
                create_mock_memory_engine_with_wiring_and_org_plan_store(
                    &settings,
                    &wiring,
                    org_plan_store.clone(),
                    memory_usage_snapshot_store,
                )
                .expect("failed to create mock memory engine"),
            );
            let background_jobs = Arc::new(create_background_job_runner(
                &settings,
                memory_engine.clone(),
                background_job_run_store.clone(),
                background_job_lock_store.clone(),
                background_job_lock_owner_id.clone(),
                ShutdownToken::new(),
                wiring.call_source_slot.clone(),
            ));
            Self {
                started_at: Utc::now(),
                admission: crate::admission::Admission::from_settings(&settings),
                memory_engine,
                api_key_store: Arc::new(MockApiKeyStore::new()),
                settings: settings.clone(),
                rate_limiter: create_rate_limiter(&settings),
                metrics: Arc::new(Metrics::default()),
                metrics_exporter: MetricsExporter::from_settings(&settings),
                provider_usage: wiring.usage_recorder,
                provider_usage_store: wiring.usage_store,
                provider_guardrails: wiring.guardrails,
                call_source_slot: wiring.call_source_slot,
                org_plan_store,
                background_jobs,
                background_job_run_store,
                background_job_lock_store,
                background_job_lock_owner_id,
                shutdown_token: ShutdownToken::new(),
                storage_startup: StorageStartupCheckReport::ready_without_database(),
                database_readiness: memcore_storage::DatabaseReadinessProbe::none(),
                quality: crate::quality::QualityStatus::from_settings(&settings),
            }
        } else {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(Self::initialize(settings))
            })
            .expect("failed to initialize AppState")
        }
    }

    pub fn with_memory_engine(settings: Settings, memory_engine: Arc<MemoryEngine>) -> Self {
        Self {
            settings: settings.clone(),
            started_at: Utc::now(),
            admission: crate::admission::Admission::from_settings(&settings),
            memory_engine: memory_engine.clone(),
            api_key_store: Arc::new(MockApiKeyStore::new()),
            rate_limiter: create_rate_limiter(&settings),
            metrics: Arc::new(Metrics::default()),
            metrics_exporter: MetricsExporter::from_settings(&settings),
            provider_usage: provider_usage_recorder(settings.provider_usage_metrics_enabled),
            provider_usage_store: None,
            provider_guardrails: create_provider_guardrails(&settings)
                .expect("provider guardrails")
                .0,
            call_source_slot: create_provider_guardrails(&settings)
                .expect("provider guardrails")
                .1,
            org_plan_store: Arc::new(MockOrgPlanStore::new()),
            background_jobs: Arc::new(create_background_job_runner(
                &settings,
                memory_engine.clone(),
                None,
                None,
                None,
                ShutdownToken::new(),
                create_provider_guardrails(&settings)
                    .expect("provider guardrails")
                    .1,
            )),
            background_job_run_store: None,
            background_job_lock_store: None,
            background_job_lock_owner_id: None,
            shutdown_token: ShutdownToken::new(),
            storage_startup: StorageStartupCheckReport::ready_without_database(),
            database_readiness: memcore_storage::DatabaseReadinessProbe::none(),
            quality: crate::quality::QualityStatus::from_settings(&settings),
        }
    }

    pub fn with_memory_engine_and_provider_usage(
        settings: Settings,
        memory_engine: Arc<MemoryEngine>,
        provider_usage: Arc<dyn ProviderUsageRecorder>,
    ) -> Self {
        Self {
            settings: settings.clone(),
            started_at: Utc::now(),
            admission: crate::admission::Admission::from_settings(&settings),
            memory_engine: memory_engine.clone(),
            api_key_store: Arc::new(MockApiKeyStore::new()),
            rate_limiter: create_rate_limiter(&settings),
            metrics: Arc::new(Metrics::default()),
            metrics_exporter: MetricsExporter::from_settings(&settings),
            provider_usage,
            provider_usage_store: None,
            provider_guardrails: create_provider_guardrails(&settings)
                .expect("provider guardrails")
                .0,
            call_source_slot: create_provider_guardrails(&settings)
                .expect("provider guardrails")
                .1,
            org_plan_store: Arc::new(MockOrgPlanStore::new()),
            background_jobs: Arc::new(create_background_job_runner(
                &settings,
                memory_engine.clone(),
                None,
                None,
                None,
                ShutdownToken::new(),
                create_provider_guardrails(&settings)
                    .expect("provider guardrails")
                    .1,
            )),
            background_job_run_store: None,
            background_job_lock_store: None,
            background_job_lock_owner_id: None,
            shutdown_token: ShutdownToken::new(),
            storage_startup: StorageStartupCheckReport::ready_without_database(),
            database_readiness: memcore_storage::DatabaseReadinessProbe::none(),
            quality: crate::quality::QualityStatus::from_settings(&settings),
        }
    }

    pub fn with_memory_engine_provider_usage_and_store(
        settings: Settings,
        memory_engine: Arc<MemoryEngine>,
        provider_usage: Arc<dyn ProviderUsageRecorder>,
        provider_usage_store: Option<Arc<dyn ProviderUsageStore>>,
    ) -> Self {
        Self {
            settings: settings.clone(),
            started_at: Utc::now(),
            admission: crate::admission::Admission::from_settings(&settings),
            memory_engine: memory_engine.clone(),
            api_key_store: Arc::new(MockApiKeyStore::new()),
            rate_limiter: create_rate_limiter(&settings),
            metrics: Arc::new(Metrics::default()),
            metrics_exporter: MetricsExporter::from_settings(&settings),
            provider_usage,
            provider_usage_store,
            provider_guardrails: create_provider_guardrails(&settings)
                .expect("provider guardrails")
                .0,
            call_source_slot: create_provider_guardrails(&settings)
                .expect("provider guardrails")
                .1,
            org_plan_store: Arc::new(MockOrgPlanStore::new()),
            background_jobs: Arc::new(create_background_job_runner(
                &settings,
                memory_engine.clone(),
                None,
                None,
                None,
                ShutdownToken::new(),
                create_provider_guardrails(&settings)
                    .expect("provider guardrails")
                    .1,
            )),
            background_job_run_store: None,
            background_job_lock_store: None,
            background_job_lock_owner_id: None,
            shutdown_token: ShutdownToken::new(),
            storage_startup: StorageStartupCheckReport::ready_without_database(),
            database_readiness: memcore_storage::DatabaseReadinessProbe::none(),
            quality: crate::quality::QualityStatus::from_settings(&settings),
        }
    }
}

async fn run_storage_startup_checks(
    settings: &Settings,
) -> MemcoreResult<StorageStartupCheckReport> {
    let mode = storage_migration_mode(settings);
    let require_clean = settings.database_require_clean_migrations;
    let uses_postgres = settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres;
    let uses_sqlite = settings.fact_backend == FactBackend::Sqlite
        || settings.event_backend == EventBackend::Sqlite;

    tracing::info!(
        migration_mode = migration_mode_label(mode),
        require_clean_migrations = require_clean,
        uses_sqlite,
        uses_postgres,
        "running storage startup checks"
    );

    let postgres_report = if uses_postgres {
        #[cfg(feature = "postgres")]
        {
            let url = require_postgres_url(settings)?;
            Some(check_postgres_startup(&url, mode, require_clean).await?)
        }
        #[cfg(not(feature = "postgres"))]
        {
            if settings.event_backend == EventBackend::Postgres {
                return Err(MemcoreError::ValidationError(
                    "Postgres event backend requires the `postgres` cargo feature".to_string(),
                ));
            }
            return Err(MemcoreError::ValidationError(
                "Postgres fact backend requires the `postgres` cargo feature".to_string(),
            ));
        }
    } else {
        None
    };

    let sqlite_report = if uses_sqlite {
        Some(check_sqlite_startup(&settings.database_url, mode, require_clean).await?)
    } else {
        None
    };

    let report = combine_storage_startup_reports(postgres_report, sqlite_report);
    tracing::info!(
        database_connected = report.database_connected,
        migrations_clean = report.migrations_clean,
        pending_migrations = report
            .migration_report
            .as_ref()
            .map(|report| report.pending_count),
        warning_count = report.warnings.len(),
        "storage startup checks completed"
    );
    Ok(report)
}

fn combine_storage_startup_reports(
    first: Option<StorageStartupCheckReport>,
    second: Option<StorageStartupCheckReport>,
) -> StorageStartupCheckReport {
    match (first, second) {
        (None, None) => StorageStartupCheckReport::ready_without_database(),
        (Some(report), None) | (None, Some(report)) => report,
        (Some(first), Some(second)) => {
            let mut warnings = first.warnings;
            warnings.extend(second.warnings);
            StorageStartupCheckReport {
                database_connected: first.database_connected && second.database_connected,
                migrations_clean: first.migrations_clean && second.migrations_clean,
                migration_report: first.migration_report.or(second.migration_report),
                warnings,
            }
        }
    }
}

fn storage_migration_mode(settings: &Settings) -> StorageMigrationMode {
    if !settings.database_migrations_enabled {
        return StorageMigrationMode::Disabled;
    }

    match settings.database_migration_mode {
        DatabaseMigrationMode::Auto => StorageMigrationMode::Auto,
        DatabaseMigrationMode::ValidateOnly => StorageMigrationMode::ValidateOnly,
        DatabaseMigrationMode::Disabled => StorageMigrationMode::Disabled,
    }
}

fn migration_mode_label(mode: StorageMigrationMode) -> &'static str {
    match mode {
        StorageMigrationMode::Auto => "auto",
        StorageMigrationMode::ValidateOnly => "validate_only",
        StorageMigrationMode::Disabled => "disabled",
    }
}

fn is_sqlite_memory_database(settings: &Settings) -> bool {
    settings.database_url.contains(":memory:")
}

fn create_background_job_runner(
    settings: &Settings,
    memory_engine: Arc<MemoryEngine>,
    background_job_run_store: Option<Arc<dyn BackgroundJobRunStore>>,
    background_job_lock_store: Option<Arc<dyn BackgroundJobLockStore>>,
    background_job_lock_owner_id: Option<String>,
    shutdown_token: ShutdownToken,
    call_source_slot: Arc<ProviderCallSourceSlot>,
) -> BackgroundJobRunner {
    let org_ids = settings.background_job_org_ids.clone();
    let jobs: Vec<Arc<dyn BackgroundJob>> = vec![
        Arc::new(CallSourceTaggedJob::new(
            Arc::new(MemoryUsageSnapshotJob::new(
                memory_engine.clone(),
                settings.memory_usage_snapshot_job_enabled,
                Duration::from_secs(settings.memory_usage_snapshot_job_interval_seconds),
                org_ids.clone(),
            )),
            call_source_slot.clone(),
        )),
        Arc::new(CallSourceTaggedJob::new(
            Arc::new(ProviderUsageRetentionJob::new(
                memory_engine,
                settings.provider_usage_retention_job_enabled,
                Duration::from_secs(settings.provider_usage_retention_job_interval_seconds),
                org_ids,
                settings.provider_usage_retention_days,
            )),
            call_source_slot.clone(),
        )),
        Arc::new(CallSourceTaggedJob::new(
            Arc::new(MemoryRetentionJob::new(
                settings.memory_retention_job_enabled,
                Duration::from_secs(settings.memory_retention_job_interval_seconds),
            )),
            call_source_slot,
        )),
    ];

    BackgroundJobRunner::new(
        settings.background_jobs_enabled,
        Duration::from_secs(settings.background_job_runner_interval_seconds),
        jobs,
    )
    .with_history_store(
        settings.background_job_history_enabled,
        background_job_run_store,
    )
    .with_lock_store(
        settings.background_job_lock_enabled,
        background_job_lock_owner_id.unwrap_or_default(),
        Duration::from_secs(settings.background_job_lock_ttl_seconds),
        background_job_lock_store,
    )
    .with_retry_policy(background_job_retry_policy(settings))
    .with_shutdown(
        shutdown_token,
        Duration::from_secs(settings.background_job_shutdown_timeout_seconds),
    )
}

struct CallSourceTaggedJob {
    inner: Arc<dyn BackgroundJob>,
    slot: Arc<ProviderCallSourceSlot>,
}

impl CallSourceTaggedJob {
    fn new(inner: Arc<dyn BackgroundJob>, slot: Arc<ProviderCallSourceSlot>) -> Self {
        Self { inner, slot }
    }
}

#[async_trait::async_trait]
impl BackgroundJob for CallSourceTaggedJob {
    fn kind(&self) -> memcore_core::BackgroundJobKind {
        self.inner.kind()
    }

    fn interval(&self) -> Duration {
        self.inner.interval()
    }

    fn enabled(&self) -> bool {
        self.inner.enabled()
    }

    async fn run_once(&self) -> MemcoreResult<memcore_core::BackgroundJobRun> {
        self.slot.set(ProviderCallSource::BackgroundJob);
        let result = self.inner.run_once().await;
        self.slot.clear();
        result
    }
}

fn create_rate_limiter(settings: &Settings) -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(
        settings.rate_limit_enabled,
        settings.rate_limit_requests_per_minute,
    ))
}

fn org_quota_limits_from_settings(settings: &Settings) -> OrgQuotaLimits {
    OrgQuotaLimits::from_raw(
        settings.quotas_enabled,
        settings.max_users_per_org,
        settings.max_memories_per_user,
        settings.max_memories_per_org,
        settings.daily_provider_request_limit,
        settings.daily_provider_token_limit,
    )
}

/// Wires `MemoryEngine` from settings: configurable fact/vector stores and LLM/embedding providers.
pub async fn create_memory_engine(
    settings: &Settings,
    wiring: &ProviderWiring,
    org_plan_store: Arc<dyn OrgPlanStore>,
    memory_usage_snapshot_store: Arc<dyn MemoryUsageSnapshotStore>,
) -> MemcoreResult<MemoryEngine> {
    let (fact_store, event_store) = create_storage(settings).await?;
    let llm_provider = create_llm_provider(
        settings,
        wiring.usage_recorder.clone(),
        wiring.attribution_slot.clone(),
        wiring.guardrails.clone(),
        wiring.call_source_slot.clone(),
    )?;
    let embedding_provider = create_embedding_provider(
        settings,
        wiring.usage_recorder.clone(),
        wiring.attribution_slot.clone(),
        wiring.guardrails.clone(),
        wiring.call_source_slot.clone(),
    )?;
    // The vector stores fail closed on a dimension or embedding-identity mismatch.
    let identity = embedding_provider.embedding_identity();
    // The configured embedder verifies every store before any vector is written
    // (fingerprint / legacy compatibility proof), unless MEMCORE_PROVIDER_PROBE=off.
    let probe = settings.providers.probe.then(|| embedding_provider.clone());
    let vector_store = create_vector_store(
        settings,
        embedding_provider.dimensions(),
        identity.as_ref(),
        probe.as_ref(),
    )
    .await?;
    let episodes = create_episode_components(
        settings,
        embedding_provider.dimensions(),
        identity.as_ref(),
        probe.as_ref(),
    )
    .await?;
    let (entities, entity_store) = create_entity_components(settings).await?;
    let episode_lifecycle = create_episode_lifecycle(
        settings,
        embedding_provider.dimensions(),
        identity.clone(),
        probe.clone(),
    )
    .await?;
    let rerank_provider = create_rerank_provider(settings).await?;
    let ingest_journal = create_ingest_journal(settings).await?;

    apply_context_cache_async(
        MemoryEngine::new(fact_store, vector_store, llm_provider, embedding_provider)
            .with_ingest_journal(ingest_journal)
            .with_embedding_dedup_config(embedding_dedup_policy(settings).0)
            .with_pii_redaction(settings.enable_pii_redaction)
            .with_classification_skip_when_no_related(settings.classification_skip_when_no_related)
            .with_related_fact_matcher(related_fact_matcher(settings))
            .with_temporal_rewrite(temporal_rewrite(settings))
            .with_importance_policy(importance_policy(settings))
            .with_episodes(episodes)
            .with_episode_lifecycle(episode_lifecycle)
            .with_entities(entities)
            .with_entity_store(entity_store)
            .with_structured_extraction(structured_extraction_config(settings))
            .with_consolidation(consolidation_config(settings))
            .with_rrf_config(rrf_config(settings))
            .with_default_retrieval_mode(default_retrieval_mode(settings))
            .with_candidate_pipeline(candidate_pipeline_config(settings))
            .with_rerank_provider(rerank_provider)
            .with_decomposition_provider(decomposition_provider(settings))
            .with_llm_decomposition(llm_decomposition_identity(settings))
            .with_search_max_limit(settings.search_max_limit)?
            .with_query_embedding_cache(settings.query_embedding_cache_capacity)
            .with_event_store(event_store)
            .with_usage_attribution(wiring.attribution_slot.clone())
            .with_provider_usage_store(wiring.usage_store.clone())
            .with_org_plan_store(org_plan_store)
            .with_memory_usage_snapshot_store(Some(memory_usage_snapshot_store))
            .with_global_quota_limits(org_quota_limits_from_settings(settings))
            .with_audit_provider_info(
                Some(llm_provider_name(settings)),
                Some(settings.llm_model.clone()),
            ),
        settings,
    )
    .await
}

async fn create_api_key_store(settings: &Settings) -> MemcoreResult<Arc<dyn ApiKeyStore>> {
    if settings.auth_mode == AuthMode::Dev {
        return Ok(Arc::new(MockApiKeyStore::new()));
    }

    #[cfg(feature = "postgres")]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        let postgres_url = require_postgres_url(settings)?;
        let store = PostgresApiKeyStore::new(connect_postgres_pool(&postgres_url).await?);
        return Ok(Arc::new(store));
    }

    #[cfg(not(feature = "postgres"))]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        return Err(MemcoreError::ValidationError(
            "database auth with postgres storage requires the `postgres` cargo feature".to_string(),
        ));
    }

    let store = if is_sqlite_memory_database(settings) {
        SqliteApiKeyStore::connect(&settings.database_url).await?
    } else {
        SqliteApiKeyStore::new(connect_sqlite_pool(&settings.database_url).await?)
    };
    Ok(Arc::new(store))
}

async fn create_org_plan_store(settings: &Settings) -> MemcoreResult<Arc<dyn OrgPlanStore>> {
    if settings.fact_backend == FactBackend::Mock && settings.event_backend == EventBackend::Mock {
        return Ok(Arc::new(MockOrgPlanStore::new()));
    }

    #[cfg(feature = "postgres")]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        let postgres_url = require_postgres_url(settings)?;
        let store = PostgresOrgPlanStore::new(connect_postgres_pool(&postgres_url).await?);
        return Ok(Arc::new(store));
    }

    #[cfg(not(feature = "postgres"))]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        if settings.event_backend == EventBackend::Postgres {
            return Err(MemcoreError::ValidationError(
                "Postgres event backend requires the `postgres` cargo feature".to_string(),
            ));
        }
        return Err(MemcoreError::ValidationError(
            "Postgres fact backend requires the `postgres` cargo feature".to_string(),
        ));
    }

    let store = if is_sqlite_memory_database(settings) {
        SqliteOrgPlanStore::connect(&settings.database_url).await?
    } else {
        SqliteOrgPlanStore::new(connect_sqlite_pool(&settings.database_url).await?)
    };
    Ok(Arc::new(store))
}

async fn create_memory_usage_snapshot_store(
    settings: &Settings,
) -> MemcoreResult<Arc<dyn MemoryUsageSnapshotStore>> {
    if settings.fact_backend == FactBackend::Mock && settings.event_backend == EventBackend::Mock {
        return Ok(Arc::new(MockMemoryUsageSnapshotStore::new()));
    }

    #[cfg(feature = "postgres")]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        let postgres_url = require_postgres_url(settings)?;
        let store =
            PostgresMemoryUsageSnapshotStore::new(connect_postgres_pool(&postgres_url).await?);
        return Ok(Arc::new(store));
    }

    #[cfg(not(feature = "postgres"))]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        return Err(MemcoreError::ValidationError(
            "Postgres storage requires the `postgres` cargo feature".to_string(),
        ));
    }

    let store = if is_sqlite_memory_database(settings) {
        SqliteMemoryUsageSnapshotStore::connect(&settings.database_url).await?
    } else {
        SqliteMemoryUsageSnapshotStore::new(connect_sqlite_pool(&settings.database_url).await?)
    };
    Ok(Arc::new(store))
}

async fn create_storage(
    settings: &Settings,
) -> MemcoreResult<(Arc<dyn FactStore>, Arc<dyn MemoryEventStore>)> {
    #[cfg(not(feature = "postgres"))]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        if settings.event_backend == EventBackend::Postgres {
            return Err(MemcoreError::ValidationError(
                "Postgres event backend requires the `postgres` cargo feature".to_string(),
            ));
        }
        return Err(MemcoreError::ValidationError(
            "Postgres fact backend requires the `postgres` cargo feature".to_string(),
        ));
    }

    match (&settings.fact_backend, &settings.event_backend) {
        (FactBackend::Mock, EventBackend::Mock) => Ok((
            Arc::new(MockFactStore::new()),
            Arc::new(MockMemoryEventStore::new()),
        )),
        (FactBackend::Sqlite, EventBackend::Sqlite) => {
            let fact_store = if is_sqlite_memory_database(settings) {
                SqliteFactStore::connect(&settings.database_url).await?
            } else {
                SqliteFactStore::new(connect_sqlite_pool(&settings.database_url).await?)
            };
            let event_store = SqliteMemoryEventStore::new(fact_store.pool());
            Ok((Arc::new(fact_store), Arc::new(event_store)))
        }
        (FactBackend::Sqlite, EventBackend::Mock) => {
            let fact_store = if is_sqlite_memory_database(settings) {
                SqliteFactStore::connect(&settings.database_url).await?
            } else {
                SqliteFactStore::new(connect_sqlite_pool(&settings.database_url).await?)
            };
            Ok((Arc::new(fact_store), Arc::new(MockMemoryEventStore::new())))
        }
        (FactBackend::Mock, EventBackend::Sqlite) => {
            let event_store = if is_sqlite_memory_database(settings) {
                SqliteMemoryEventStore::connect(&settings.database_url).await?
            } else {
                SqliteMemoryEventStore::new(connect_sqlite_pool(&settings.database_url).await?)
            };
            Ok((Arc::new(MockFactStore::new()), Arc::new(event_store)))
        }
        #[cfg(feature = "postgres")]
        (FactBackend::Postgres, EventBackend::Postgres) => {
            let postgres_url = require_postgres_url(settings)?;
            let fact_store = PostgresFactStore::new(connect_postgres_pool(&postgres_url).await?);
            let event_store = PostgresMemoryEventStore::new(fact_store.pool());
            Ok((Arc::new(fact_store), Arc::new(event_store)))
        }
        #[cfg(feature = "postgres")]
        (FactBackend::Postgres, EventBackend::Mock) => {
            let postgres_url = require_postgres_url(settings)?;
            let fact_store = PostgresFactStore::new(connect_postgres_pool(&postgres_url).await?);
            Ok((Arc::new(fact_store), Arc::new(MockMemoryEventStore::new())))
        }
        #[cfg(feature = "postgres")]
        (FactBackend::Mock, EventBackend::Postgres) => {
            let postgres_url = require_postgres_url(settings)?;
            let event_store =
                PostgresMemoryEventStore::new(connect_postgres_pool(&postgres_url).await?);
            Ok((Arc::new(MockFactStore::new()), Arc::new(event_store)))
        }
        #[cfg(feature = "postgres")]
        (FactBackend::Sqlite, EventBackend::Postgres) => {
            let fact_store = if is_sqlite_memory_database(settings) {
                SqliteFactStore::connect(&settings.database_url).await?
            } else {
                SqliteFactStore::new(connect_sqlite_pool(&settings.database_url).await?)
            };
            let postgres_url = require_postgres_url(settings)?;
            let event_store =
                PostgresMemoryEventStore::new(connect_postgres_pool(&postgres_url).await?);
            Ok((Arc::new(fact_store), Arc::new(event_store)))
        }
        #[cfg(feature = "postgres")]
        (FactBackend::Postgres, EventBackend::Sqlite) => {
            let postgres_url = require_postgres_url(settings)?;
            let fact_store = PostgresFactStore::new(connect_postgres_pool(&postgres_url).await?);
            let event_store = if is_sqlite_memory_database(settings) {
                SqliteMemoryEventStore::connect(&settings.database_url).await?
            } else {
                SqliteMemoryEventStore::new(connect_sqlite_pool(&settings.database_url).await?)
            };
            Ok((Arc::new(fact_store), Arc::new(event_store)))
        }
        #[cfg(not(feature = "postgres"))]
        (FactBackend::Postgres, _) | (_, EventBackend::Postgres) => {
            Err(MemcoreError::ValidationError(
                "Postgres storage requires the `postgres` cargo feature".to_string(),
            ))
        }
    }
}

/// A live PostgreSQL check when facts or events are on PostgreSQL; none otherwise.
fn database_readiness_probe(
    settings: &Settings,
) -> MemcoreResult<memcore_storage::DatabaseReadinessProbe> {
    #[cfg(feature = "postgres")]
    if settings.fact_backend == FactBackend::Postgres
        || settings.event_backend == EventBackend::Postgres
    {
        let url = require_postgres_url(settings)?;
        return memcore_storage::DatabaseReadinessProbe::postgres(
            &url,
            crate::startup::PROBE_TIMEOUT,
        );
    }
    let _ = settings;
    Ok(memcore_storage::DatabaseReadinessProbe::none())
}

#[cfg(feature = "postgres")]
fn require_postgres_url(settings: &Settings) -> MemcoreResult<String> {
    settings
        .postgres_url
        .as_ref()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            if settings.event_backend == EventBackend::Postgres
                && settings.fact_backend != FactBackend::Postgres
            {
                MemcoreError::ValidationError(
                    "MEMCORE_POSTGRES_URL is required when MEMCORE_EVENT_BACKEND=postgres"
                        .to_string(),
                )
            } else {
                MemcoreError::ValidationError(
                    "MEMCORE_POSTGRES_URL is required when MEMCORE_FACT_BACKEND=postgres"
                        .to_string(),
                )
            }
        })
}

fn provider_execution_policy(settings: &Settings) -> MemcoreResult<ProviderExecutionPolicy> {
    let mut timeout_seconds = settings.provider_timeout_seconds;
    let mut max_retries = settings.provider_max_retries;
    if settings.provider_guardrails_enabled {
        // Precedence: when guardrails are enabled, use the stricter (min) caps.
        timeout_seconds = timeout_seconds.min(settings.provider_timeout_seconds);
        max_retries = max_retries.min(settings.provider_max_retries_per_call);
    }
    ProviderExecutionPolicy::from_config(
        timeout_seconds,
        max_retries,
        settings.provider_initial_backoff_ms,
        settings.provider_max_backoff_ms,
        settings.provider_backoff_multiplier,
        settings.provider_retry_jitter_enabled,
    )
}

fn background_job_retry_policy(settings: &Settings) -> BackgroundJobRetryPolicy {
    BackgroundJobRetryPolicy {
        enabled: settings.background_job_retries_enabled,
        max_retries: settings.background_job_max_retries,
        initial_backoff: Duration::from_millis(settings.background_job_initial_backoff_ms),
        max_backoff: Duration::from_millis(settings.background_job_max_backoff_ms),
        backoff_multiplier: settings.background_job_backoff_multiplier,
        jitter_enabled: settings.background_job_retry_jitter_enabled,
    }
}

/// The LLM provider wired exactly as the server wires it (resilient router, retry policy,
/// guardrails, output-token limits), for offline tools such as the Step 6.5 annotation harness.
/// Provider usage is recorded in memory only.
pub fn create_llm_provider_for_tool(settings: &Settings) -> MemcoreResult<Arc<dyn LlmProvider>> {
    let (guardrails, call_source_slot) = create_provider_guardrails(settings)?;
    create_llm_provider(
        settings,
        provider_usage_recorder(settings.provider_usage_metrics_enabled),
        Arc::new(ProviderUsageAttributionSlot::new()),
        guardrails,
        call_source_slot,
    )
}

fn create_unbounded_llm_provider(
    settings: &Settings,
    usage_recorder: Arc<dyn ProviderUsageRecorder>,
    attribution_slot: Arc<ProviderUsageAttributionSlot>,
    guardrails: Arc<ProviderGuardrailEnforcer>,
    call_source_slot: Arc<ProviderCallSourceSlot>,
) -> MemcoreResult<Arc<dyn LlmProvider>> {
    let runtime = provider_runtime(settings)?;
    let llm_names = if settings.provider_fallback_enabled {
        settings.llm_fallback_order.clone()
    } else {
        vec![llm_provider_kind_name(&settings.llm_provider).to_string()]
    };
    let summarizer_names = if settings.provider_fallback_enabled {
        settings.summarizer_fallback_order.clone()
    } else {
        vec![llm_provider_kind_name(&settings.llm_provider).to_string()]
    };

    if settings.provider_fallback_enabled {
        validate_provider_fallback_order(&llm_names, validate_llm_provider_name)?;
        validate_provider_fallback_order(&summarizer_names, validate_summarizer_provider_name)?;
    } else if matches!(
        settings.llm_provider,
        LlmProviderKind::OpenRouter | LlmProviderKind::Anthropic | LlmProviderKind::Groq
    ) {
        return build_llm_provider_by_name(
            llm_provider_kind_name(&settings.llm_provider),
            settings,
        );
    }

    let providers = llm_candidates_from_names(&llm_names, ProviderCapability::Llm, settings)?;
    let summarizer_providers = llm_candidates_from_names(
        &summarizer_names,
        ProviderCapability::Summarization,
        settings,
    )?;

    Ok(build_resilient_llm_from_candidates_with_guardrails(
        providers,
        summarizer_providers,
        runtime.circuit_breaker,
        runtime.policy,
        settings.provider_fallback_enabled,
        Some(runtime.metrics),
        Some(usage_recorder),
        Some(attribution_slot),
        settings.provider_cost_tracking_enabled,
        Some(guardrails),
        Some(call_source_slot),
    ))
}

fn create_unbounded_embedding_provider(
    settings: &Settings,
    usage_recorder: Arc<dyn ProviderUsageRecorder>,
    attribution_slot: Arc<ProviderUsageAttributionSlot>,
    guardrails: Arc<ProviderGuardrailEnforcer>,
    call_source_slot: Arc<ProviderCallSourceSlot>,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    let runtime = provider_runtime(settings)?;
    let names = if settings.provider_fallback_enabled {
        settings.embedding_fallback_order.clone()
    } else {
        vec![embedding_provider_kind_name(&settings.embedding_provider).to_string()]
    };

    if settings.provider_fallback_enabled {
        validate_provider_fallback_order(&names, validate_embedding_provider_name)?;
    }

    let providers = embedding_candidates_from_names(&names, settings)?;

    build_resilient_embedding_from_candidates_with_guardrails(
        providers,
        runtime.circuit_breaker,
        runtime.policy,
        settings.provider_fallback_enabled,
        Some(runtime.metrics),
        Some(usage_recorder),
        Some(attribution_slot),
        settings.provider_cost_tracking_enabled,
        Some(guardrails),
        Some(call_source_slot),
    )
}

/// The configured LLM provider, bounded by the process-wide chat permits unless
/// admission control is off.
fn create_llm_provider(
    settings: &Settings,
    usage_recorder: Arc<dyn ProviderUsageRecorder>,
    attribution_slot: Arc<ProviderUsageAttributionSlot>,
    guardrails: Arc<ProviderGuardrailEnforcer>,
    call_source_slot: Arc<ProviderCallSourceSlot>,
) -> MemcoreResult<Arc<dyn LlmProvider>> {
    let provider = create_unbounded_llm_provider(
        settings,
        usage_recorder,
        attribution_slot,
        guardrails,
        call_source_slot,
    )?;
    Ok(match provider_bounds(settings) {
        Some(bounds) => crate::admission::BoundedLlmProvider::wrap(provider, bounds.chat),
        None => provider,
    })
}

/// The configured embedding provider, bounded by the process-wide embedding permits
/// unless admission control is off.
/// Only for `memcore-maintenance verify-vectors --repair`: the configured
/// embedding provider with the API's usage accounting and guardrails. Refused when embedding
/// fallback is configured, because a fallback could embed with another model and mix embedding
/// spaces in one collection.
pub async fn maintenance_embedding_provider(
    settings: &Settings,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    if settings.provider_fallback_enabled && settings.embedding_fallback_order.len() > 1 {
        return Err(MemcoreError::ValidationError(
            "maintenance refused: embedding provider fallback is configured; a repair must \
             embed with exactly one provider"
                .to_string(),
        ));
    }
    let wiring = ProviderWiring::from_settings(settings).await?;
    create_embedding_provider(
        settings,
        wiring.usage_recorder,
        wiring.attribution_slot,
        wiring.guardrails,
        wiring.call_source_slot,
    )
}

/// The configured embedding provider's identity and dimensions,
/// from the provider object alone: no network call, no embedding, no usage store.
pub fn configured_embedding_identity(
    settings: &Settings,
) -> MemcoreResult<(Option<memcore_core::EmbeddingIdentity>, usize)> {
    let provider = build_embedding_provider_by_name(
        embedding_provider_kind_name(&settings.embedding_provider),
        settings,
    )?;
    Ok((provider.embedding_identity(), provider.dimensions()))
}

fn create_embedding_provider(
    settings: &Settings,
    usage_recorder: Arc<dyn ProviderUsageRecorder>,
    attribution_slot: Arc<ProviderUsageAttributionSlot>,
    guardrails: Arc<ProviderGuardrailEnforcer>,
    call_source_slot: Arc<ProviderCallSourceSlot>,
) -> MemcoreResult<Arc<dyn EmbeddingProvider>> {
    let provider = create_unbounded_embedding_provider(
        settings,
        usage_recorder,
        attribution_slot,
        guardrails,
        call_source_slot,
    )?;
    Ok(match provider_bounds(settings) {
        Some(bounds) => {
            crate::admission::BoundedEmbeddingProvider::wrap(provider, bounds.embedding)
        }
        None => provider,
    })
}

/// One set of provider permits per process, shared by every engine built from these settings.
fn provider_bounds(settings: &Settings) -> Option<crate::admission::ProviderBounds> {
    static BOUNDS: std::sync::OnceLock<
        std::sync::Mutex<
            Vec<(
                memcore_config::AdmissionSettings,
                crate::admission::ProviderBounds,
            )>,
        >,
    > = std::sync::OnceLock::new();
    if !settings.admission.enabled {
        return None;
    }
    let cache = BOUNDS.get_or_init(Default::default);
    let mut cache = cache.lock().expect("provider bounds lock");
    if let Some((_, bounds)) = cache.iter().find(|(a, _)| *a == settings.admission) {
        return Some(bounds.clone());
    }
    let bounds = crate::admission::ProviderBounds::from_settings(settings)?;
    cache.push((settings.admission.clone(), bounds.clone()));
    Some(bounds)
}

fn provider_init_error(provider: &str, err: MemcoreError) -> MemcoreError {
    MemcoreError::ValidationError(format!("failed to initialize {provider} provider: {err}"))
}

/// Episode storage (`MEMCORE_EPISODES_ENABLED`); `None` when disabled, so nothing is created. The
/// episode vectors get their own collection (Qdrant) or table (LanceDB) with the fact store's
/// embedding dimensions and distance.
async fn create_episode_components(
    settings: &Settings,
    dimensions: usize,
    identity: Option<&memcore_core::EmbeddingIdentity>,
    probe: Option<&Arc<dyn EmbeddingProvider>>,
) -> MemcoreResult<Option<EpisodeComponents>> {
    if !settings.episodes_enabled {
        return Ok(None);
    }
    let store: Arc<dyn EpisodeStore> = match settings.fact_backend {
        FactBackend::Mock => Arc::new(MockEpisodeStore::new()),
        FactBackend::Sqlite => {
            if is_sqlite_memory_database(settings) {
                Arc::new(SqliteEpisodeStore::connect(&settings.database_url).await?)
            } else {
                Arc::new(SqliteEpisodeStore::new(
                    connect_sqlite_pool(&settings.database_url).await?,
                ))
            }
        }
        #[cfg(feature = "postgres")]
        FactBackend::Postgres => {
            let postgres_url = require_postgres_url(settings)?;
            Arc::new(PostgresEpisodeStore::new(
                connect_postgres_pool(&postgres_url).await?,
            ))
        }
        #[cfg(not(feature = "postgres"))]
        FactBackend::Postgres => {
            return Err(MemcoreError::ValidationError(
                "Postgres storage requires the `postgres` cargo feature".to_string(),
            ));
        }
    };
    let vectors = create_vector_store_named(
        settings,
        dimensions,
        identity,
        &settings.qdrant_episode_collection,
        &format!("{}_episodes", settings.lancedb_table),
        VectorCollectionKind::Episodes,
        probe,
    )
    .await?;
    Ok(Some(EpisodeComponents { store, vectors }))
}

async fn create_vector_store(
    settings: &Settings,
    dimensions: usize,
    identity: Option<&memcore_core::EmbeddingIdentity>,
    probe: Option<&Arc<dyn EmbeddingProvider>>,
) -> MemcoreResult<Arc<dyn VectorStore>> {
    create_vector_store_named(
        settings,
        dimensions,
        identity,
        &settings.qdrant_collection,
        &settings.lancedb_table,
        VectorCollectionKind::Facts,
        probe,
    )
    .await
}

/// The configured Qdrant write mode (`MEMCORE_QDRANT_WRITE_MODE`).
#[cfg(feature = "qdrant")]
fn qdrant_write_mode(settings: &Settings) -> memcore_storage::QdrantWriteMode {
    match settings.qdrant_write_mode {
        memcore_config::QdrantWriteModeSetting::Legacy => memcore_storage::QdrantWriteMode::Legacy,
        memcore_config::QdrantWriteModeSetting::Optimized => {
            memcore_storage::QdrantWriteMode::Optimized
        }
    }
}

/// Which payload indexes a Qdrant collection gets.
#[derive(Clone, Copy)]
enum VectorCollectionKind {
    Facts,
    Episodes,
}

/// Ensures the collection's keyword payload indexes unless `MEMCORE_QDRANT_ENSURE_INDEXES=false`
/// . Creates only missing indexes; a failure stops the open, so it is surfaced.
#[cfg(feature = "qdrant")]
async fn ensure_qdrant_indexes(
    store: &QdrantVectorStore,
    collection: &str,
    enabled: bool,
    fields: &[&str],
) -> MemcoreResult<()> {
    if !enabled {
        tracing::warn!(
            collection,
            "qdrant payload index check skipped (MEMCORE_QDRANT_ENSURE_INDEXES=false)"
        );
        return Ok(());
    }
    let report = store.ensure_payload_indexes(fields).await?;
    if !report.created.is_empty() {
        tracing::info!(
            collection,
            created = ?report.created,
            present = ?report.present,
            "created qdrant payload indexes"
        );
    }
    Ok(())
}

async fn create_vector_store_named(
    settings: &Settings,
    dimensions: usize,
    identity: Option<&memcore_core::EmbeddingIdentity>,
    qdrant_collection: &str,
    lancedb_table: &str,
    kind: VectorCollectionKind,
    probe: Option<&Arc<dyn EmbeddingProvider>>,
) -> MemcoreResult<Arc<dyn VectorStore>> {
    #[cfg(not(feature = "qdrant"))]
    let _ = (identity, kind, probe);
    match settings.vector_backend {
        VectorBackend::Mock => Ok(Arc::new(MockVectorStore::new())),
        VectorBackend::LanceDb => {
            // LanceDB support was removed. The refusal (and its message) is the one every build
            // without the former `lancedb` feature, including the release build, already returned.
            let _ = (dimensions, lancedb_table);
            Err(MemcoreError::ValidationError(
                "LanceDB vector backend requires the `lancedb` cargo feature".to_string(),
            ))
        }
        VectorBackend::Qdrant => {
            #[cfg(feature = "qdrant")]
            {
                let url = require_qdrant_url(settings)?;
                let store = QdrantVectorStore::connect_verified(
                    &url,
                    qdrant_collection,
                    dimensions,
                    identity,
                    probe.map(|p| p.as_ref() as &dyn EmbeddingProvider),
                )
                .await?
                .with_write_mode(qdrant_write_mode(settings))
                .with_search_hnsw_ef(settings.qdrant_search_hnsw_ef)
                .with_exact_search_max_tenant_points(
                    settings.qdrant_exact_search_max_tenant_points,
                );
                let fields = match kind {
                    VectorCollectionKind::Facts => memcore_storage::FACT_PAYLOAD_INDEXES,
                    VectorCollectionKind::Episodes => memcore_storage::EPISODE_PAYLOAD_INDEXES,
                };
                ensure_qdrant_indexes(
                    &store,
                    qdrant_collection,
                    settings.qdrant_ensure_indexes,
                    fields,
                )
                .await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "qdrant"))]
            {
                let _ = (dimensions, qdrant_collection);
                Err(MemcoreError::ValidationError(
                    "Qdrant vector backend requires the `qdrant` cargo feature".to_string(),
                ))
            }
        }
    }
}

/// The embedding-dedup policy and its identity label (D-L3): an explicit
/// `MEMCORE_EMBEDDING_DEDUP_THRESHOLD`; else Titan v2 keeps its validated cosine 0.92; else an
/// uncalibrated model gets exact-duplicate-only dedup (embedding similarity off; the exact
/// normalised-text check still runs). The mock provider (tests) keeps the engine default.
fn embedding_dedup_policy(
    settings: &Settings,
) -> (memcore_core::EmbeddingDeduplicationConfig, Option<String>) {
    let default = memcore_core::EmbeddingDeduplicationConfig::default();
    if settings.embedding_provider != memcore_config::EmbeddingProviderKind::OpenAi {
        return (default, None);
    }
    if let Some(threshold) = settings.providers.embedding_options.dedup_threshold {
        return (
            memcore_core::EmbeddingDeduplicationConfig {
                similarity_threshold: threshold,
                ..default
            },
            Some(format!("cosine_{threshold}_explicit")),
        );
    }
    let model = settings.embedding_model.to_ascii_lowercase();
    if model.contains("titan-embed-text-v2") || model.contains("titan-embed-v2") {
        return (default, Some("cosine_0.92_titan_v2_validated".to_string()));
    }
    (
        memcore_core::EmbeddingDeduplicationConfig {
            enabled: false,
            ..default
        },
        Some("exact_text_only_uncalibrated".to_string()),
    )
}

#[cfg(feature = "qdrant")]
fn require_qdrant_url(settings: &Settings) -> MemcoreResult<String> {
    let url = settings.qdrant_url.trim();
    if url.is_empty() {
        return Err(MemcoreError::ValidationError(
            "MEMCORE_QDRANT_URL is required when MEMCORE_VECTOR_BACKEND=qdrant".to_string(),
        ));
    }
    Ok(url.to_string())
}

/// In-memory mock fact and vector stores for fast API tests.
pub fn create_mock_memory_engine(settings: &Settings) -> MemcoreResult<MemoryEngine> {
    create_mock_memory_engine_with_wiring(settings, &ProviderWiring::for_mock_tests(settings))
}

pub fn create_mock_memory_engine_with_usage(
    settings: &Settings,
    usage_recorder: Arc<dyn ProviderUsageRecorder>,
) -> MemcoreResult<MemoryEngine> {
    create_mock_memory_engine_with_wiring(settings, &ProviderWiring::for_tests(usage_recorder))
}

pub fn create_mock_memory_engine_with_wiring(
    settings: &Settings,
    wiring: &ProviderWiring,
) -> MemcoreResult<MemoryEngine> {
    create_mock_memory_engine_with_wiring_and_org_plan_store(
        settings,
        wiring,
        Arc::new(MockOrgPlanStore::new()),
        Arc::new(MockMemoryUsageSnapshotStore::new()),
    )
}

pub fn create_mock_memory_engine_with_wiring_and_org_plan_store(
    settings: &Settings,
    wiring: &ProviderWiring,
    org_plan_store: Arc<dyn OrgPlanStore>,
    memory_usage_snapshot_store: Arc<dyn MemoryUsageSnapshotStore>,
) -> MemcoreResult<MemoryEngine> {
    apply_context_cache(
        MemoryEngine::new(
            Arc::new(MockFactStore::new()),
            Arc::new(MockVectorStore::new()),
            create_llm_provider(
                settings,
                wiring.usage_recorder.clone(),
                wiring.attribution_slot.clone(),
                wiring.guardrails.clone(),
                wiring.call_source_slot.clone(),
            )?,
            create_embedding_provider(
                settings,
                wiring.usage_recorder.clone(),
                wiring.attribution_slot.clone(),
                wiring.guardrails.clone(),
                wiring.call_source_slot.clone(),
            )?,
        )
        .with_pii_redaction(settings.enable_pii_redaction)
        .with_ingest_journal(settings.ingest_journal_enabled.then(|| {
            ingest_journal_components(
                Arc::new(memcore_storage::MockIngestJournalStore::new()),
                settings,
            )
        }))
        .with_classification_skip_when_no_related(settings.classification_skip_when_no_related)
        .with_related_fact_matcher(related_fact_matcher(settings))
        .with_temporal_rewrite(temporal_rewrite(settings))
        .with_importance_policy(importance_policy(settings))
        .with_episodes(settings.episodes_enabled.then(|| EpisodeComponents {
            store: Arc::new(MockEpisodeStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
        }))
        .with_entities(
            settings
                .entities_enabled
                .then(|| memcore_core::EntityComponents {
                    store: Arc::new(memcore_storage::MockEntityStore::new()),
                    config: entity_config(settings),
                }),
        )
        .with_entity_store((!settings.entities_enabled).then(|| {
            Arc::new(memcore_storage::MockEntityStore::new())
                as Arc<dyn memcore_core::ports::EntityStore>
        }))
        .with_episode_lifecycle((!settings.episodes_enabled).then(|| {
            memcore_core::EpisodeLifecycle {
                store: Arc::new(MockEpisodeStore::new()),
                vectors: Arc::new(MockVectorStore::new()),
            }
        }))
        .with_structured_extraction(structured_extraction_config(settings))
        .with_consolidation(consolidation_config(settings))
        .with_rrf_config(rrf_config(settings))
        .with_default_retrieval_mode(default_retrieval_mode(settings))
        .with_candidate_pipeline(candidate_pipeline_config(settings))
        .with_decomposition_provider(decomposition_provider(settings))
        .with_llm_decomposition(llm_decomposition_identity(settings))
        .with_search_max_limit(settings.search_max_limit)?
        .with_query_embedding_cache(settings.query_embedding_cache_capacity)
        .with_event_store(Arc::new(MockMemoryEventStore::new()))
        .with_usage_attribution(wiring.attribution_slot.clone())
        .with_provider_usage_store(wiring.usage_store.clone())
        .with_org_plan_store(org_plan_store)
        .with_memory_usage_snapshot_store(Some(memory_usage_snapshot_store))
        .with_global_quota_limits(org_quota_limits_from_settings(settings))
        .with_audit_provider_info(
            Some(llm_provider_name(settings)),
            Some(settings.llm_model.clone()),
        ),
        settings,
    )
}

fn context_cache_config_from_settings(settings: &Settings) -> ContextCacheConfig {
    ContextCacheConfig {
        enabled: settings.context_cache_enabled,
        ttl_seconds: settings.context_cache_ttl_seconds,
        max_entries: settings.context_cache_max_entries,
        stampede_protection_enabled: settings.context_cache_stampede_protection_enabled,
        stampede_lock_timeout_seconds: settings.context_cache_lock_timeout_seconds,
        stale_while_revalidate_enabled: settings.context_cache_stale_while_revalidate_enabled,
        stale_ttl_seconds: settings.context_cache_stale_ttl_seconds,
        metrics_enabled: settings.context_cache_metrics_enabled,
    }
}

fn apply_context_cache(engine: MemoryEngine, settings: &Settings) -> MemcoreResult<MemoryEngine> {
    // The startup policy again, for settings built in code (it already
    // ran in configuration validation for the environment path), before any backend exists.
    settings.check_context_cache_policy()?;
    if settings.context_cache_backend == ContextCacheBackend::Redis {
        return Err(MemcoreError::ValidationError(
            "Redis context cache backend requires async AppState::initialize".to_string(),
        ));
    }

    let config = context_cache_config_from_settings(settings);
    config.validate()?;

    Ok(engine.with_context_cache(
        Arc::new(InMemoryContextCache::new(
            settings.context_cache_max_entries,
        )),
        config,
    ))
}

async fn apply_context_cache_async(
    engine: MemoryEngine,
    settings: &Settings,
) -> MemcoreResult<MemoryEngine> {
    // Refuses `redis` here before any connection is attempted.
    settings.check_context_cache_policy()?;
    if settings.context_cache_backend != ContextCacheBackend::Redis {
        return apply_context_cache(engine, settings);
    }

    let config = context_cache_config_from_settings(settings);
    config.validate()?;

    #[cfg(feature = "redis-cache")]
    {
        let redis_url = require_redis_url(settings)?;
        let cache = RedisContextCache::connect(
            &redis_url,
            settings.context_cache_key_prefix.clone(),
            settings.context_cache_ttl_seconds,
        )
        .await?;
        return Ok(engine.with_context_cache(Arc::new(cache), config));
    }

    #[cfg(not(feature = "redis-cache"))]
    {
        Err(MemcoreError::ValidationError(
            "Redis context cache backend requires the `redis-cache` cargo feature".to_string(),
        ))
    }
}

#[cfg(feature = "redis-cache")]
fn require_redis_url(settings: &Settings) -> MemcoreResult<String> {
    settings
        .redis_url
        .as_ref()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            MemcoreError::ValidationError(
                "MEMCORE_REDIS_URL is required when MEMCORE_CONTEXT_CACHE_BACKEND=redis"
                    .to_string(),
            )
        })
}

fn llm_provider_name(settings: &Settings) -> String {
    match settings.llm_provider {
        LlmProviderKind::Mock => "mock".to_string(),
        LlmProviderKind::OpenAi => "openai".to_string(),
        LlmProviderKind::OpenRouter => "openrouter".to_string(),
        LlmProviderKind::Anthropic => "anthropic".to_string(),
        LlmProviderKind::Groq => "groq".to_string(),
    }
}

#[cfg(test)]
mod t19_tests {
    use super::*;

    fn openai_settings(model: &str) -> Settings {
        Settings {
            embedding_provider: memcore_config::EmbeddingProviderKind::OpenAi,
            embedding_model: model.to_string(),
            ..Settings::default()
        }
    }

    /// D-L3: Titan keeps 0.92; uncalibrated models get exact-duplicate-only dedup; an explicit
    /// threshold wins; the dedup policy label is part of the identity.
    #[test]
    fn embedding_dedup_policy_follows_d_l3() {
        let (titan, label) = embedding_dedup_policy(&openai_settings("bedrock-titan-embed-v2"));
        assert!(titan.enabled);
        assert_eq!(titan.similarity_threshold, 0.92);
        assert_eq!(label.as_deref(), Some("cosine_0.92_titan_v2_validated"));
        let (titan_arn, _) =
            embedding_dedup_policy(&openai_settings("amazon.titan-embed-text-v2:0"));
        assert!(titan_arn.enabled);

        for model in [
            "text-embedding-3-small",
            "bge-small-en-v1.5",
            "nomic-embed-text",
        ] {
            let (config, label) = embedding_dedup_policy(&openai_settings(model));
            assert!(!config.enabled, "{model}: uncalibrated -> exact text only");
            assert_eq!(label.as_deref(), Some("exact_text_only_uncalibrated"));
        }

        let mut explicit = openai_settings("bge-small-en-v1.5");
        explicit.providers.embedding_options.dedup_threshold = Some(0.95);
        let (config, label) = embedding_dedup_policy(&explicit);
        assert!(config.enabled);
        assert_eq!(config.similarity_threshold, 0.95);
        assert_eq!(label.as_deref(), Some("cosine_0.95_explicit"));

        let (mock, label) = embedding_dedup_policy(&Settings::default());
        assert_eq!(mock, memcore_core::EmbeddingDeduplicationConfig::default());
        assert_eq!(label, None);
    }
}
