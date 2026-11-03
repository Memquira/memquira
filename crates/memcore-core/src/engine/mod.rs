mod candidates;
mod complete_export;
mod entities;
mod ingest_journal;
mod query_embedding_cache;
mod structured;
mod types;

use std::sync::Arc;

use crate::observability::{
    counted_write, record_classification_skipped, record_stage, record_vector_hit_rejected,
    record_write_failure, timed,
};

use chrono::{Duration, Utc};
use memcore_common::MemcoreResult;

use crate::admin::{
    CreateMemoryUsageSnapshotInput, CreateMemoryUsageSnapshotOutput, DEFAULT_LIST_ORG_USERS_LIMIT,
    ListOrgUsersInput, ListOrgUsersOutput, MAX_LIST_ORG_USERS_LIMIT,
    MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT, MemoryUsageLatestSnapshot, MemoryUsageSnapshot,
    OrgMemoryUsageSummary, OrgSummaryInput, OrgSummaryOutput, OrgUsageDashboardInput,
    OrgUsageDashboardOutput, ProviderUsageDailyInput, ProviderUsageDailyOutput,
    QueryMemoryUsageSnapshotsInput, QueryMemoryUsageSnapshotsOutput, SearchOrgMemoryEventsInput,
    SearchOrgMemoryEventsOutput, empty_provider_usage_summary,
    validate_memory_usage_snapshot_limit,
};
use crate::audit::{
    build_add_event, build_consolidation_noop_event, build_delete_event, build_forget_user_event,
    build_import_replace_event, build_invalidate_event, build_noop_event, build_update_event,
    record_event_best_effort,
};
use crate::consolidation::{
    ConsolidationConfig, FastPath, fast_path, gather_related_facts, resolve_invalid_at,
    validate_decision,
};
use crate::context_cache_metrics_recorder;
use crate::dedup::{
    DeduplicationDecision, EmbeddingDeduplicationConfig, detect_duplicate,
    detect_embedding_duplicate, find_existing_facts_for_dedup, vector_hit_rejection,
};
use crate::export::{
    EXPORT_EPISODES_LIMIT, EXPORT_EVENTS_LIMIT, EXPORT_FACTS_LIMIT, UserMemoryExport,
};
use crate::import::{
    DUPLICATE_FACT_ID, ImportMode, ImportUserDataInput, ImportUserDataOutput,
    ImportValidationSummary, collect_import_validation, collect_truncation_mode_validation,
    derived_import_fact_id, resolve_import_fact_id,
};
use crate::importance::{ImportancePolicy, ImportanceScorer};
use crate::lifecycle::{
    LifecycleApplyResult, LifecycleContext, RelatedDecision, RelatedFactMatcher,
    apply_fact_operation_with_id, apply_invalidate, apply_update_with_content,
    checked_embedding_from, find_related_facts, find_related_facts_semantic,
    find_related_facts_semantic_v23, guard_related_decision, guard_related_decision_v23,
    link_superseded_by, vector_record_from_fact,
};
use crate::pagination::{PageCursor, build_page, parse_optional_cursor};
use crate::ports::{
    ConsolidationDecision, ConsolidationInput, ConsolidationOperation, EmbeddingProvider,
    FactClassificationInput, FactExtractionInput, FactFulltextQuery, FactSearchQuery, FactStore,
    LlmProvider, MemoryEventQuery, MemoryEventStore, MemoryUsageSnapshotQuery,
    MemoryUsageSnapshotStore, OrgMemoryEventQuery, OrgPlanStore, OrgUserListQuery,
    ProviderUsageAttributionSlot, ProviderUsageDailyQuery, ProviderUsageQuery, ProviderUsageStore,
    VectorRecord, VectorStore, VectorWriteIntent, validate_event_date_range,
};
use crate::ports::{DEFAULT_PROVIDER_USAGE_LIMIT, EntityStore, EpisodeStore, MemoryMessage};
use crate::privacy::{PiiRedactor, redact_messages_for_extraction};
use crate::quota::{
    CheckMemoryWriteQuotaInput, CheckProviderQuotaInput, GetOrgQuotaStatusInput, QuotaCheckResult,
    QuotaService, ResolvedOrgQuotaLimits,
};
use crate::ranking::{
    RankedCandidate, RankingConfig, RankingSource, RrfConfig, reciprocal_rank_fusion,
    weighted_score_for,
};
use crate::retention::{
    ApplyProviderUsageRetentionInput, ApplyProviderUsageRetentionOutput, ApplyRetentionInput,
    ApplyRetentionOutput,
};
use crate::{
    BuildContextInput, BuildContextOutput, ContextCache, ContextCacheConfig,
    ContextCacheMetricsRecorder, ContextCacheMetricsSnapshot, ContextCacheUsage, Episode, Fact,
    FactOperation, FactOperationDecision, MemoryEvent, MemorySearchResult, MemoryType,
    OrgQuotaLimits, SimpleTokenEstimator, TenantContext, apply_provider_compression_summary,
    assemble_context_with_budget, build_context_cache_key, cached_entry_from_output,
};
use crate::{ContextCacheCoordinator, DEFAULT_CONTEXT_CACHE_MAX_ENTRIES, InMemoryContextCache};
use uuid::Uuid;

pub use query_embedding_cache::{
    QUERY_EMBEDDING_FLIGHT_ABANDONED, QueryEmbeddingCache, QueryEmbeddingCacheStats,
};
pub use structured::{
    DEFAULT_EXTRACTION_CONCURRENCY, DEFAULT_EXTRACTION_WINDOW_OVERLAP,
    DEFAULT_EXTRACTION_WINDOW_TURNS, StructuredExtractionConfig, TemporalRewrite,
    extraction_windows,
};
/// What a context built with history puts before a memory that is no longer current.
pub const HISTORY_MARKER: &str = "[no longer current] ";

pub use types::{
    AddMemoryInput, AddMemoryOutput, CompleteExportInput, DEFAULT_LIST_MEMORIES_LIMIT,
    DEFAULT_LIST_MEMORY_EVENTS_LIMIT, DEFAULT_MIN_IMPORTANCE, DEFAULT_SEARCH_LIMIT,
    DeleteMemoryInput, DeleteMemoryOutput, EpisodeSearchResult, ExportUserDataInput,
    FactDiscardCounts, FactDiscardReason, ForgetUserInput, ForgetUserOutput, HistoricalResult,
    ListMemoriesInput, ListMemoriesOutput, ListMemoryEventsInput, ListMemoryEventsOutput,
    MAX_CONFIGURABLE_SEARCH_LIMIT, MAX_LIST_MEMORIES_LIMIT, MAX_LIST_MEMORY_EVENTS_LIMIT,
    MAX_SEARCH_LIMIT, MemoryOperationSummary, MemorySession, RetrievalMode, SearchEpisodesInput,
    SearchEpisodesOutput, SearchMemoryInput, SearchMemoryOutput, SearchOptions,
};

pub use entities::{
    DEFAULT_ENTITY_LIST_LIMIT, EntityComponents, EntityDetail, EntityFactView, EntityRelationView,
    ListEntitiesInput, ListEntitiesOutput, MAX_ENTITY_LIST_LIMIT,
};
use ingest_journal::JournalSession;
pub use ingest_journal::{
    DEFAULT_INGEST_INFLIGHT_WAIT, DEFAULT_INGEST_RECEIPT_TTL, DEFAULT_INGEST_RECOVERY_BATCH,
    DEFAULT_INGEST_RECOVERY_STALE_AFTER, IngestJournalComponents, IngestRecoveryReport,
    IngestRecoveryStatus,
};

#[derive(Clone)]
pub struct MemoryEngine {
    fact_store: Arc<dyn FactStore>,
    vector_store: Arc<dyn VectorStore>,
    llm_provider: Arc<dyn LlmProvider>,
    embedding_provider: Arc<dyn EmbeddingProvider>,
    event_store: Option<Arc<dyn MemoryEventStore>>,
    audit_provider_name: Option<String>,
    audit_model_name: Option<String>,
    min_importance: f32,
    enable_pii_redaction: bool,
    /// On the legacy ingest path, add a candidate without the
    /// classification call when its related-fact lookup returns no fact. `false` (the default)
    /// keeps the previous path, which calls the classifier for every candidate.
    classification_skip_when_no_related: bool,
    /// The legacy path's related-fact lookup and how its
    /// classifier decisions apply. `Legacy` (the default) is the previous behaviour.
    related_fact_matcher: RelatedFactMatcher,
    embedding_dedup_config: EmbeddingDeduplicationConfig,
    ranking_config: RankingConfig,
    rrf_config: RrfConfig,
    default_retrieval_mode: RetrievalMode,
    search_max_limit: usize,
    context_cache: Arc<dyn ContextCache>,
    context_cache_config: ContextCacheConfig,
    context_cache_coordinator: ContextCacheCoordinator,
    context_cache_metrics: Arc<dyn ContextCacheMetricsRecorder>,
    usage_attribution: Option<Arc<ProviderUsageAttributionSlot>>,
    provider_usage_store: Option<Arc<dyn ProviderUsageStore>>,
    org_plan_store: Option<Arc<dyn OrgPlanStore>>,
    memory_usage_snapshot_store: Option<Arc<dyn MemoryUsageSnapshotStore>>,
    global_quota_limits: OrgQuotaLimits,
    /// Structured extraction. `None` (the default) keeps the single legacy extraction call.
    structured_extraction: Option<StructuredExtractionConfig>,
    /// (`MEMCORE_TEMPORAL_REWRITE`): how structured extraction writes a dated fact's
    /// sentence. `Legacy` (the default) is the previous rewrite.
    temporal_rewrite: TemporalRewrite,
    /// (`MEMCORE_IMPORTANCE_POLICY`): which candidates pass the importance threshold.
    /// `Legacy` (the default) is `importance >= min_importance` only.
    importance_policy: ImportancePolicy,
    /// (`MEMCORE_EPISODES_ENABLED`). `None` (the default) stores no episodes.
    episodes: Option<EpisodeComponents>,
    /// Consolidation v2 (`MEMCORE_CONSOLIDATION_MODE=llm_v2`). `None` (the default) keeps
    /// the legacy consolidation path byte for byte, and search leaves nothing out.
    consolidation: Option<ConsolidationConfig>,
    /// Entity layer (`MEMCORE_ENTITIES_ENABLED`). `None` (the default) sends no annotation
    /// request and stores no entity rows.
    entities: Option<EntityComponents>,
    /// The entity store with the layer off, so lifecycle operations (forget, fact cleanup,
    /// retention, export, import) still reach entity data persisted while it was on.
    entity_store: Option<Arc<dyn EntityStore>>,
    /// Episode storage with episodes off, for lifecycle operations only (forget,
    /// retention, export, import).
    episode_lifecycle: Option<EpisodeLifecycle>,
    /// The candidate-list transformations; all off by default.
    candidate_pipeline: crate::candidates::CandidatePipelineConfig,
    /// The optional reranker for candidate lists; `None` (the default) never reranks.
    rerank_provider: Option<Arc<dyn crate::candidates::rerank::RerankProvider>>,
    /// The optional query decomposition, with its fingerprinted
    /// single-flight cache; `None` (the default) never decomposes.
    decomposition: Option<Arc<crate::candidates::decomposition::DecompositionService>>,
    /// (`MEMCORE_QUERY_EMBEDDING_CACHE_CAPACITY`): search-query embeddings by provider
    /// identity, organisation and exact query; `None` (the default) embeds every search query.
    query_embedding_cache: Option<Arc<QueryEmbeddingCache>>,
    /// The durable journal of in-flight add requests (rollback after an error, a
    /// cancellation or a crash). `None` keeps the V2 compensation of a failed request's adds.
    ingest_journal: Option<IngestJournalComponents>,
}

/// Persisted episode data that lifecycle operations (forget-user, retention, export,
/// import) must reach with `MEMCORE_EPISODES_ENABLED` off. `vectors` must never create its
/// collection just by being opened: the API wires a store that opens an existing collection and
/// creates one only on the first write (an explicit import).
#[derive(Clone)]
pub struct EpisodeLifecycle {
    pub store: Arc<dyn EpisodeStore>,
    pub vectors: Arc<dyn VectorStore>,
}

/// Episode storage: the relational store and a vector store of their own, separate from the fact
/// vectors so fact search can never return an episode.
#[derive(Clone)]
pub struct EpisodeComponents {
    pub store: Arc<dyn EpisodeStore>,
    pub vectors: Arc<dyn VectorStore>,
}

impl MemoryEngine {
    pub fn new(
        fact_store: Arc<dyn FactStore>,
        vector_store: Arc<dyn VectorStore>,
        llm_provider: Arc<dyn LlmProvider>,
        embedding_provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        let cache = Arc::new(InMemoryContextCache::new(DEFAULT_CONTEXT_CACHE_MAX_ENTRIES));
        let cache_config = ContextCacheConfig::default();
        let metrics = context_cache_metrics_recorder(&cache_config);
        let coordinator =
            ContextCacheCoordinator::new(cache.clone(), cache_config, metrics.clone());
        Self {
            fact_store,
            vector_store,
            llm_provider,
            embedding_provider,
            event_store: None,
            audit_provider_name: None,
            audit_model_name: None,
            min_importance: types::DEFAULT_MIN_IMPORTANCE,
            enable_pii_redaction: false,
            classification_skip_when_no_related: false,
            related_fact_matcher: RelatedFactMatcher::Legacy,
            temporal_rewrite: TemporalRewrite::Legacy,
            importance_policy: ImportancePolicy::Legacy,
            embedding_dedup_config: EmbeddingDeduplicationConfig::default(),
            ranking_config: RankingConfig::default(),
            rrf_config: RrfConfig::default(),
            default_retrieval_mode: RetrievalMode::default(),
            search_max_limit: types::MAX_SEARCH_LIMIT,
            context_cache: cache,
            context_cache_config: cache_config,
            context_cache_coordinator: coordinator,
            context_cache_metrics: metrics,
            usage_attribution: None,
            provider_usage_store: None,
            org_plan_store: None,
            memory_usage_snapshot_store: None,
            global_quota_limits: OrgQuotaLimits::disabled(),
            structured_extraction: None,
            episodes: None,
            consolidation: None,
            entities: None,
            entity_store: None,
            episode_lifecycle: None,
            candidate_pipeline: crate::candidates::CandidatePipelineConfig::default(),
            rerank_provider: None,
            decomposition: None,
            query_embedding_cache: None,
            ingest_journal: None,
        }
    }

    /// Single-flight cache of search-query embeddings, up to `capacity` entries;
    /// 0 (the default) keeps every search query embedded by the provider.
    pub fn with_query_embedding_cache(mut self, capacity: usize) -> Self {
        self.query_embedding_cache =
            (capacity > 0).then(|| Arc::new(QueryEmbeddingCache::new(capacity)));
        self
    }

    /// The embedding provider's runtime diagnostics (a local model's queue), if it has any.
    pub fn embedding_diagnostics(&self) -> Option<serde_json::Value> {
        self.embedding_provider.diagnostics()
    }

    pub fn query_embedding_cache_stats(&self) -> Option<QueryEmbeddingCacheStats> {
        self.query_embedding_cache
            .as_ref()
            .map(|cache| cache.stats())
    }

    /// A search query's embedding, through the query-embedding cache when it is on.
    async fn embed_search_query(
        &self,
        tenant: &TenantContext,
        query: &str,
    ) -> MemcoreResult<Vec<f32>> {
        match &self.query_embedding_cache {
            Some(cache) => {
                cache
                    .embed(self.embedding_provider.as_ref(), &tenant.org_id, query)
                    .await
            }
            None => self.embedding_provider.embed_query(query).await,
        }
    }

    /// An optional decomposition provider, run at most once per logical
    /// question through this engine's decomposition cache.
    pub fn with_decomposition_provider(
        mut self,
        provider: Option<Arc<dyn crate::candidates::decomposition::DecompositionProvider>>,
    ) -> Self {
        self.decomposition = provider.map(|provider| {
            Arc::new(crate::candidates::decomposition::DecompositionService::new(
                provider,
            ))
        });
        self
    }

    /// Decomposition by the frozen `decompose-v1` contract through this engine's own
    /// LLM provider. `None` leaves the current decomposition setting unchanged.
    pub fn with_llm_decomposition(
        self,
        identity: Option<crate::candidates::decomposition::DecompositionIdentity>,
    ) -> Self {
        match identity {
            Some(identity) => {
                let provider = crate::candidates::decomposition::LlmDecompositionProvider::new(
                    self.llm_provider.clone(),
                    identity,
                );
                self.with_decomposition_provider(Some(Arc::new(provider)))
            }
            None => self,
        }
    }

    /// The candidate-list transformations (query analysis, temporal ranking, multi-query).
    pub fn with_candidate_pipeline(
        mut self,
        config: crate::candidates::CandidatePipelineConfig,
    ) -> Self {
        self.candidate_pipeline = config;
        self
    }

    /// An optional reranker for candidate lists. `None` never reranks.
    pub fn with_rerank_provider(
        mut self,
        provider: Option<Arc<dyn crate::candidates::rerank::RerankProvider>>,
    ) -> Self {
        self.rerank_provider = provider;
        self
    }

    /// Enables consolidation v2 (`MEMCORE_CONSOLIDATION_MODE=llm_v2`). `None` keeps the legacy
    /// path. With v2 on, search and context leave out invalidated facts unless a request asks for
    /// history.
    pub fn with_consolidation(mut self, config: Option<ConsolidationConfig>) -> Self {
        self.consolidation = config;
        self
    }

    pub fn consolidation_config(&self) -> Option<ConsolidationConfig> {
        self.consolidation
    }

    /// Enables episodes (`MEMCORE_EPISODES_ENABLED`): every added message is stored, and embedded
    /// when long enough, before extraction runs.
    pub fn with_episodes(mut self, episodes: Option<EpisodeComponents>) -> Self {
        self.episodes = episodes;
        self
    }

    /// Journals every add request's fact operations so that a failed, cancelled or
    /// interrupted request is rolled back (see `ports::ingest_journal`).
    pub fn with_ingest_journal(mut self, journal: Option<IngestJournalComponents>) -> Self {
        self.ingest_journal = journal;
        self
    }

    pub fn ingest_journal_enabled(&self) -> bool {
        self.ingest_journal.is_some()
    }

    pub fn episodes_enabled(&self) -> bool {
        self.episodes.is_some()
    }

    /// Episode storage for lifecycle operations while episodes are off. Ignored when
    /// they are on (the enabled components are used).
    pub fn with_episode_lifecycle(mut self, lifecycle: Option<EpisodeLifecycle>) -> Self {
        self.episode_lifecycle = lifecycle;
        self
    }

    /// The episode storage lifecycle operations act on: the enabled components, else the
    /// lifecycle storage.
    fn episode_data(&self) -> Option<EpisodeComponents> {
        match (&self.episodes, &self.episode_lifecycle) {
            (Some(episodes), _) => Some(episodes.clone()),
            (None, Some(lifecycle)) => Some(EpisodeComponents {
                store: lifecycle.store.clone(),
                vectors: lifecycle.vectors.clone(),
            }),
            (None, None) => None,
        }
    }

    /// Deletes all of the user's episode rows and vectors, whether episodes are on or off.
    async fn delete_user_episode_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        if let Some(episodes) = self.episode_data() {
            episodes.store.delete_user_episodes(tenant).await?;
            episodes.vectors.delete_by_user(tenant).await?;
        }
        Ok(())
    }

    /// Enables structured extraction (`MEMCORE_STRUCTURED_EXTRACTION_ENABLED`). `None` leaves the
    /// legacy extraction path in place.
    pub fn with_structured_extraction(
        mut self,
        config: Option<StructuredExtractionConfig>,
    ) -> Self {
        self.structured_extraction = config;
        self
    }

    pub fn with_event_store(mut self, event_store: Arc<dyn MemoryEventStore>) -> Self {
        self.event_store = Some(event_store);
        self
    }

    pub fn with_org_plan_store(mut self, org_plan_store: Arc<dyn OrgPlanStore>) -> Self {
        self.org_plan_store = Some(org_plan_store);
        self
    }

    pub fn with_global_quota_limits(mut self, limits: OrgQuotaLimits) -> Self {
        self.global_quota_limits = limits;
        self
    }

    pub fn with_audit_provider_info(
        mut self,
        provider_name: Option<String>,
        model_name: Option<String>,
    ) -> Self {
        self.audit_provider_name = provider_name;
        self.audit_model_name = model_name;
        self
    }

    pub fn with_min_importance(mut self, min_importance: f32) -> Self {
        self.min_importance = min_importance;
        self
    }

    pub fn with_pii_redaction(mut self, enabled: bool) -> Self {
        self.enable_pii_redaction = enabled;
        self
    }

    /// `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED`.
    pub fn with_classification_skip_when_no_related(mut self, enabled: bool) -> Self {
        self.classification_skip_when_no_related = enabled;
        self
    }

    pub fn with_related_fact_matcher(mut self, matcher: RelatedFactMatcher) -> Self {
        self.related_fact_matcher = matcher;
        self
    }

    pub fn with_temporal_rewrite(mut self, rewrite: TemporalRewrite) -> Self {
        self.temporal_rewrite = rewrite;
        self
    }

    pub fn with_importance_policy(mut self, policy: ImportancePolicy) -> Self {
        self.importance_policy = policy;
        self
    }

    pub fn with_embedding_dedup_config(mut self, config: EmbeddingDeduplicationConfig) -> Self {
        self.embedding_dedup_config = config;
        self
    }

    pub fn with_ranking_config(mut self, config: RankingConfig) -> Self {
        self.ranking_config = config;
        self
    }

    pub fn with_rrf_config(mut self, config: RrfConfig) -> Self {
        self.rrf_config = config;
        self
    }

    /// The retrieval mode `search_memory` uses (`MEMCORE_SEARCH_STRATEGY`). Default `Hybrid`.
    pub fn with_default_retrieval_mode(mut self, mode: RetrievalMode) -> Self {
        self.default_retrieval_mode = mode;
        self
    }

    pub fn default_retrieval_mode(&self) -> RetrievalMode {
        self.default_retrieval_mode
    }

    /// The highest search `limit` accepted (`MEMCORE_SEARCH_MAX_LIMIT`). A value outside
    /// `MAX_SEARCH_LIMIT..=MAX_CONFIGURABLE_SEARCH_LIMIT` is rejected, never clamped.
    pub fn with_search_max_limit(mut self, max_limit: usize) -> MemcoreResult<Self> {
        if !(types::MAX_SEARCH_LIMIT..=types::MAX_CONFIGURABLE_SEARCH_LIMIT).contains(&max_limit) {
            return Err(memcore_common::MemcoreError::ValidationError(format!(
                "MEMCORE_SEARCH_MAX_LIMIT must be between {} and {}, got {max_limit}",
                types::MAX_SEARCH_LIMIT,
                types::MAX_CONFIGURABLE_SEARCH_LIMIT
            )));
        }
        self.search_max_limit = max_limit;
        Ok(self)
    }

    pub fn search_max_limit(&self) -> usize {
        self.search_max_limit
    }

    pub fn with_context_cache(
        mut self,
        cache: Arc<dyn ContextCache>,
        config: ContextCacheConfig,
    ) -> Self {
        let metrics = context_cache_metrics_recorder(&config);
        self.context_cache = cache.clone();
        self.context_cache_config = config;
        self.context_cache_metrics = metrics.clone();
        self.context_cache_coordinator = ContextCacheCoordinator::new(cache, config, metrics);
        self
    }

    pub fn context_cache_config(&self) -> ContextCacheConfig {
        self.context_cache_config
    }

    pub fn context_cache_metrics_snapshot(&self) -> ContextCacheMetricsSnapshot {
        self.context_cache_metrics.snapshot()
    }

    pub fn with_usage_attribution(mut self, slot: Arc<ProviderUsageAttributionSlot>) -> Self {
        self.usage_attribution = Some(slot);
        self
    }

    pub fn with_provider_usage_store(mut self, store: Option<Arc<dyn ProviderUsageStore>>) -> Self {
        self.provider_usage_store = store;
        self
    }

    pub fn with_memory_usage_snapshot_store(
        mut self,
        store: Option<Arc<dyn MemoryUsageSnapshotStore>>,
    ) -> Self {
        self.memory_usage_snapshot_store = store;
        self
    }

    fn bind_usage_attribution(&self, tenant: &TenantContext) {
        if let Some(slot) = &self.usage_attribution {
            slot.set(tenant.org_id.clone(), Some(tenant.user_id.clone()));
        }
    }

    pub fn pii_redaction_enabled(&self) -> bool {
        self.enable_pii_redaction
    }

    pub async fn add_memory(&self, input: AddMemoryInput) -> MemcoreResult<AddMemoryOutput> {
        validate_tenant(&input.tenant)?;
        validate_messages(&input.messages)?;
        self.bind_usage_attribution(&input.tenant);

        // closure: a request with a stable identity (every message has a client id)
        // that already succeeded is answered from its receipt (a retry after a lost response).
        let request_key = self
            .ingest_journal
            .as_ref()
            .and_then(|_| ingest_journal::ingest_request_key(&input));
        if let Some(key) = &request_key
            && let Some(output) = self.replay_ingest_receipt(&input.tenant, key).await?
        {
            return Ok(output);
        }

        // Episodes are stored before extraction, so a failed extraction still leaves the messages;
        // a retry with the same message IDs stores nothing twice. The extraction request below does
        // not depend on this step.
        if let Some(episodes) = &self.episodes {
            self.store_episodes(&input, episodes).await?;
        }

        let messages_for_extraction =
            messages_for_llm_extraction(&input.messages, self.enable_pii_redaction);
        // The entity annotation sees the same (redacted) messages as extraction; the extraction
        // request itself is unchanged.
        let messages_for_entities = self
            .entities
            .as_ref()
            .map(|_| messages_for_extraction.clone());

        let extract_started = std::time::Instant::now();
        let (extraction, temporal) = match self.structured_extraction {
            // Legacy path: byte-identical to Part A.
            None => (
                self.llm_provider
                    .extract_facts(FactExtractionInput {
                        tenant: input.tenant.clone(),
                        messages: messages_for_extraction,
                        metadata: input.metadata.clone(),
                        mode: Default::default(),
                    })
                    .await?,
                Default::default(),
            ),
            Some(config) => {
                let structured = self
                    .extract_structured(&input, messages_for_extraction, config)
                    .await?;
                (structured.output, structured.temporal)
            }
        };

        record_stage("ingest", "extract", extract_started);

        if extraction.skipped.total() > 0 || extraction.event_time_unparsed > 0 {
            tracing::warn!(
                event = "add_memory_extraction_losses",
                returned_by_model = extraction.returned_by_model(),
                usable = extraction.candidates.len(),
                unknown_memory_type = extraction.skipped.unknown_memory_type,
                invalid_candidate = extraction.skipped.invalid_candidate,
                event_time_unparsed = extraction.event_time_unparsed,
                "the model returned facts that extraction could not fully use"
            );
        }

        // `extracted` counts what the model actually returned, not what survived parsing, so the
        // summary's own invariant surfaces extraction losses instead of hiding them.
        let mut summary = MemoryOperationSummary {
            extracted: extraction.returned_by_model(),
            added: 0,
            updated: 0,
            deleted: 0,
            invalidated: 0,
            noop: 0,
            discarded: Default::default(),
            skipped: extraction.skipped,
            event_time_unparsed: extraction.event_time_unparsed,
            temporal,
            invalid_source_message_ids: extraction.invalid_source_message_ids,
        };
        let candidates = extraction.candidates;
        let mut memories = Vec::new();

        let lifecycle_ctx = LifecycleContext {
            fact_store: self.fact_store.as_ref(),
            vector_store: self.vector_store.as_ref(),
            embedding_provider: self.embedding_provider.as_ref(),
        };

        // With the ingest journal every fact operation below is recorded before it is
        // applied, and the request commits only after all of them completed (just before it
        // answers); an error, a cancellation (the session's drop) or a crash rolls them back.
        // Without a journal, a failure compensates the facts this request added (V2).
        let mut journal = JournalSession::new(self, &input.tenant, request_key);
        // The facts this request added, so that a later failure in the same
        // request can be compensated instead of leaving a partial write behind.
        let mut added_this_request: Vec<Fact> = Vec::new();
        let loop_result: MemcoreResult<()> = async {
        'candidates: for candidate in candidates {
            validate_candidate(&candidate)?;

            let candidate = ImportanceScorer::adjust(&candidate);

            if !self
                .importance_policy
                .passes(&candidate, self.min_importance)
            {
                summary.discard(FactDiscardReason::LowImportance);
                continue 'candidates;
            }

            // Consolidation v2 replaces everything below for this candidate; with it off
            // (the default) the legacy path runs unchanged.
            if let Some(config) = self.consolidation {
                self.consolidate_v2(
                    &input,
                    &candidate,
                    &config,
                    &lifecycle_ctx,
                    &mut journal,
                    &mut summary,
                    &mut memories,
                )
                .await?;
                continue 'candidates;
            }

            // A candidate that would rely on another request's uncommitted fact (a
            // duplicate of it, or its update / supersession) waits for that request to commit or
            // roll back, then is processed again from here.
            let mut waits = 0usize;
            'attempt: loop {
            let existing_for_dedup = find_existing_facts_for_dedup(
                self.fact_store.as_ref(),
                &input.tenant,
                candidate.memory_type,
            )
            .await?;

            if let DeduplicationDecision::Duplicate {
                existing_fact_id,
                reason,
            } = detect_duplicate(&candidate, &existing_for_dedup)
            {
                if let Some(other) = journal.foreign_in_flight(existing_fact_id).await? {
                    journal.await_in_flight(other, &mut waits).await?;
                    continue 'attempt;
                }
                // The SQL match proves the row, not its vector. Verify the
                // matched fact's points and repair a missing or stale vector before the NOOP is
                // acknowledged; a repair failure fails the request.
                self.heal_fact_vector(&input.tenant, existing_fact_id)
                    .await?;
                summary.discard(FactDiscardReason::DuplicateContent);
                record_event_best_effort(
                    &self.event_store,
                    &input.tenant,
                    build_noop_event(
                        &input.tenant,
                        &dedup_noop_decision(existing_fact_id, reason),
                        &self.audit_provider_name,
                        &self.audit_model_name,
                    ),
                )
                .await;
                continue 'candidates;
            }

            let candidate_embedding = timed(
                "ingest",
                "fact_embed",
                self.embedding_provider.embed_text(&candidate.content),
            )
            .await?;

            let dedup_started = std::time::Instant::now();
            if let Some(DeduplicationDecision::Duplicate {
                existing_fact_id,
                reason,
            }) = detect_embedding_duplicate(
                self.fact_store.as_ref(),
                self.vector_store.as_ref(),
                &input.tenant,
                candidate.memory_type,
                &candidate_embedding,
                &self.embedding_dedup_config,
            )
            .await?
            {
                if let Some(other) = journal.foreign_in_flight(existing_fact_id).await? {
                    journal.await_in_flight(other, &mut waits).await?;
                    continue 'attempt;
                }
                summary.discard(FactDiscardReason::DuplicateEmbedding);
                record_event_best_effort(
                    &self.event_store,
                    &input.tenant,
                    build_noop_event(
                        &input.tenant,
                        &dedup_noop_decision(existing_fact_id, reason),
                        &self.audit_provider_name,
                        &self.audit_model_name,
                    ),
                )
                .await;
                continue 'candidates;
            }

            let lookup_started = std::time::Instant::now();
            let related_facts = match self.related_fact_matcher {
                RelatedFactMatcher::Legacy => {
                    find_related_facts(self.fact_store.as_ref(), &input.tenant, &candidate).await?
                }
                RelatedFactMatcher::Semantic => {
                    find_related_facts_semantic(
                        self.fact_store.as_ref(),
                        self.vector_store.as_ref(),
                        &input.tenant,
                        &candidate,
                        &candidate_embedding,
                    )
                    .await?
                }
                RelatedFactMatcher::SemanticV23 => {
                    find_related_facts_semantic_v23(
                        self.fact_store.as_ref(),
                        self.vector_store.as_ref(),
                        &input.tenant,
                        &candidate,
                        &candidate_embedding,
                    )
                    .await?
                }
            };
            // V2.2 evaluation: the lookup's own duration (the `dedup_classify` stage also includes
            // the classifier call). Sizes, ids and timings only: no fact or candidate text.
            record_stage("ingest", "related_lookup", lookup_started);
            tracing::info!(
                event = "related_fact_lookup",
                matcher = ?self.related_fact_matcher,
                offered = related_facts.len(),
                offered_ids = ?related_facts.iter().map(|fact| fact.id).collect::<Vec<_>>(),
                elapsed_us = u64::try_from(lookup_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                "related-fact lookup"
            );
            let offered = self
                .related_fact_matcher
                .is_semantic()
                .then(|| related_facts.clone());

            // With no related fact there is nothing to update, delete
            // or conflict with, and the stored result of an Add depends only on the candidate
            // (`apply_add` reads no other decision field), so the provider call is skipped.
            let decision = if related_facts.is_empty() && self.classification_skip_when_no_related {
                record_classification_skipped();
                no_related_add_decision()
            } else {
                match self
                    .llm_provider
                    .classify_fact_operation(FactClassificationInput {
                        tenant: input.tenant.clone(),
                        candidate_fact: candidate.clone(),
                        existing_facts: related_facts,
                    })
                    .await
                {
                    Ok(decision) => decision,
                    // One invalid classification used to fail the whole write after the
                    // earlier candidates were already stored. The candidate is rejected (not stored,
                    // not reinterpreted as an Add) and counted; the request goes on. Transport and
                    // provider failures still fail the request.
                    Err(error) if error.is_provider_output_invalid() => {
                        tracing::warn!(
                            event = "candidate_rejected_invalid_classification",
                            error_code = error.code(),
                            "a candidate fact was rejected: the model's classification was invalid"
                        );
                        summary.discard(FactDiscardReason::InvalidClassification);
                        continue 'candidates;
                    }
                    Err(error) => return Err(error),
                }
            };
            record_stage("ingest", "dedup_classify", dedup_started);

            // Under the semantic matcher an Update / Delete applies only to
            // an offered fact and keeps history; anything else it may not change is an Add.
            let decision = match offered {
                None => decision,
                Some(offered) => match if self.related_fact_matcher
                    == RelatedFactMatcher::SemanticV23
                {
                    guard_related_decision_v23(&decision, &offered, &candidate)
                } else {
                    guard_related_decision(&decision, &offered, &candidate)
                } {
                    RelatedDecision::Apply {
                        decision: guarded,
                        downgraded,
                    } => {
                        if let Some(reason) = downgraded {
                            crate::observability::record_related_decision_downgraded(reason);
                        }
                        guarded
                    }
                    RelatedDecision::Supersede { target } => {
                        if let Some(other) = journal.foreign_in_flight(target).await? {
                            journal.await_in_flight(other, &mut waits).await?;
                            continue 'attempt;
                        }
                        if let Some(other) = self
                            .supersede_with_candidate(
                                &input,
                                &candidate,
                                &lifecycle_ctx,
                                &mut journal,
                                candidate_embedding,
                                target,
                                &decision,
                                &mut summary,
                                &mut memories,
                            )
                            .await?
                        {
                            journal.await_in_flight(other, &mut waits).await?;
                            continue 'attempt;
                        }
                        continue 'candidates;
                    }
                },
            };

            if matches!(decision.operation, FactOperation::Update | FactOperation::Delete)
                && let Some(target) = decision.target_fact_id
                && let Some(other) = journal.foreign_in_flight(target).await?
            {
                journal.await_in_flight(other, &mut waits).await?;
                continue 'attempt;
            }
            let new_fact_id = journal.record_decision(&lifecycle_ctx, &decision).await?;
            let result = timed(
                "ingest",
                "fact_write",
                apply_fact_operation_with_id(
                    &lifecycle_ctx,
                    &input.tenant,
                    &candidate,
                    &decision,
                    &input.metadata,
                    Some(candidate_embedding),
                    crate::dedup::ExactDedupRule::Legacy,
                    new_fact_id,
                ),
            )
            .await?;

            match result {
                LifecycleApplyResult::Added(fact) => {
                    summary.added += 1;
                    added_this_request.push(fact.clone());
                    record_event_best_effort(
                        &self.event_store,
                        &input.tenant,
                        build_add_event(
                            &input.tenant,
                            &fact,
                            &self.audit_provider_name,
                            &self.audit_model_name,
                            input.metadata.clone(),
                        ),
                    )
                    .await;
                    memories.push(fact);
                }
                LifecycleApplyResult::Updated { previous, updated } => {
                    summary.updated += 1;
                    record_event_best_effort(
                        &self.event_store,
                        &input.tenant,
                        build_update_event(
                            &input.tenant,
                            &previous,
                            &updated,
                            &self.audit_provider_name,
                            &self.audit_model_name,
                            input.metadata.clone(),
                        ),
                    )
                    .await;
                    memories.push(updated);
                }
                LifecycleApplyResult::Deleted(fact) => {
                    summary.deleted += 1;
                    self.delete_entity_data_for_facts(&input.tenant, &[fact.id])
                        .await?;
                    record_event_best_effort(
                        &self.event_store,
                        &input.tenant,
                        build_delete_event(
                            &input.tenant,
                            &fact,
                            &self.audit_provider_name,
                            &self.audit_model_name,
                            input.metadata.clone(),
                        ),
                    )
                    .await;
                }
                LifecycleApplyResult::NoOp => {
                    summary.discard(FactDiscardReason::NoOpDecision);
                    record_event_best_effort(
                        &self.event_store,
                        &input.tenant,
                        build_noop_event(
                            &input.tenant,
                            &decision,
                            &self.audit_provider_name,
                            &self.audit_model_name,
                        ),
                    )
                    .await;
                }
                LifecycleApplyResult::Duplicate { existing_fact_id } => {
                    if let Some(other) = journal.foreign_in_flight(existing_fact_id).await? {
                        journal.await_in_flight(other, &mut waits).await?;
                        continue 'attempt;
                    }
                    self.record_concurrent_duplicate(&input.tenant, existing_fact_id, &mut summary)
                        .await;
                }
            }
            break 'attempt;
            }
        }
        Ok(())
        }
        .await;
        if let Err(error) = loop_result {
            if journal.enabled() {
                journal.rollback(&error).await;
            } else {
                self.compensate_partial_write(&input.tenant, &added_this_request, &summary, &error)
                    .await;
            }
            return Err(error);
        }

        if summary.added + summary.updated + summary.deleted + summary.invalidated > 0 {
            self.invalidate_user_context_cache(&input.tenant).await;
        }

        let entities = match (&self.entities, messages_for_entities) {
            (Some(components), Some(messages)) => Some(
                self.process_entities(components, &input, messages, &memories)
                    .await,
            ),
            _ => None,
        };

        if summary.discarded.total() > 0 {
            tracing::debug!(
                event = "add_memory_candidates_discarded",
                extracted = summary.extracted,
                stored = summary.stored(),
                discarded = summary.discarded.total(),
                low_importance = summary.discarded.low_importance,
                duplicate_content = summary.discarded.duplicate_content,
                duplicate_embedding = summary.discarded.duplicate_embedding,
                no_op_decision = summary.discarded.no_op_decision,
                "extraction candidates were not stored"
            );
        }

        let output = AddMemoryOutput {
            extracted: summary.extracted,
            added: summary.added,
            updated: summary.updated,
            deleted: summary.deleted,
            invalidated: summary.invalidated,
            noop: summary.noop,
            discarded: summary.discarded,
            skipped: summary.skipped,
            event_time_unparsed: summary.event_time_unparsed,
            temporal: summary.temporal,
            invalid_source_message_ids: summary.invalid_source_message_ids,
            memories,
            entities,
        };

        // Every operation of this request is complete; only now may it succeed. The
        // receipt (for a request with a stable identity) commits in the same transaction.
        if let Err(error) = journal.commit(&output).await {
            journal.rollback(&error).await;
            return Err(error);
        }

        Ok(output)
    }

    /// A write that fails after some of its candidates were
    /// already added used to keep them while the client received an error; a retry then
    /// re-extracted and could store paraphrased duplicates. The facts this request **added** are now
    /// soft-deleted and their vectors removed (the pre-request active state; the rows stay as deleted
    /// audit records, and the exact-dedup key is released), each with a delete event, a warning and a
    /// metric. Updates, deletes and supersessions already applied are kept: undoing half of a
    /// supersession would leave no current value. Episodes are kept (a retry with stable message ids
    /// reuses them). Not covered: a request cancelled mid-way (client disconnect or deadline drops
    /// the future before this runs) and a crash during compensation; both stay observable through
    /// the existing write-failure metrics and are repairable with the verify tooling.
    async fn compensate_partial_write(
        &self,
        tenant: &TenantContext,
        added: &[Fact],
        summary: &MemoryOperationSummary,
        error: &memcore_common::MemcoreError,
    ) {
        if added.is_empty() {
            return;
        }
        let (mut rolled_back, mut failures) = (0usize, 0usize);
        for fact in added {
            let row = self.fact_store.soft_delete_fact(tenant, fact.id).await;
            let point = match self.vector_store.delete_by_fact_id(tenant, fact.id).await {
                Ok(()) | Err(memcore_common::MemcoreError::NotFound(_)) => Ok(()),
                Err(other) => Err(other),
            };
            if row.is_ok() && point.is_ok() {
                rolled_back += 1;
                record_event_best_effort(
                    &self.event_store,
                    tenant,
                    build_delete_event(
                        tenant,
                        fact,
                        &self.audit_provider_name,
                        &self.audit_model_name,
                        serde_json::json!({ "reason": "partial_write_compensation" }),
                    ),
                )
                .await;
            } else {
                failures += 1;
            }
        }
        tracing::warn!(
            event = "partial_write_compensated",
            added_rolled_back = rolled_back,
            compensation_failures = failures,
            non_add_operations_kept = summary.updated + summary.deleted + summary.invalidated,
            error_code = error.code(),
            "a failed write was compensated: the facts it had added were removed"
        );
        crate::observability::record_partial_write_compensation(rolled_back, failures);
        self.invalidate_user_context_cache(tenant).await;
    }

    /// Consolidation v2 for one candidate (`crate::consolidation`). Every
    /// outcome is recorded as a memory event whose metadata carries the path taken, the decision,
    /// its reason and the offered candidates. Nothing is ever hard-deleted.
    async fn consolidate_v2(
        &self,
        input: &AddMemoryInput,
        candidate: &crate::CandidateFact,
        config: &ConsolidationConfig,
        ctx: &LifecycleContext<'_>,
        journal: &mut JournalSession<'_>,
        summary: &mut MemoryOperationSummary,
        memories: &mut Vec<Fact>,
    ) -> MemcoreResult<()> {
        use serde_json::json;

        let embedding = self
            .embedding_provider
            .embed_text(&candidate.content)
            .await?;
        let related = gather_related_facts(
            self.fact_store.as_ref(),
            self.vector_store.as_ref(),
            &input.tenant,
            candidate,
            &embedding,
            config,
        )
        .await?;
        let mut audit = json!({
            "consolidation_mode": "llm_v2",
            "candidates": related.iter().map(|related| json!({
                "fact_id": related.fact.id,
                "similarity": related.similarity,
                "same_subject": related.same_subject,
            })).collect::<Vec<_>>(),
        });

        let decide = |operation, target_fact_id, reason: &str| ConsolidationDecision {
            operation,
            target_fact_id,
            merged_content: None,
            invalid_at: None,
            invalid_at_unparsed: None,
            reason: Some(reason.to_string()),
        };
        let (decision, path) = match fast_path(candidate, &related, config) {
            FastPath::NoOp { target_fact_id } => (
                decide(
                    ConsolidationOperation::NoOp,
                    Some(target_fact_id),
                    "exact duplicate: same text, subject and event time",
                ),
                "fast_noop",
            ),
            FastPath::Add => (
                decide(
                    ConsolidationOperation::Add,
                    None,
                    "no related fact at or above the similarity floor",
                ),
                "fast_add",
            ),
            FastPath::Decide => {
                let proposed = self
                    .llm_provider
                    .decide_consolidation(ConsolidationInput {
                        tenant: input.tenant.clone(),
                        candidate_fact: candidate.clone(),
                        existing_facts: related
                            .iter()
                            .map(|related| related.fact.clone())
                            .collect(),
                    })
                    .await?;
                audit["model_decision"] = json!({
                    "operation": proposed.operation,
                    "target_fact_id": proposed.target_fact_id,
                    "invalid_at": proposed.invalid_at,
                    "invalid_at_unparsed": proposed.invalid_at_unparsed,
                    "reason": proposed.reason,
                });
                match validate_decision(&proposed, candidate, &related) {
                    Ok(validated) => (validated, "model"),
                    Err(rejection) => {
                        audit["rejected"] = json!(rejection);
                        (
                            decide(
                                ConsolidationOperation::Add,
                                None,
                                "model decision rejected; stored as a new fact",
                            ),
                            "model_rejected",
                        )
                    }
                }
            }
        };
        audit["path"] = json!(path);
        audit["operation"] = json!(decision.operation);
        audit["reason"] = json!(decision.reason);

        match decision.operation {
            ConsolidationOperation::NoOp => {
                summary.discard(if path == "fast_noop" {
                    FactDiscardReason::DuplicateContent
                } else {
                    FactDiscardReason::NoOpDecision
                });
                record_event_best_effort(
                    &self.event_store,
                    &input.tenant,
                    build_consolidation_noop_event(
                        &input.tenant,
                        decision.target_fact_id,
                        &self.audit_provider_name,
                        &self.audit_model_name,
                        audit,
                    ),
                )
                .await;
            }
            ConsolidationOperation::Add => {
                self.consolidation_add(
                    input, candidate, ctx, journal, None, embedding, audit, summary, memories,
                )
                .await?;
            }
            ConsolidationOperation::Update => {
                let target = decision
                    .target_fact_id
                    .expect("validated UPDATE has a target");
                let content = decision
                    .merged_content
                    .clone()
                    .unwrap_or_else(|| candidate.content.clone());
                journal.record_update(ctx, target).await?;
                if let LifecycleApplyResult::Updated { previous, updated } =
                    apply_update_with_content(ctx, &input.tenant, candidate, target, &content)
                        .await?
                {
                    summary.updated += 1;
                    record_event_best_effort(
                        &self.event_store,
                        &input.tenant,
                        build_update_event(
                            &input.tenant,
                            &previous,
                            &updated,
                            &self.audit_provider_name,
                            &self.audit_model_name,
                            with_consolidation_audit(&input.metadata, &audit),
                        ),
                    )
                    .await;
                    memories.push(updated);
                }
            }
            ConsolidationOperation::Invalidate => {
                let target = decision
                    .target_fact_id
                    .expect("validated INVALIDATE has a target");
                // Superseded now (bookkeeping); `invalid_at` only from a
                // source-supported change time, else left unset.
                let superseded_at = Utc::now();
                let resolution = resolve_invalid_at(&decision, candidate, superseded_at);
                audit["invalid_at"] = json!(resolution.invalid_at);
                audit["invalid_at_source"] = json!(resolution.source.as_str());
                audit["superseded_at"] = json!(superseded_at);
                if let Some(rejected) = &resolution.rejected {
                    audit["invalid_at_rejected"] = json!({
                        "value": rejected.value,
                        "reason": rejected.reason,
                    });
                }
                let replacement_id = Uuid::new_v4();
                journal
                    .record_supersession(ctx, target, Some(replacement_id))
                    .await?;
                let (before, _after) = apply_invalidate(
                    ctx,
                    &input.tenant,
                    target,
                    resolution.invalid_at,
                    superseded_at,
                )
                .await?;
                summary.invalidated += 1;
                // The candidate is the new current statement ("INVALIDATE old + ADD
                // new"); once stored, the superseded fact links to it.
                let mut add_audit = audit.clone();
                add_audit["superseded_fact_id"] = json!(target);
                let added = self
                    .consolidation_add(
                        input,
                        candidate,
                        ctx,
                        journal,
                        Some(replacement_id),
                        embedding,
                        add_audit,
                        summary,
                        memories,
                    )
                    .await;
                let mut replacement = None;
                if let Ok(Some(replacement_id)) = &added
                    && link_superseded_by(ctx, &input.tenant, target, *replacement_id)
                        .await
                        .is_ok()
                {
                    replacement = Some(*replacement_id);
                }
                audit["superseded_by"] = json!(replacement);
                record_event_best_effort(
                    &self.event_store,
                    &input.tenant,
                    build_invalidate_event(
                        &input.tenant,
                        &before,
                        &self.audit_provider_name,
                        &self.audit_model_name,
                        audit,
                    ),
                )
                .await;
                added?;
            }
        }
        Ok(())
    }

    /// The classifier's Update / Delete of an offered
    /// fact. The candidate is stored first as a new fact; only then is `target` marked superseded
    /// and linked to it, so a failure never leaves the user without the new statement. The previous
    /// statement keeps its row, vector and history; `invalid_at` is left unset (no
    /// source-supported change time here). A concurrent identical add supersedes nothing.
    #[allow(clippy::too_many_arguments)]
    async fn supersede_with_candidate(
        &self,
        input: &AddMemoryInput,
        candidate: &crate::CandidateFact,
        ctx: &LifecycleContext<'_>,
        journal: &mut JournalSession<'_>,
        embedding: Vec<f32>,
        target: Uuid,
        decision: &FactOperationDecision,
        summary: &mut MemoryOperationSummary,
        memories: &mut Vec<Fact>,
    ) -> MemcoreResult<Option<Uuid>> {
        use serde_json::json;

        let add = FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: decision.reason.clone(),
            confidence: decision.confidence,
        };
        // One journal step for the whole supersession (the target's before-image and
        // the replacement's id), so a rollback undoes it as a unit.
        let replacement_id = Uuid::new_v4();
        journal
            .record_supersession(ctx, target, Some(replacement_id))
            .await?;
        let result = timed(
            "ingest",
            "fact_write",
            apply_fact_operation_with_id(
                ctx,
                &input.tenant,
                candidate,
                &add,
                &input.metadata,
                Some(embedding),
                crate::dedup::ExactDedupRule::Legacy,
                replacement_id,
            ),
        )
        .await?;
        let fact = match result {
            LifecycleApplyResult::Added(fact) => fact,
            LifecycleApplyResult::Duplicate { existing_fact_id } => {
                // The equivalent fact may be another request's uncommitted add; the
                // caller waits for it and processes the candidate again.
                if let Some(other) = journal.foreign_in_flight(existing_fact_id).await? {
                    return Ok(Some(other));
                }
                self.record_concurrent_duplicate(&input.tenant, existing_fact_id, summary)
                    .await;
                return Ok(None);
            }
            _ => return Ok(None),
        };
        summary.added += 1;
        let audit = json!({
            "related_fact_matcher": self.related_fact_matcher.as_str(),
            "classifier_operation": decision.operation,
            "classifier_reason": decision.reason,
            "superseded_fact_id": target,
        });
        record_event_best_effort(
            &self.event_store,
            &input.tenant,
            build_add_event(
                &input.tenant,
                &fact,
                &self.audit_provider_name,
                &self.audit_model_name,
                with_consolidation_audit(&input.metadata, &audit),
            ),
        )
        .await;
        let replacement = fact.id;
        memories.push(fact);
        let (before, _after) =
            apply_invalidate(ctx, &input.tenant, target, None, Utc::now()).await?;
        summary.invalidated += 1;
        link_superseded_by(ctx, &input.tenant, target, replacement).await?;
        let mut invalidate_audit = audit;
        invalidate_audit["superseded_by"] = json!(replacement);
        record_event_best_effort(
            &self.event_store,
            &input.tenant,
            build_invalidate_event(
                &input.tenant,
                &before,
                &self.audit_provider_name,
                &self.audit_model_name,
                invalidate_audit,
            ),
        )
        .await;
        Ok(None)
    }

    /// Stores `candidate` as a new fact for consolidation v2 and records the add event. Returns the
    /// stored fact's ID. `journalled_id` is the id a journalled supersession already allocated for
    /// it; otherwise the add is journalled here.
    #[allow(clippy::too_many_arguments)]
    async fn consolidation_add(
        &self,
        input: &AddMemoryInput,
        candidate: &crate::CandidateFact,
        ctx: &LifecycleContext<'_>,
        journal: &mut JournalSession<'_>,
        journalled_id: Option<Uuid>,
        embedding: Vec<f32>,
        audit: serde_json::Value,
        summary: &mut MemoryOperationSummary,
        memories: &mut Vec<Fact>,
    ) -> MemcoreResult<Option<Uuid>> {
        let add = FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: None,
            confidence: 1.0,
        };
        let fact_id = match journalled_id {
            Some(id) => id,
            None => {
                let id = Uuid::new_v4();
                journal
                    .record(crate::ports::IngestJournalStep::Add { fact_id: id })
                    .await?;
                id
            }
        };
        let result = apply_fact_operation_with_id(
            ctx,
            &input.tenant,
            candidate,
            &add,
            &input.metadata,
            Some(embedding),
            crate::dedup::ExactDedupRule::ConsolidationV2,
            fact_id,
        )
        .await?;
        if let LifecycleApplyResult::Duplicate { existing_fact_id } = result {
            self.record_concurrent_duplicate(&input.tenant, existing_fact_id, summary)
                .await;
            return Ok(None);
        }
        if let LifecycleApplyResult::Added(fact) = result {
            summary.added += 1;
            record_event_best_effort(
                &self.event_store,
                &input.tenant,
                build_add_event(
                    &input.tenant,
                    &fact,
                    &self.audit_provider_name,
                    &self.audit_model_name,
                    with_consolidation_audit(&input.metadata, &audit),
                ),
            )
            .await;
            let id = fact.id;
            memories.push(fact);
            return Ok(Some(id));
        }
        Ok(None)
    }

    pub async fn search_memory(
        &self,
        input: SearchMemoryInput,
    ) -> MemcoreResult<SearchMemoryOutput> {
        self.search_memory_with_mode(input, self.default_retrieval_mode)
            .await
    }

    /// Search with an explicit set of legs. `Hybrid` is the original fused search. `Semantic`
    /// skips the keyword leg; `Fulltext` skips the embedding call and the vector leg and ranks by the
    /// store's full-text relevance; `Rrf` fuses the vector leg with the full-text leg. A single leg
    /// keeps its own order (RRF over one list is monotone). Every leg fetches the same candidate
    /// count, twice the requested limit.
    pub async fn search_memory_with_mode(
        &self,
        input: SearchMemoryInput,
        mode: RetrievalMode,
    ) -> MemcoreResult<SearchMemoryOutput> {
        self.search_memory_with_options(
            input,
            SearchOptions {
                mode: Some(mode),
                ..SearchOptions::default()
            },
        )
        .await
    }

    /// Search with explicit options: the retrieval legs (default: the configured mode) and the
    /// scoped user's names, which the full-text leg does not search for.
    pub async fn search_memory_with_options(
        &self,
        input: SearchMemoryInput,
        options: SearchOptions,
    ) -> MemcoreResult<SearchMemoryOutput> {
        use std::cmp::Ordering;
        let mode = options.mode.unwrap_or(self.default_retrieval_mode);
        use std::collections::HashMap;

        validate_tenant(&input.tenant)?;
        validate_query(&input.query)?;
        self.bind_usage_attribution(&input.tenant);
        let limit = normalize_search_limit(input.limit, self.search_max_limit)?;
        let internal_limit = internal_search_limit(limit);

        let mut vector_results = if mode == RetrievalMode::Fulltext {
            Vec::new()
        } else {
            let embedding = timed(
                "search",
                "fact_query_embed",
                self.embed_search_query(&input.tenant, &input.query),
            )
            .await?;
            timed(
                "search",
                "fact_vector_search",
                self.vector_store
                    .search_vectors(crate::ports::VectorSearchQuery {
                        tenant: input.tenant.clone(),
                        embedding,
                        limit: internal_limit,
                        memory_types: input.memory_types.clone(),
                        metadata_filter: input.metadata_filter,
                    }),
            )
            .await?
        };

        // Equal vector scores are common (facts with identical text have identical embeddings)
        // and a vector store may return tied hits in any order, so the same logical store could
        // rank them differently from one request or rebuild to the next. Hits keep their score
        // order; only exactly equal scores (no epsilon) are ordered by stable fields of the fact:
        // content, then source message ids, then subject. The fact id only separates facts
        // identical in all three. The facts read here are reused below instead of re-read.
        //
        // SQL is the authority for every semantic hit. A hit is used only if
        // the tenant's live SQL fact exists and the point carries that row's current text and
        // memory type (`vector_hit_rejection`); the returned text and metadata always come from
        // the SQL row. A rejected hit (an orphan, or a stale point left by a failed vector write)
        // keeps its position in the hit order, exactly as orphans always did, so the ranks and
        // fused scores of the accepted hits are unchanged; it contributes nothing else. A SQL
        // error fails the search: Qdrant text is never a fallback.
        let fetch_started = std::time::Instant::now();
        let mut vector_facts: HashMap<Uuid, Fact> = HashMap::with_capacity(vector_results.len());
        for hit in &vector_results {
            if let Some(fact) = self.fact_store.get_fact(&input.tenant, hit.fact_id).await?
                && fact.org_id == input.tenant.org_id
                && fact.user_id == input.tenant.user_id
            {
                vector_facts.insert(hit.fact_id, fact);
            }
        }
        record_stage("search", "fact_sql_fetch", fetch_started);
        let merge_started = std::time::Instant::now();
        vector_results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    vector_tie_key(left, &vector_facts).cmp(&vector_tie_key(right, &vector_facts))
                })
        });

        let keyword_facts = match mode {
            RetrievalMode::Hybrid => {
                self.fact_store
                    .search_facts(FactSearchQuery {
                        tenant: input.tenant.clone(),
                        memory_types: input.memory_types.clone(),
                        query_text: Some(input.query.clone()),
                        limit: internal_limit,
                        cursor: None,
                        include_deleted: false,
                    })
                    .await?
            }
            RetrievalMode::Semantic => Vec::new(),
            RetrievalMode::Fulltext | RetrievalMode::Rrf => self
                .fact_store
                .fulltext_search_facts(FactFulltextQuery {
                    tenant: input.tenant.clone(),
                    memory_types: input.memory_types.clone(),
                    query: crate::search::FulltextQuery::parse_scoped(
                        &input.query,
                        &options.user_names,
                    ),
                    limit: internal_limit,
                })
                .await?
                .into_iter()
                .map(|scored| scored.fact)
                .collect(),
        };

        let accepted: Vec<bool> = vector_results
            .iter()
            .map(|hit| {
                match vector_hit_rejection(hit, vector_facts.get(&hit.fact_id), &input.tenant) {
                    Some(reason) => {
                        record_vector_hit_rejected("search", reason);
                        false
                    }
                    None => true,
                }
            })
            .collect();

        let semantic_candidates: Vec<RankedCandidate> = vector_results
            .iter()
            .enumerate()
            .filter(|(index, _)| accepted[*index])
            .map(|(index, vector_result)| RankedCandidate {
                fact_id: vector_result.fact_id,
                source: RankingSource::Semantic,
                rank: index + 1,
                score: vector_result.score,
            })
            .collect();

        let keyword_candidates: Vec<RankedCandidate> = keyword_facts
            .iter()
            .enumerate()
            .map(|(index, fact)| RankedCandidate {
                fact_id: fact.id,
                source: RankingSource::Keyword,
                rank: index + 1,
                score: 0.0,
            })
            .collect();

        let fused =
            reciprocal_rank_fusion(&semantic_candidates, &keyword_candidates, &self.rrf_config);

        let vector_by_fact: HashMap<Uuid, _> = vector_results
            .iter()
            .zip(&accepted)
            .filter(|(_, accepted)| **accepted)
            .map(|(result, _)| (result.fact_id, result))
            .collect();
        let keyword_by_fact: HashMap<Uuid, _> =
            keyword_facts.iter().map(|fact| (fact.id, fact)).collect();
        let now = Utc::now();

        let mut scored: Vec<(MemorySearchResult, f32, f32, chrono::DateTime<Utc>)> =
            Vec::with_capacity(fused.len());
        let mut history: HashMap<Uuid, types::HistoricalResult> = HashMap::new();

        for (fact_id, rrf_score) in fused {
            let fact = if let Some(fact) = keyword_by_fact.get(&fact_id) {
                Some((*fact).clone())
            } else if let Some(fact) = vector_facts.get(&fact_id) {
                Some(fact.clone())
            } else {
                self.fact_store.get_fact(&input.tenant, fact_id).await?
            };

            let Some(fact) = fact else {
                continue;
            };
            if fact.org_id != input.tenant.org_id || fact.user_id != input.tenant.user_id {
                continue;
            }
            // With consolidation v2 on, inactive facts (invalidated or superseded) are
            // history, left out unless the request asks for it.: the same
            // with the semantic related-fact matcher, the one legacy-mode path that supersedes.
            // Legacy mode otherwise never leaves anything out.
            let history_kept_out =
                self.consolidation.is_some() || self.related_fact_matcher.is_semantic();
            if history_kept_out && !options.include_history && !fact.is_active() {
                continue;
            }
            if !fact.is_active() {
                history.insert(
                    fact.id,
                    types::HistoricalResult {
                        fact_id: fact.id,
                        superseded_at: fact.superseded_at,
                        superseded_by: fact.superseded_by,
                        invalid_at: fact.invalid_at,
                    },
                );
            }

            let vector_result = vector_by_fact.get(&fact_id);
            let semantic_score = vector_result.map(|result| result.score).unwrap_or(0.0);
            // The SQL row's text and metadata, never the point's payload. An
            // accepted hit carries the same text (checked above); S0 found the stored payload
            // metadata equal to SQL on every fact.
            let content = fact.content.clone();
            let metadata = fact.metadata.clone();

            let weighted = weighted_score_for(
                semantic_score,
                fact.importance,
                fact.confidence,
                Some(fact.updated_at),
                &fact.memory_type,
                now,
                &self.ranking_config,
            );

            scored.push((
                MemorySearchResult {
                    fact_id: fact.id,
                    content,
                    memory_type: fact.memory_type,
                    score: rrf_score,
                    vector_score: vector_result.map(|result| result.score),
                    confidence: fact.confidence,
                    importance: fact.importance,
                    valid_at: fact.valid_at,
                    metadata,
                    structure: fact.structure.clone(),
                },
                rrf_score,
                weighted,
                fact.updated_at,
            ));
        }

        scored.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then(right.2.partial_cmp(&left.2).unwrap_or(Ordering::Equal))
                .then(right.3.cmp(&left.3))
                .then_with(|| left.0.fact_id.cmp(&right.0.fact_id))
        });
        scored.truncate(limit);

        let results: Vec<MemorySearchResult> = scored
            .into_iter()
            .map(|(mut result, rrf_score, _, _)| {
                result.score = rrf_score;
                result
            })
            .collect();

        record_stage("search", "fact_merge", merge_started);
        let history = results
            .iter()
            .filter_map(|result| history.remove(&result.fact_id))
            .collect();
        Ok(SearchMemoryOutput { results, history })
    }

    /// Semantic search over one user's episodes: one vector search of exactly
    /// `limit` hits, ordered by vector similarity. Exactly equal scores (no epsilon) are ordered by
    /// content, message ID, session ID and finally episode ID, as tied facts are ordered. No
    /// full-text leg, reranking or query rewriting.
    pub async fn search_episodes(
        &self,
        input: SearchEpisodesInput,
    ) -> MemcoreResult<SearchEpisodesOutput> {
        use std::cmp::Ordering;
        use std::collections::HashMap;

        let Some(episodes) = &self.episodes else {
            return Err(memcore_common::MemcoreError::ValidationError(
                "episodes are not enabled (MEMCORE_EPISODES_ENABLED)".to_string(),
            ));
        };
        validate_tenant(&input.tenant)?;
        validate_query(&input.query)?;
        self.bind_usage_attribution(&input.tenant);
        let limit = normalize_search_limit(input.limit, self.search_max_limit)?;

        let embedding = timed(
            "search",
            "episode_query_embed",
            self.embed_search_query(&input.tenant, &input.query),
        )
        .await?;
        let hits = timed(
            "search",
            "episode_vector_search",
            episodes
                .vectors
                .search_vectors(crate::ports::VectorSearchQuery {
                    tenant: input.tenant.clone(),
                    embedding,
                    limit,
                    memory_types: None,
                    metadata_filter: None,
                }),
        )
        .await?;
        let ids: Vec<Uuid> = hits.iter().map(|hit| hit.fact_id).collect();
        let stored: HashMap<Uuid, Episode> = timed(
            "search",
            "episode_sql_fetch",
            episodes.store.get_episodes(&input.tenant, &ids),
        )
        .await?
        .into_iter()
        .map(|episode| (episode.id, episode))
        .collect();

        let merge_started = std::time::Instant::now();
        let mut results: Vec<EpisodeSearchResult> = hits
            .into_iter()
            .filter_map(|hit| {
                stored.get(&hit.fact_id).map(|episode| EpisodeSearchResult {
                    episode: episode.clone(),
                    score: hit.score,
                })
            })
            .collect();
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| episode_tie_key(&left.episode).cmp(&episode_tie_key(&right.episode)))
        });

        record_stage("search", "episode_merge", merge_started);
        Ok(SearchEpisodesOutput { results })
    }

    /// Stores the request's messages as episodes and embeds the eligible ones.
    async fn store_episodes(
        &self,
        input: &AddMemoryInput,
        episodes: &EpisodeComponents,
    ) -> MemcoreResult<()> {
        let now = Utc::now();
        let batch = input
            .messages
            .iter()
            .map(|message| Episode {
                id: Uuid::new_v4(),
                org_id: input.tenant.org_id.clone(),
                user_id: input.tenant.user_id.clone(),
                session_id: input.session.id.clone(),
                message_id: message.message_id.clone(),
                speaker: message.name.clone(),
                role: message.role.clone(),
                // The same redaction extraction applies; stored text can differ from the original.
                content: if self.enable_pii_redaction {
                    PiiRedactor::redact_text(&message.content)
                } else {
                    message.content.clone()
                },
                // The client's time only: message, then session. Never the receipt time (P2-20).
                observed_at: message.timestamp.or(input.session.timestamp),
                recorded_at: now,
                embedded: false,
                metadata: input.metadata.clone(),
            })
            .collect();
        // Ingestion inserts the request's episodes atomically and rejects a client
        // message_id that already identifies a different message (409), before any extraction.
        self.index_episodes(&input.tenant, episodes, batch, true)
            .await?;
        Ok(())
    }

    /// Inserts episodes (idempotently) and embeds each stored episode that is long enough and not
    /// embedded yet, including one left unembedded by an earlier failed request. Returns how many
    /// rows the store returned. `checked` (ingestion) uses the atomic, conflict-checked insert;
    /// import keeps the existing insert.
    async fn index_episodes(
        &self,
        tenant: &TenantContext,
        episodes: &EpisodeComponents,
        batch: Vec<Episode>,
        checked: bool,
    ) -> MemcoreResult<usize> {
        if batch.is_empty() {
            return Ok(0);
        }
        let stored = timed("ingest", "episode_write", async {
            if checked {
                episodes.store.insert_episodes_checked(tenant, batch).await
            } else {
                episodes.store.insert_episodes(tenant, batch).await
            }
        })
        .await?;
        // The rows are stored; a failure from here leaves embeddable episodes with
        // `embedded = 0` (pending for `verify-vectors`). Counted for ingestion; an import counts
        // its own failure once under `import`.
        let indexed = async {
            let mut to_embed: Vec<&Episode> = Vec::new();
            for episode in &stored {
                if episode.embedded
                    || !episode.is_embeddable()
                    || to_embed.iter().any(|queued| queued.id == episode.id)
                {
                    continue;
                }
                to_embed.push(episode);
            }
            // One batched embedding request for the request's episodes instead of one
            // per episode; the vectors, their order and the upserts are unchanged.
            let embeddings = if to_embed.is_empty() {
                Vec::new()
            } else {
                timed(
                    "ingest",
                    "episode_embed",
                    self.embedding_provider.embed_batch(
                        to_embed
                            .iter()
                            .map(|episode| episode.content.clone())
                            .collect(),
                    ),
                )
                .await?
            };
            if embeddings.len() != to_embed.len() {
                return Err(memcore_common::MemcoreError::ProviderError(format!(
                    "batched episode embedding returned {} vectors for {} episodes",
                    embeddings.len(),
                    to_embed.len()
                )));
            }
            let mut embedded: Vec<Uuid> = Vec::with_capacity(to_embed.len());
            for (episode, embedding) in to_embed.into_iter().zip(embeddings) {
                // Point id = episode id (the stored row's id, kept on a retry): an upsert
                // overwrites the only point this episode can have.
                episodes
                    .vectors
                    .write_vector(
                        tenant,
                        VectorRecord {
                            id: episode.id,
                            fact_id: episode.id,
                            org_id: episode.org_id.clone(),
                            user_id: episode.user_id.clone(),
                            embedding,
                            content: episode.content.clone(),
                            memory_type: MemoryType::Conversation,
                            metadata: serde_json::json!({ "kind": "episode" }),
                        },
                        VectorWriteIntent::StablePointId,
                    )
                    .await?;
                embedded.push(episode.id);
            }
            if !embedded.is_empty() {
                episodes
                    .store
                    .mark_episodes_embedded(tenant, &embedded)
                    .await?;
            }
            Ok(stored.len())
        }
        .await;
        if checked {
            counted_write("episode_index", indexed)
        } else {
            indexed
        }
    }

    pub async fn build_context(
        &self,
        input: BuildContextInput,
    ) -> MemcoreResult<BuildContextOutput> {
        validate_tenant(&input.tenant)?;
        validate_query(&input.query)?;
        self.bind_usage_attribution(&input.tenant);
        input.budget.validate()?;
        input
            .compression_options
            .validate(input.budget.available_tokens())?;
        self.context_cache_config.validate()?;

        if !self.context_cache_config.enabled {
            return self.build_context_uncached(input).await;
        }

        let cache_key = build_context_cache_key(&input);
        let (entry, cache_usage) = self
            .context_cache_coordinator
            .get_or_compute(cache_key, || async {
                let output = self.build_context_uncached(input).await?;
                Ok(cached_entry_from_output(
                    &output,
                    &self.context_cache_config,
                ))
            })
            .await?;

        Ok(BuildContextOutput {
            context: entry.context,
            memories: entry.memories,
            budget: entry.budget,
            compression: entry.compression,
            cache: cache_usage,
        })
    }

    /// Recomputes and stores a stale context cache entry (background refresh).
    /// Releases the background-refresh slot a stale hit reserved when the refresh will not run
    /// (for example at shutdown), so the per-process refresh budget is not leaked.
    pub async fn release_stale_refresh(&self, input: &BuildContextInput) {
        let cache_key = build_context_cache_key(input);
        self.context_cache_coordinator
            .finish_refresh(&cache_key)
            .await;
    }

    pub async fn refresh_stale_context(&self, input: BuildContextInput) -> MemcoreResult<()> {
        validate_tenant(&input.tenant)?;
        validate_query(&input.query)?;
        self.bind_usage_attribution(&input.tenant);
        input.budget.validate()?;
        input
            .compression_options
            .validate(input.budget.available_tokens())?;
        self.context_cache_config.validate()?;

        if !self.context_cache_config.enabled {
            return Ok(());
        }

        let cache_key = build_context_cache_key(&input);
        self.context_cache_coordinator
            .refresh_entry(cache_key, || async {
                let output = self.build_context_uncached(input).await?;
                Ok(cached_entry_from_output(
                    &output,
                    &self.context_cache_config,
                ))
            })
            .await
    }

    async fn build_context_uncached(
        &self,
        input: BuildContextInput,
    ) -> MemcoreResult<BuildContextOutput> {
        let max_memories = normalize_context_max_memories(input.max_memories)?;
        let tenant = input.tenant.clone();
        let compression_options = input.compression_options.clone();
        let format_options = input.format_options.clone();

        let include_history = input.include_history;
        let search_output = self
            .search_memory_with_options(
                SearchMemoryInput {
                    tenant: input.tenant,
                    query: input.query,
                    limit: max_memories,
                    memory_types: input.memory_types,
                    metadata_filter: None,
                },
                SearchOptions {
                    include_history,
                    ..SearchOptions::default()
                },
            )
            .await?;

        // A context built with history states which memories are no longer current,
        // so an answer is never built on a previous value taken for the current one. Without
        // history (the default) no result is inactive and the context is unchanged.
        let labelled: Vec<MemorySearchResult>;
        let results = if include_history && !search_output.history.is_empty() {
            labelled = search_output
                .results
                .iter()
                .map(|result| {
                    let mut result = result.clone();
                    if search_output
                        .history
                        .iter()
                        .any(|entry| entry.fact_id == result.fact_id)
                    {
                        result.content = format!("{HISTORY_MARKER}{}", result.content);
                    }
                    result
                })
                .collect();
            &labelled
        } else {
            &search_output.results
        };
        let mut assembled = assemble_context_with_budget(
            results,
            input.include_metadata,
            &format_options,
            &input.budget,
            &compression_options,
            &SimpleTokenEstimator,
        );

        if matches!(
            compression_options.mode,
            crate::ContextCompressionMode::ProviderSummary
        ) && !assembled.skipped_items.is_empty()
        {
            let available_tokens = assembled.budget.available_tokens;
            let summary_budget = crate::effective_summary_budget(
                available_tokens,
                assembled.budget.used_tokens,
                compression_options.summary_max_tokens,
            );

            if summary_budget > 0 {
                let (compressed, effective_mode) = crate::summarize_skipped_memories(
                    compression_options.mode,
                    self.llm_provider.clone(),
                    &tenant,
                    &assembled.skipped_items,
                    summary_budget,
                    format_options.format,
                    compression_options.include_summary_section,
                )
                .await;

                if !compressed.text.is_empty() {
                    assembled = apply_provider_compression_summary(
                        assembled,
                        compressed,
                        format_options.format,
                        available_tokens,
                        effective_mode,
                        &SimpleTokenEstimator,
                    );
                }
            }
        }

        Ok(BuildContextOutput {
            context: assembled.context,
            memories: assembled.memories,
            budget: assembled.budget,
            compression: assembled.compression,
            cache: ContextCacheUsage::disabled(),
        })
    }

    pub async fn list_memories(
        &self,
        input: ListMemoriesInput,
    ) -> MemcoreResult<ListMemoriesOutput> {
        validate_tenant(&input.tenant)?;
        let limit = normalize_list_limit(input.limit)?;
        let cursor = parse_optional_cursor(input.cursor)?;

        let memory_types = input.memory_type.map(|memory_type| vec![memory_type]);

        let memories = self
            .fact_store
            .search_facts(FactSearchQuery {
                tenant: input.tenant,
                memory_types,
                query_text: input.query_text,
                limit,
                cursor,
                include_deleted: input.include_deleted,
            })
            .await?;

        let page = build_page(memories, limit, |fact| PageCursor {
            last_id: fact.id.to_string(),
            last_sort_value: fact.updated_at,
        })?;

        Ok(ListMemoriesOutput {
            memories: page.items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn delete_memory(
        &self,
        input: DeleteMemoryInput,
    ) -> MemcoreResult<DeleteMemoryOutput> {
        validate_tenant(&input.tenant)?;

        // The delete is re-drivable. The tenant-scoped, deleted-inclusive
        // read finds the row whether or not an earlier attempt already soft-deleted it; another
        // tenant's id and an unknown id are both `None`, so both get the same 404.
        let Some(state) = self
            .fact_store
            .get_fact_including_deleted(&input.tenant, input.memory_id)
            .await?
        else {
            return Err(memcore_common::MemcoreError::NotFound(
                "memory not found".to_string(),
            ));
        };

        if !state.deleted {
            match self
                .fact_store
                .soft_delete_fact(&input.tenant, input.memory_id)
                .await
            {
                Ok(()) => {
                    // The one logical delete event, recorded when the logical delete happens, so
                    // a later cleanup failure leaves it audited and a re-driven retry adds none.
                    record_event_best_effort(
                        &self.event_store,
                        &input.tenant,
                        build_delete_event(
                            &input.tenant,
                            &state.fact,
                            &self.audit_provider_name,
                            &self.audit_model_name,
                            serde_json::json!({ "source": "delete_memory" }),
                        ),
                    )
                    .await;
                }
                // A concurrent delete won the soft delete (the SQL stores update only live rows):
                // it records the event; this request completes the cleanup.
                Err(memcore_common::MemcoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
            // The row is now invisible: no cached context may keep serving it, even if the
            // cleanup below fails.
            self.invalidate_user_context_cache(&input.tenant).await;
        }

        // Cleanup, also on a retry of an already soft-deleted row. A fact without a point (or
        // whose point an earlier attempt removed) is already clean: NotFound is completion.
        // The row is soft-deleted; a failed cleanup leaves its point or entity data.
        match self
            .vector_store
            .delete_by_fact_id(&input.tenant, input.memory_id)
            .await
        {
            Ok(()) | Err(memcore_common::MemcoreError::NotFound(_)) => {}
            Err(error) => {
                record_write_failure("fact_delete");
                return Err(error);
            }
        }

        // The fact's entity links and relations go with it, and so does any entity left
        // without a link, whether entities are on or off (idempotent on a retry).
        counted_write(
            "fact_delete",
            self.delete_entity_data_for_facts(&input.tenant, &[input.memory_id])
                .await,
        )?;

        self.invalidate_user_context_cache(&input.tenant).await;

        Ok(DeleteMemoryOutput { deleted: true })
    }

    /// The retry heal of the SQL exact-dedup NOOP. A healthy fact has exactly
    /// one point carrying its row's text and memory type. Otherwise (no point after a failed ADD
    /// vector write, a stale point after a failed UPDATE vector write, or several points) the row
    /// text is embedded with the configured provider (the same non-empty / finite check as
    /// updates) and written with `Replace`, which reuses the fact's existing point id when it has
    /// one (Qdrant) and never re-keys a point; then the result is verified once. Any failure is
    /// returned, so the NOOP is never acknowledged over a vector that is still missing or stale.
    /// No SQL row changes and no event is recorded. A store that cannot report points (`None`)
    /// keeps the previous behaviour; a fact deleted meanwhile needs no vector.
    async fn heal_fact_vector(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        let Some(fact) = self.fact_store.get_fact(tenant, fact_id).await? else {
            return Ok(());
        };
        let healthy = |points: &[crate::ports::VectorPointState]| {
            points.len() == 1
                && points[0].content == fact.content
                && points[0].memory_type == fact.memory_type
        };
        let Some(points) = self
            .vector_store
            .fact_vector_points(tenant, fact_id)
            .await?
        else {
            return Ok(());
        };
        if healthy(&points) {
            return Ok(());
        }
        // The window is open (missing, stale or extra points); a failed repair
        // leaves it open.
        let repaired = async {
            let embedding =
                checked_embedding_from(self.embedding_provider.as_ref(), &fact.content).await?;
            self.vector_store
                .write_vector(
                    tenant,
                    vector_record_from_fact(&fact, embedding),
                    VectorWriteIntent::Replace,
                )
                .await?;
            match self
                .vector_store
                .fact_vector_points(tenant, fact_id)
                .await?
            {
                Some(points) if healthy(&points) => Ok(()),
                None => Ok(()),
                Some(_) => Err(memcore_common::MemcoreError::Conflict(
                    "the memory's vector could not be verified after repair; retry the request"
                        .to_string(),
                )),
            }
        }
        .await;
        counted_write("fact_heal", repaired)
    }

    pub async fn apply_retention(
        &self,
        input: ApplyRetentionInput,
    ) -> MemcoreResult<ApplyRetentionOutput> {
        validate_tenant(&input.tenant)?;

        if !input.policy.enabled {
            return Ok(ApplyRetentionOutput::zero(input.dry_run));
        }

        let mut output = ApplyRetentionOutput::zero(input.dry_run);

        if let Some(fact_days) = input.policy.fact_days_active() {
            let cutoff = Utc::now() - Duration::days(i64::from(fact_days));
            let result = self
                .fact_store
                .delete_facts_older_than(&input.tenant, cutoff, input.dry_run)
                .await;
            // Invalidate right after the prune (also when it failed, since it may
            // have committed), before the cleanup below can fail and return.
            if !input.dry_run && !matches!(&result, Ok(pruned) if pruned.count == 0) {
                self.invalidate_user_context_cache(&input.tenant).await;
            }
            let result = result?;

            output.facts_matched = result.count;
            if input.dry_run {
                output.facts_deleted = 0;
            } else {
                output.facts_deleted = result.count;
                // Entity links and relations of pruned facts go with them, and so does every
                // entity left without a link.
                // The rows are pruned; a failed cleanup leaves their entity data or
                // points.
                counted_write(
                    "retention",
                    self.delete_entity_data_for_facts(&input.tenant, &result.fact_ids)
                        .await,
                )?;
                for fact_id in result.fact_ids {
                    if let Err(err) = self
                        .vector_store
                        .delete_by_fact_id(&input.tenant, fact_id)
                        .await
                        && !matches!(err, memcore_common::MemcoreError::NotFound(_))
                    {
                        record_write_failure("retention");
                        return Err(err);
                    }
                }
            }

            // Episodes follow the fact retention window, measured from when they were stored. With
            // episodes off, persisted episodes are still pruned; the counts are reported
            // then only when something matched, so the response is otherwise unchanged.
            if let Some(episodes) = self.episode_data() {
                let ids = episodes
                    .store
                    .delete_episodes_recorded_before(&input.tenant, cutoff, input.dry_run)
                    .await?;
                if self.episodes.is_some() || !ids.is_empty() {
                    output.episodes_matched = Some(ids.len());
                    output.episodes_deleted = Some(if input.dry_run { 0 } else { ids.len() });
                }
                if !input.dry_run {
                    for id in ids {
                        if let Err(err) =
                            episodes.vectors.delete_by_fact_id(&input.tenant, id).await
                            && !matches!(err, memcore_common::MemcoreError::NotFound(_))
                        {
                            record_write_failure("retention");
                            return Err(err);
                        }
                    }
                }
            }
        }

        if let Some(event_days) = input.policy.event_days_active()
            && let Some(event_store) = &self.event_store
        {
            let cutoff = Utc::now() - Duration::days(i64::from(event_days));
            let count = event_store
                .delete_events_older_than(&input.tenant, cutoff, input.dry_run)
                .await?;

            output.events_matched = count;
            output.events_deleted = if input.dry_run { 0 } else { count };
        }

        if !input.dry_run && output.facts_deleted > 0 {
            self.invalidate_user_context_cache(&input.tenant).await;
        }

        Ok(output)
    }

    pub async fn apply_provider_usage_retention(
        &self,
        input: ApplyProviderUsageRetentionInput,
    ) -> MemcoreResult<ApplyProviderUsageRetentionOutput> {
        validate_org_id(&input.org_id)?;

        if input.retention_days == 0 {
            return Ok(ApplyProviderUsageRetentionOutput::zero(
                input.dry_run,
                Utc::now(),
            ));
        }

        let store = self.provider_usage_store.as_ref().ok_or_else(|| {
            memcore_common::MemcoreError::ValidationError(
                "provider usage persistence is not configured".to_string(),
            )
        })?;

        let cutoff = Utc::now() - Duration::days(i64::from(input.retention_days));
        let matched = store
            .delete_usage_events_older_than(&input.org_id, cutoff, true)
            .await?;

        let deleted = if input.dry_run {
            0
        } else {
            store
                .delete_usage_events_older_than(&input.org_id, cutoff, false)
                .await?
        };

        Ok(ApplyProviderUsageRetentionOutput {
            dry_run: input.dry_run,
            matched_events: matched,
            deleted_events: deleted,
            cutoff,
        })
    }

    pub async fn get_org_summary(&self, input: OrgSummaryInput) -> MemcoreResult<OrgSummaryOutput> {
        validate_org_id(&input.org_id)?;

        let total_facts = self.fact_store.count_facts_by_org(&input.org_id).await?;
        let total_users = self.fact_store.count_users_by_org(&input.org_id).await?;
        let total_events = match &self.event_store {
            Some(store) => Some(store.count_events_by_org(&input.org_id).await?),
            None => None,
        };

        let total_episodes = match &self.episodes {
            Some(episodes) => Some(episodes.store.count_episodes_by_org(&input.org_id).await?),
            None => None,
        };

        Ok(OrgSummaryOutput {
            org_id: input.org_id,
            total_users,
            total_facts,
            total_events,
            total_episodes,
        })
    }

    pub async fn list_org_users(
        &self,
        input: ListOrgUsersInput,
    ) -> MemcoreResult<ListOrgUsersOutput> {
        validate_org_id(&input.org_id)?;
        let limit = normalize_org_users_limit(input.limit)?;
        let cursor = parse_optional_cursor(input.cursor)?;

        let users = self
            .fact_store
            .list_users_by_org(OrgUserListQuery {
                org_id: input.org_id,
                limit,
                cursor,
            })
            .await?;

        let page = build_page(users, limit, |user| PageCursor {
            last_id: user.user_id.clone(),
            last_sort_value: user.last_memory_at.unwrap_or_else(Utc::now),
        })?;

        Ok(ListOrgUsersOutput {
            users: page.items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn search_org_memory_events(
        &self,
        input: SearchOrgMemoryEventsInput,
    ) -> MemcoreResult<SearchOrgMemoryEventsOutput> {
        validate_org_id(&input.org_id)?;
        let limit = normalize_org_memory_events_limit(input.limit)?;
        let cursor = parse_optional_cursor(input.cursor)?;

        let Some(event_store) = &self.event_store else {
            return Ok(SearchOrgMemoryEventsOutput {
                events: Vec::new(),
                next_cursor: None,
            });
        };

        validate_event_date_range(input.created_after, input.created_before)?;

        let mut query = OrgMemoryEventQuery::new(input.org_id, limit);
        query.user_id = input.user_id;
        query.fact_id = input.fact_id;
        query.operation = input.operation;
        query.created_after = input.created_after;
        query.created_before = input.created_before;
        query.query_text = input.query_text;
        query.cursor = cursor;

        let events = event_store.list_events_by_org(query).await?;

        let page = build_page(events, limit, |event| PageCursor {
            last_id: event.id.to_string(),
            last_sort_value: event.created_at,
        })?;

        Ok(SearchOrgMemoryEventsOutput {
            events: page.items,
            next_cursor: page.next_cursor,
        })
    }

    pub async fn get_org_quota_status(
        &self,
        input: GetOrgQuotaStatusInput,
    ) -> MemcoreResult<QuotaCheckResult> {
        validate_org_id(&input.org_id)?;
        if let Some(user_id) = &input.user_id {
            TenantContext::new(input.org_id.clone(), user_id.clone())?;
        }

        self.quota_service().get_org_quota_status(input).await
    }

    pub async fn get_org_usage_dashboard(
        &self,
        input: OrgUsageDashboardInput,
    ) -> MemcoreResult<OrgUsageDashboardOutput> {
        validate_org_id(&input.org_id)?;
        validate_event_date_range(Some(input.created_after), Some(input.created_before))?;

        let plan = match &self.org_plan_store {
            Some(store) => store.get_org_plan(&input.org_id).await?,
            None => None,
        };
        let quota = self
            .get_org_quota_status(GetOrgQuotaStatusInput {
                org_id: input.org_id.clone(),
                user_id: None,
            })
            .await?;

        // Total = stored non-deleted rows (capacity, what quotas count); active = active
        // rows (no `invalid_at`, not superseded).
        let total_memories = self.fact_store.count_facts_by_org(&input.org_id).await? as u64;
        let active_memories = self
            .fact_store
            .count_active_facts_by_org(&input.org_id)
            .await? as u64;
        let latest_snapshot = self.latest_memory_usage_snapshot(&input.org_id).await?;
        let memory = OrgMemoryUsageSummary {
            total_users: self.fact_store.count_users_by_org(&input.org_id).await? as u64,
            total_memories,
            active_memories,
            deleted_memories: None,
            latest_snapshot,
        };

        let provider = match &self.provider_usage_store {
            Some(store) => {
                let result = store
                    .query_usage(ProviderUsageQuery {
                        org_id: input.org_id.clone(),
                        user_id: None,
                        provider_name: None,
                        model_name: None,
                        capability: None,
                        operation_name: None,
                        created_after: Some(input.created_after),
                        created_before: Some(input.created_before),
                        limit: DEFAULT_PROVIDER_USAGE_LIMIT,
                        cursor: None,
                    })
                    .await?;
                result.summary.into()
            }
            None => empty_provider_usage_summary(),
        };

        Ok(OrgUsageDashboardOutput {
            org_id: input.org_id,
            generated_at: Utc::now(),
            window_start: input.created_after,
            window_end: input.created_before,
            plan,
            quota,
            memory,
            provider,
        })
    }

    pub async fn create_memory_usage_snapshot(
        &self,
        input: CreateMemoryUsageSnapshotInput,
    ) -> MemcoreResult<CreateMemoryUsageSnapshotOutput> {
        validate_org_id(&input.org_id)?;
        let store = self.memory_usage_snapshot_store()?;

        // Total = stored non-deleted rows; active = active rows.
        let total_memories = self.fact_store.count_facts_by_org(&input.org_id).await? as u64;
        let active_memories = self
            .fact_store
            .count_active_facts_by_org(&input.org_id)
            .await? as u64;
        let snapshot = MemoryUsageSnapshot {
            id: Uuid::new_v4(),
            org_id: input.org_id.clone(),
            total_users: self.fact_store.count_users_by_org(&input.org_id).await? as u64,
            total_memories,
            active_memories,
            deleted_memories: None,
            captured_at: input.captured_at.unwrap_or_else(Utc::now),
            metadata: None,
        };

        let snapshot = store.insert_snapshot(snapshot).await?;
        Ok(CreateMemoryUsageSnapshotOutput { snapshot })
    }

    pub async fn query_memory_usage_snapshots(
        &self,
        input: QueryMemoryUsageSnapshotsInput,
    ) -> MemcoreResult<QueryMemoryUsageSnapshotsOutput> {
        validate_org_id(&input.org_id)?;
        validate_event_date_range(input.created_after, input.created_before)?;
        let limit = validate_memory_usage_snapshot_limit(input.limit)?;
        let store = self.memory_usage_snapshot_store()?;

        let result = store
            .query_snapshots(MemoryUsageSnapshotQuery {
                org_id: input.org_id,
                created_after: input.created_after,
                created_before: input.created_before,
                limit,
                cursor: input.cursor,
            })
            .await?;

        Ok(QueryMemoryUsageSnapshotsOutput {
            snapshots: result.snapshots,
            next_cursor: result.next_cursor,
        })
    }

    async fn latest_memory_usage_snapshot(
        &self,
        org_id: &str,
    ) -> MemcoreResult<Option<MemoryUsageLatestSnapshot>> {
        let Some(store) = &self.memory_usage_snapshot_store else {
            return Ok(None);
        };

        let result = store
            .query_snapshots(MemoryUsageSnapshotQuery {
                org_id: org_id.to_string(),
                created_after: None,
                created_before: None,
                limit: 1,
                cursor: None,
            })
            .await?;

        Ok(result
            .snapshots
            .first()
            .map(MemoryUsageLatestSnapshot::from))
    }

    pub async fn get_provider_usage_daily(
        &self,
        input: ProviderUsageDailyInput,
    ) -> MemcoreResult<ProviderUsageDailyOutput> {
        validate_org_id(&input.org_id)?;
        validate_event_date_range(Some(input.created_after), Some(input.created_before))?;

        let buckets = match &self.provider_usage_store {
            Some(store) => {
                let mut query = ProviderUsageDailyQuery::new(
                    input.org_id.clone(),
                    input.created_after,
                    input.created_before,
                );
                query.provider_name = input.provider_name;
                query.model_name = input.model_name;
                query.capability = input.capability;
                store.query_usage_daily(query).await?
            }
            None => Vec::new(),
        };

        Ok(ProviderUsageDailyOutput {
            org_id: input.org_id,
            window_start: input.created_after,
            window_end: input.created_before,
            buckets,
        })
    }

    pub async fn resolve_org_quota_limits(
        &self,
        org_id: &str,
    ) -> MemcoreResult<ResolvedOrgQuotaLimits> {
        validate_org_id(org_id)?;
        self.quota_service().resolve_org_quota_limits(org_id).await
    }

    pub async fn check_memory_write_quota(
        &self,
        input: CheckMemoryWriteQuotaInput,
    ) -> MemcoreResult<QuotaCheckResult> {
        let tenant = TenantContext::new(input.org_id.clone(), input.user_id.clone())?;
        validate_tenant(&tenant)?;
        self.quota_service().check_memory_write_allowed(input).await
    }

    pub async fn check_provider_quota(
        &self,
        input: CheckProviderQuotaInput,
    ) -> MemcoreResult<QuotaCheckResult> {
        validate_org_id(&input.org_id)?;
        self.quota_service()
            .check_provider_call_allowed(input)
            .await
    }

    fn quota_service(&self) -> QuotaService {
        QuotaService::new(
            self.fact_store.clone(),
            self.provider_usage_store.clone(),
            self.org_plan_store.clone(),
            self.global_quota_limits.clone(),
        )
    }

    pub async fn forget_user(&self, input: ForgetUserInput) -> MemcoreResult<ForgetUserOutput> {
        validate_tenant(&input.tenant)?;

        let deleted = self.fact_store.delete_user_data(&input.tenant).await;
        // The cached contexts go as soon as the fact deletion was attempted (a
        // failed call may still have committed), before any later step can fail and return, so no
        // context read before the deletion stays servable. The final invalidation stays.
        self.invalidate_user_context_cache(&input.tenant).await;
        deleted?;

        // The fact rows are gone; a later failure leaves points, episodes, entities
        // or audit content of the user (a retry completes the erasure).
        let erased = async {
            // Every SQL row of the user goes before any vector,
            // so a failed vector deletion leaves only points, which no read serves (every read is
            // gated on SQL rows), and nothing of the user stays searchable until the retry. Note:
            // persisted episode and entity data is deleted whether the features are on or off.
            let episodes = self.episode_data();
            if let Some(episodes) = &episodes {
                episodes.store.delete_user_episodes(&input.tenant).await?;
            }
            if let Some(store) = self.entity_data_store() {
                store.delete_user_entities(&input.tenant).await?;
            }
            // Journal rows of the user's in-flight writes (before-images hold fact
            // text).
            if let Some(journal) = &self.ingest_journal {
                journal.store.delete_user_operations(&input.tenant).await?;
            }
            self.vector_store.delete_by_user(&input.tenant).await?;
            if let Some(episodes) = &episodes {
                episodes.vectors.delete_by_user(&input.tenant).await?;
            }
            // The user's audit events carry fact text; erase them (exact org +
            // user; content-free tombstones kept) and verify, or fail: never report a successful
            // erasure while content-bearing events remain. Retrying is safe.
            if let Some(events) = &self.event_store {
                events.erase_user_audit_content(&input.tenant).await?;
                let remaining = events.count_user_audit_content(&input.tenant).await?;
                if remaining > 0 {
                    return Err(memcore_common::MemcoreError::StorageError(format!(
                        "forget-user incomplete: {remaining} audit events with content remain; retry"
                    )));
                }
            }
            Ok(())
        }
        .await;
        counted_write("forget_user", erased)?;

        record_event_best_effort(
            &self.event_store,
            &input.tenant,
            // Content-free tombstone: no provider or model is involved in a forget.
            build_forget_user_event(&input.tenant, &None, &None),
        )
        .await;

        self.invalidate_user_context_cache(&input.tenant).await;

        Ok(ForgetUserOutput { deleted: true })
    }

    pub async fn import_user_data(
        &self,
        input: ImportUserDataInput,
    ) -> MemcoreResult<ImportUserDataOutput> {
        validate_tenant(&input.tenant)?;

        let mut validation =
            collect_import_validation(&input.export, &input.tenant, input.restore_events);
        // A truncated export must never drive a replace import (checked before any write).
        collect_truncation_mode_validation(&input.export, input.mode, &mut validation);

        if !validation.valid {
            if input.dry_run {
                return Ok(dry_run_output(
                    &input,
                    validation,
                    self.event_store.is_some(),
                    self.episode_data().is_some(),
                    self.entity_data_store().is_some(),
                ));
            }
            return Err(memcore_common::MemcoreError::ValidationError(
                validation
                    .first_error_message()
                    .unwrap_or_else(|| "import validation failed".to_string()),
            ));
        }

        if input.dry_run {
            return Ok(dry_run_output(
                &input,
                validation,
                self.event_store.is_some(),
                self.episode_data().is_some(),
                self.entity_data_store().is_some(),
            ));
        }

        // The stored id of every exported fact is
        // planned before any write (in replace mode before the deletion), from this tenant's own
        // rows only. An id the tenant already holds (live or soft-deleted) is kept; any other id
        // is stored under `derived_import_fact_id`, which depends on the tenant and the source id
        // alone. Fact ids are global keys, so keeping a non-owned id would let other tenants' rows
        // change the outcome of this import; no id is chosen through a global lookup.
        let mut planned_ids = Vec::with_capacity(input.export.facts.len());
        for exported_fact in &input.export.facts {
            let owned = self
                .fact_store
                .get_fact_including_deleted(&input.tenant, exported_fact.id)
                .await?
                .is_some();
            planned_ids.push(if owned {
                exported_fact.id
            } else {
                derived_import_fact_id(&input.tenant, exported_fact.id)
            });
        }
        let mut distinct_ids = std::collections::HashSet::with_capacity(planned_ids.len());
        if !planned_ids.iter().all(|id| distinct_ids.insert(*id)) {
            return Err(memcore_common::MemcoreError::ValidationError(format!(
                "{DUPLICATE_FACT_ID}: two exported facts would be stored under the same id; nothing was imported"
            )));
        }

        // An append import is fail-closed against
        // re-application. Before the first write, every planned id is read in this tenant, deleted
        // rows included: a live row with identical content means this export (or its failed
        // partial run) is already applied (409 IMPORT_ALREADY_APPLIED, which takes precedence);
        // otherwise a soft-deleted row with the id would collide (409 IMPORT_ID_CONFLICT). A live
        // row with different content is re-keyed to a fresh id. Replace mode deletes first and is
        // the documented recovery. This is a read-only preflight, not a transaction: concurrent
        // imports of the same export are not serialised by it.
        let mut import_ids = planned_ids.clone();
        if matches!(input.mode, ImportMode::Append) {
            let mut deleted_id_collision = false;
            for ((exported_fact, planned), import_id) in input
                .export
                .facts
                .iter()
                .zip(&planned_ids)
                .zip(import_ids.iter_mut())
            {
                let Some(state) = self
                    .fact_store
                    .get_fact_including_deleted(&input.tenant, *planned)
                    .await?
                else {
                    continue;
                };
                if state.deleted {
                    deleted_id_collision = true;
                } else if state.fact.content == exported_fact.content {
                    return Err(memcore_common::MemcoreError::import_already_applied());
                } else {
                    *import_id = resolve_import_fact_id(*planned, true, input.mode);
                }
            }
            if deleted_id_collision {
                return Err(memcore_common::MemcoreError::import_id_conflict());
            }
        }

        // The write phase. An import is not a transaction, so a failure here can
        // leave the stores partly written; it is counted once under `import`.
        let written = async {
            let mut replaced_existing = false;

            if matches!(input.mode, ImportMode::Replace) {
                let deleted = self.fact_store.delete_user_data(&input.tenant).await;
                // The user's old data is (being) deleted: invalidate now, before
                // a later step of the replace can fail and return.
                self.invalidate_user_context_cache(&input.tenant).await;
                deleted?;
                self.vector_store.delete_by_user(&input.tenant).await?;
                self.delete_user_episode_data(&input.tenant).await?;
                if let Some(store) = self.entity_data_store() {
                    store.delete_user_entities(&input.tenant).await?;
                }
                replaced_existing = true;

                record_event_best_effort(
                    &self.event_store,
                    &input.tenant,
                    build_import_replace_event(
                        &input.tenant,
                        &self.audit_provider_name,
                        &self.audit_model_name,
                    ),
                )
                .await;
            }

            let mut imported_facts = 0usize;
            let skipped_facts = 0usize;

            // S3I: one map from exported to stored id for every reference to a fact of this import
            // (`superseded_by`, restored events, entity links and relations). References to facts
            // outside the import keep their exported value (entity links to them are dropped).
            let fact_ids: std::collections::HashMap<Uuid, Uuid> = input
                .export
                .facts
                .iter()
                .zip(&import_ids)
                .map(|(exported, id)| (exported.id, *id))
                .collect();
            for (exported_fact, fact_id) in input.export.facts.iter().zip(import_ids) {
                let mut fact = fact_for_import(exported_fact, &input.tenant, fact_id)?;
                if let Some(replacement) = fact.superseded_by
                    && let Some(stored) = fact_ids.get(&replacement)
                {
                    fact.superseded_by = Some(*stored);
                }

                self.fact_store
                    .insert_fact(&input.tenant, fact.clone())
                    .await?;

                let embedding = self.embedding_provider.embed_text(&fact.content).await?;

                let record = VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id: fact.id,
                    org_id: fact.org_id.clone(),
                    user_id: fact.user_id.clone(),
                    embedding,
                    content: fact.content.clone(),
                    memory_type: fact.memory_type,
                    metadata: fact.metadata.clone(),
                };

                self.vector_store
                    .upsert_vector(&input.tenant, record)
                    .await?;

                imported_facts += 1;
            }

            let mut imported_events = 0usize;

            if input.restore_events
                && let Some(event_store) = &self.event_store
            {
                for exported_event in &input.export.memory_events {
                    let event = restored_event_from_export(exported_event, &fact_ids);
                    event_store.record_event(&input.tenant, event).await?;
                    imported_events += 1;
                }
            }

            // Episodes in the payload are restored (rows and vectors, as with episodes on)
            // whether or not episodes are enabled; restoring them does not turn episode behaviour
            // on.
            let (imported_episodes, skipped_episodes) =
                match (&input.export.episodes, &self.episode_data()) {
                    (None, _) => (None, None),
                    (Some(exported), None) => (Some(0), Some(exported.len())),
                    (Some(exported), Some(episodes)) => {
                        // Re-inserted under this tenant with new IDs; client message IDs keep the
                        // insert idempotent, and eligible episodes are embedded again.
                        let now = Utc::now();
                        let fresh = exported
                            .iter()
                            .map(|episode| Episode {
                                id: Uuid::new_v4(),
                                org_id: input.tenant.org_id.clone(),
                                user_id: input.tenant.user_id.clone(),
                                recorded_at: now,
                                embedded: false,
                                ..episode.clone()
                            })
                            .collect();
                        let stored = self
                            .index_episodes(&input.tenant, episodes, fresh, false)
                            .await?;
                        (Some(stored), Some(0))
                    }
                };

            // Persisted entity data is restored as data whenever an entity store exists,
            // without turning entity behaviour on.
            let (imported_entities, skipped_entities) =
                match (&input.export.entities, self.entity_data_store()) {
                    (None, _) => (None, None),
                    (Some(exported), None) => (Some(0), Some(exported.entities.len())),
                    (Some(exported), Some(store)) => {
                        let imported = self
                            .import_entities(&input.tenant, store.as_ref(), exported, &fact_ids)
                            .await?;
                        (Some(imported), Some(0))
                    }
                };

            self.invalidate_user_context_cache(&input.tenant).await;

            Ok(ImportUserDataOutput {
                manifest_consistent: input.export.completeness.is_some(),
                imported_facts,
                imported_events,
                skipped_facts,
                replaced_existing,
                dry_run: false,
                validation,
                imported_episodes,
                skipped_episodes,
                imported_entities,
                skipped_entities,
            })
        }
        .await;
        counted_write("import", written)
    }

    pub async fn export_user_data(
        &self,
        input: ExportUserDataInput,
    ) -> MemcoreResult<UserMemoryExport> {
        validate_tenant(&input.tenant)?;
        let mut truncated = Vec::new();

        // Stores over-fetch one look-ahead row (`page_fetch_limit`); it only tells whether more
        // records exist than the documented per-export limit and is never exported itself.
        let facts = self
            .fact_store
            .search_facts(FactSearchQuery {
                tenant: input.tenant.clone(),
                memory_types: None,
                query_text: None,
                limit: EXPORT_FACTS_LIMIT,
                cursor: None,
                include_deleted: input.include_deleted,
            })
            .await?;
        let facts = cap_export_section(facts, EXPORT_FACTS_LIMIT, "facts", &mut truncated);

        let memory_events = if input.include_events {
            match &self.event_store {
                Some(event_store) => {
                    event_store
                        .list_events(MemoryEventQuery::new(
                            input.tenant.clone(),
                            EXPORT_EVENTS_LIMIT,
                        ))
                        .await?
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let memory_events = cap_export_section(
            memory_events,
            EXPORT_EVENTS_LIMIT,
            "memory_events",
            &mut truncated,
        );

        // Persisted episodes are exported whether or not episodes are enabled; with them off
        // a user without episodes keeps the unchanged export.
        let episodes = match self.episode_data() {
            Some(episodes) => {
                let rows = episodes
                    .store
                    .list_episodes(&input.tenant, EXPORT_EPISODES_LIMIT + 1)
                    .await?;
                let rows =
                    cap_export_section(rows, EXPORT_EPISODES_LIMIT, "episodes", &mut truncated);
                (self.episodes.is_some() || !rows.is_empty()).then_some(rows)
            }
            None => None,
        };

        // With entities off, persisted entity data is still exported (v3) when the user has
        // any; a user without entity data keeps the unchanged export.
        let entities = match self.entity_data_store() {
            Some(store) => {
                let exported = self.export_entities(&input.tenant, store.as_ref()).await?;
                (self.entities.is_some() || !exported.entities.is_empty()).then_some(exported)
            }
            None => None,
        };

        if !truncated.is_empty() {
            tracing::warn!(
                sections = ?truncated.iter().map(|t| t.section.as_str()).collect::<Vec<_>>(),
                "user export is incomplete: per-export limits reached"
            );
        }
        let export = UserMemoryExport::new(
            input.tenant.org_id,
            input.tenant.user_id,
            facts,
            memory_events,
        )
        .with_truncated(truncated);
        // Without episodes or entities the export stays exactly the v1 format.
        let export = match episodes {
            Some(episodes) => export.with_episodes(episodes),
            None => export,
        };
        Ok(match entities {
            Some(entities) => export.with_entities(entities),
            None => export,
        })
    }

    pub async fn list_memory_events(
        &self,
        input: ListMemoryEventsInput,
    ) -> MemcoreResult<ListMemoryEventsOutput> {
        validate_tenant(&input.tenant)?;
        let limit = normalize_memory_event_list_limit(input.limit)?;
        let cursor = parse_optional_cursor(input.cursor)?;

        let Some(event_store) = &self.event_store else {
            return Ok(ListMemoryEventsOutput {
                events: Vec::new(),
                next_cursor: None,
            });
        };

        validate_event_date_range(input.created_after, input.created_before)?;

        let mut query = MemoryEventQuery::new(input.tenant, limit);
        query.fact_id = input.fact_id;
        query.operation = input.operation;
        query.created_after = input.created_after;
        query.created_before = input.created_before;
        query.query_text = input.query_text;
        query.cursor = cursor;

        let events = event_store.list_events(query).await?;

        let page = build_page(events, limit, |event| PageCursor {
            last_id: event.id.to_string(),
            last_sort_value: event.created_at,
        })?;

        Ok(ListMemoryEventsOutput {
            events: page.items,
            next_cursor: page.next_cursor,
        })
    }

    async fn invalidate_user_context_cache(&self, tenant: &TenantContext) {
        if self.context_cache_config.enabled {
            // Through the coordinator, which also bumps the user's generation so
            // an in-flight computation cannot store a context read before this invalidation.
            match self
                .context_cache_coordinator
                .invalidate_user(&tenant.org_id, &tenant.user_id)
                .await
            {
                Ok(removed) => {
                    self.context_cache_metrics.record_invalidation(removed);
                    tracing::debug!(
                        event = "context_cache_invalidated",
                        org_id = %tenant.org_id,
                        user_id = %tenant.user_id,
                        removed_entries = removed,
                        "context cache invalidated for user"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        event = "context_cache_invalidation_failed",
                        org_id = %tenant.org_id,
                        user_id = %tenant.user_id,
                        error_code = %error.code(),
                        "context cache invalidation failed"
                    );
                }
            }
        }
    }

    fn memory_usage_snapshot_store(&self) -> MemcoreResult<&Arc<dyn MemoryUsageSnapshotStore>> {
        self.memory_usage_snapshot_store.as_ref().ok_or_else(|| {
            memcore_common::MemcoreError::StorageError(
                "memory usage snapshot store is not configured".to_string(),
            )
        })
    }
}

fn messages_for_llm_extraction(
    messages: &[MemoryMessage],
    enable_pii_redaction: bool,
) -> Vec<MemoryMessage> {
    if enable_pii_redaction {
        redact_messages_for_extraction(messages)
    } else {
        messages.to_vec()
    }
}

fn validate_tenant(tenant: &TenantContext) -> MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if tenant.org_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "org_id cannot be empty".to_string(),
        ));
    }
    if tenant.user_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "user_id cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_org_id(org_id: &str) -> MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if org_id.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "org_id cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn normalize_org_users_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    let normalized = if limit == 0 {
        DEFAULT_LIST_ORG_USERS_LIMIT
    } else {
        limit
    };

    if normalized > MAX_LIST_ORG_USERS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_LIST_ORG_USERS_LIMIT}"
        )));
    }

    Ok(normalized)
}

fn normalize_org_memory_events_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if limit > MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {MAX_SEARCH_ORG_MEMORY_EVENTS_LIMIT}"
        )));
    }

    Ok(limit)
}

fn validate_query(query: &str) -> MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if query.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "query cannot be empty".to_string(),
        ));
    }
    Ok(())
}

fn normalize_context_max_memories(max_memories: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if max_memories == 0 {
        return Err(MemcoreError::ValidationError(
            "max_memories must be greater than 0".to_string(),
        ));
    }

    if max_memories > crate::MAX_CONTEXT_MAX_MEMORIES {
        return Err(MemcoreError::ValidationError(format!(
            "max_memories cannot exceed {}",
            crate::MAX_CONTEXT_MAX_MEMORIES
        )));
    }

    Ok(max_memories)
}

fn normalize_list_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if limit > types::MAX_LIST_MEMORIES_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {}",
            types::MAX_LIST_MEMORIES_LIMIT
        )));
    }

    Ok(limit)
}

/// Stable order for vector hits with exactly equal scores: the fact's content (or the hit's,
/// if the fact is missing), its source message ids, its subject, and last the fact id, which only
/// separates facts identical in all three.
fn vector_tie_key<'a>(
    hit: &'a crate::ports::VectorSearchResult,
    facts: &'a std::collections::HashMap<Uuid, Fact>,
) -> (&'a str, &'a [String], Option<&'a str>, Uuid) {
    match facts.get(&hit.fact_id) {
        Some(fact) => (
            fact.content.as_str(),
            fact.structure.source_message_ids.as_slice(),
            fact.structure.subject.as_deref(),
            hit.fact_id,
        ),
        None => (hit.content.as_str(), &[], None, hit.fact_id),
    }
}

/// Stable tie order for exactly tied episode scores: stable fields of the message first; the
/// episode ID only separates rows identical in all of them.
fn episode_tie_key(episode: &Episode) -> (&str, Option<&str>, Option<&str>, Uuid) {
    (
        episode.content.as_str(),
        episode.message_id.as_deref(),
        episode.session_id.as_deref(),
        episode.id,
    )
}

fn normalize_search_limit(limit: usize, max_limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if limit > max_limit {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {max_limit}"
        )));
    }

    Ok(limit)
}

/// Per-source retrieval limit before fusion. Up to `MAX_SEARCH_LIMIT` it is twice the final limit,
/// capped at `MAX_SEARCH_LIMIT`, exactly as before. Above it (only possible when an operator
/// raised `MEMCORE_SEARCH_MAX_LIMIT`) each leg fetches exactly `limit` candidates.
fn internal_search_limit(limit: usize) -> usize {
    if limit <= types::MAX_SEARCH_LIMIT {
        limit.saturating_mul(2).min(types::MAX_SEARCH_LIMIT)
    } else {
        limit
    }
}

fn normalize_memory_event_list_limit(limit: usize) -> MemcoreResult<usize> {
    use memcore_common::MemcoreError;

    if limit == 0 {
        return Err(MemcoreError::ValidationError(
            "limit must be greater than 0".to_string(),
        ));
    }

    if limit > types::MAX_LIST_MEMORY_EVENTS_LIMIT {
        return Err(MemcoreError::ValidationError(format!(
            "limit cannot exceed {}",
            types::MAX_LIST_MEMORY_EVENTS_LIMIT
        )));
    }

    Ok(limit)
}

fn validate_messages(messages: &[crate::ports::MemoryMessage]) -> MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if messages.is_empty() {
        return Err(MemcoreError::ValidationError(
            "messages cannot be empty".to_string(),
        ));
    }

    for message in messages {
        if message.content.trim().is_empty() {
            return Err(MemcoreError::ValidationError(
                "message content cannot be empty".to_string(),
            ));
        }
    }

    Ok(())
}

fn validate_candidate(candidate: &crate::CandidateFact) -> MemcoreResult<()> {
    use memcore_common::MemcoreError;

    if candidate.content.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "candidate fact content cannot be empty".to_string(),
        ));
    }

    if !(0.0..=1.0).contains(&candidate.confidence) {
        return Err(MemcoreError::ValidationError(
            "candidate confidence must be between 0.0 and 1.0".to_string(),
        ));
    }

    if !(0.0..=1.0).contains(&candidate.importance) {
        return Err(MemcoreError::ValidationError(
            "candidate importance must be between 0.0 and 1.0".to_string(),
        ));
    }

    Ok(())
}

/// Event metadata for a consolidation v2 add or update: the request metadata, with the decision's
/// audit fields under `consolidation`.
fn with_consolidation_audit(
    request: &serde_json::Value,
    audit: &serde_json::Value,
) -> serde_json::Value {
    let mut metadata = match request {
        serde_json::Value::Object(map) => serde_json::Value::Object(map.clone()),
        serde_json::Value::Null => serde_json::json!({}),
        other => serde_json::json!({ "request_metadata": other }),
    };
    metadata["consolidation"] = audit.clone();
    metadata
}

impl MemoryEngine {
    /// An ADD that lost to an equivalent fact already stored (the store's exact-dedup
    /// key; typically a concurrent request committed first). Counted and audited like the
    /// exact-duplicate NOOP, but WITHOUT the R4 vector heal: the winning request owns its in-flight
    /// vector write, and healing here could race it into a second point. If that write fails, the
    /// winner's request fails and an identical retry heals (R4) or `verify-vectors` reports it.
    async fn record_concurrent_duplicate(
        &self,
        tenant: &TenantContext,
        existing_fact_id: Uuid,
        summary: &mut MemoryOperationSummary,
    ) {
        summary.discard(FactDiscardReason::DuplicateContent);
        record_event_best_effort(
            &self.event_store,
            tenant,
            build_noop_event(
                tenant,
                &dedup_noop_decision(
                    existing_fact_id,
                    "exact normalized content match (store dedup key; concurrent addition)"
                        .to_string(),
                ),
                &self.audit_provider_name,
                &self.audit_model_name,
            ),
        )
        .await;
    }
}

/// The decision used when S1 skips the classification call: an Add with no target.
fn no_related_add_decision() -> FactOperationDecision {
    FactOperationDecision {
        operation: FactOperation::Add,
        target_fact_id: None,
        reason: Some("no related facts".to_string()),
        confidence: 1.0,
    }
}

fn dedup_noop_decision(existing_fact_id: Uuid, reason: String) -> FactOperationDecision {
    FactOperationDecision {
        operation: FactOperation::NoOp,
        target_fact_id: Some(existing_fact_id),
        reason: Some(reason),
        confidence: 1.0,
    }
}

/// The exported fact under `tenant` and `fact_id`, keeping every stored field the export carries:
/// the structure (subject, event time with its granularity and anchor, time expression,
/// `observed_at`, session and source message IDs), the validity and supersession times, and the
/// replacement link. Older exports without these fields import with them unset.
fn fact_for_import(exported: &Fact, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Fact> {
    Fact::new(
        fact_id,
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        exported.memory_type,
        exported.content.clone(),
        exported.summary.clone(),
        exported.source,
        exported.confidence,
        exported.importance,
        exported.valid_at,
        exported.invalid_at,
        exported.recorded_at,
        exported.updated_at,
        exported.metadata.clone(),
    )
    .map(|fact| Fact {
        structure: exported.structure.clone(),
        superseded_at: exported.superseded_at,
        superseded_by: exported.superseded_by,
        ..fact
    })
}

/// A restored event gets a new id; its `fact_id` follows a fact of this import to the fact's stored
/// id (S3I) and keeps the exported value otherwise.
fn restored_event_from_export(
    exported: &MemoryEvent,
    fact_ids: &std::collections::HashMap<Uuid, Uuid>,
) -> MemoryEvent {
    MemoryEvent {
        id: Uuid::new_v4(),
        org_id: exported.org_id.clone(),
        user_id: exported.user_id.clone(),
        fact_id: exported
            .fact_id
            .map(|id| fact_ids.get(&id).copied().unwrap_or(id)),
        operation: exported.operation,
        input_text: None,
        previous_content: exported.previous_content.clone(),
        new_content: exported.new_content.clone(),
        provider_name: exported.provider_name.clone(),
        model_name: exported.model_name.clone(),
        metadata: exported.metadata.clone(),
        created_at: exported.created_at,
    }
}

fn dry_run_output(
    input: &ImportUserDataInput,
    validation: ImportValidationSummary,
    event_store_configured: bool,
    episodes_enabled: bool,
    entities_enabled: bool,
) -> ImportUserDataOutput {
    let replaced_existing = matches!(input.mode, ImportMode::Replace);
    let exported_episodes = input.export.episodes.as_ref().map(Vec::len);
    let exported_entities = input
        .export
        .entities
        .as_ref()
        .map(|entities| entities.entities.len());

    if !validation.valid {
        return ImportUserDataOutput {
            manifest_consistent: false,
            imported_facts: 0,
            imported_events: 0,
            skipped_facts: input.export.facts.len(),
            replaced_existing,
            dry_run: true,
            validation,
            imported_episodes: exported_episodes.map(|_| 0),
            skipped_episodes: exported_episodes,
            imported_entities: exported_entities.map(|_| 0),
            skipped_entities: exported_entities,
        };
    }

    let imported_events = if input.restore_events && event_store_configured {
        input.export.memory_events.len()
    } else {
        0
    };

    ImportUserDataOutput {
        manifest_consistent: input.export.completeness.is_some(),
        imported_facts: input.export.facts.len(),
        imported_events,
        skipped_facts: 0,
        replaced_existing,
        dry_run: true,
        validation,
        imported_episodes: exported_episodes.map(|count| if episodes_enabled { count } else { 0 }),
        skipped_episodes: exported_episodes.map(|count| if episodes_enabled { 0 } else { count }),
        imported_entities: exported_entities.map(|count| if entities_enabled { count } else { 0 }),
        skipped_entities: exported_entities.map(|count| if entities_enabled { 0 } else { count }),
    }
}

/// Trims an export section to its documented per-export limit and records the truncation when
/// the store returned more rows than the limit (its look-ahead row included).
fn cap_export_section<T>(
    mut rows: Vec<T>,
    limit: usize,
    section: &str,
    truncated: &mut Vec<crate::ExportSectionTruncation>,
) -> Vec<T> {
    if rows.len() > limit {
        rows.truncate(limit);
        truncated.push(crate::ExportSectionTruncation {
            section: section.to_string(),
            limit,
        });
    }
    rows
}
