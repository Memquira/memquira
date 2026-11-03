//! The E-merge list, built inside memcore, and the optional
//! transformations, each applied only when its flag is on.

use memcore_common::{MemcoreError, MemcoreResult};

use super::{MemoryEngine, RetrievalMode, SearchEpisodesInput, SearchMemoryInput, SearchOptions};
use crate::TenantContext;
use crate::analyze;
use crate::candidates::rerank::rerank;
use crate::candidates::temporal::apply_temporal_ranking;
use crate::candidates::{
    Candidate, MultiQueryMode, SearchCandidatesInput, SearchCandidatesOutput,
    merge_by_vector_score, merge_multi_query,
};

use crate::d76_context::{
    D76_CANDIDATE_DEPTH, D76_MAX_TOKEN_BUDGET, D76ContextInput, D76ContextOutput, D76Line,
    D76PackedContextOutput, D76Packing, render_evidence, render_packed,
};

impl MemoryEngine {
    /// One user's candidate list. Baseline: the semantic memory search (`fact_limit`) and the
    /// episode search (`episode_limit`), each exactly as its own endpoint runs it, merged by raw
    /// vector similarity. Then, in order and only when enabled: query analysis, deterministic
    /// multi-query, temporal ranking and reranking. Requires episodes.
    pub async fn search_candidates(
        &self,
        input: SearchCandidatesInput,
    ) -> MemcoreResult<SearchCandidatesOutput> {
        if self.episodes.is_none() {
            return Err(MemcoreError::ValidationError(
                "episodes are not enabled (MEMCORE_EPISODES_ENABLED)".to_string(),
            ));
        }
        let config = &self.candidate_pipeline;
        let analysis = config
            .query_analysis
            .then(|| analyze(&input.query, input.reference_time));
        let mut stages = Vec::new();

        let mut list = self
            .baseline_candidates(
                &input.tenant,
                &input.query,
                input.fact_limit,
                input.episode_limit,
            )
            .await?;

        // Extra queries, after the original: the deterministic clauses, then the sub-queries of
        // the question's one decomposition operation, shared by every request with the
        // same fingerprint. Sub-queries are never decomposed again.
        let mut sub_queries: Vec<String> = Vec::new();
        if config.multi_query == MultiQueryMode::Deterministic
            && let Some(analysis) = &analysis
            && !analysis.sub_queries.is_empty()
        {
            sub_queries.extend(analysis.sub_queries.iter().cloned());
            stages.push("multi_query".to_string());
        }
        let mut decomposition = None;
        if let Some(service) = &self.decomposition {
            let audit = service
                .decompose(
                    &input.tenant.org_id,
                    &input.query,
                    input.reference_time,
                    &sub_queries,
                    config.decomposition_timeout,
                )
                .await;
            sub_queries.extend(audit.accepted.iter().cloned());
            stages.push(format!("decomposition:{}", service.provider_name()));
            decomposition = Some(audit);
        }
        if !sub_queries.is_empty() {
            let mut lists = vec![list];
            for sub_query in &sub_queries {
                lists.push(
                    self.baseline_candidates(
                        &input.tenant,
                        sub_query,
                        input.fact_limit,
                        input.episode_limit,
                    )
                    .await?,
                );
            }
            list = merge_multi_query(lists);
        }

        if config.temporal_ranking
            && let Some(analysis) = &analysis
        {
            list = apply_temporal_ranking(list, analysis, config.temporal_boost, config.window);
            stages.push("temporal_ranking".to_string());
        }

        let mut rerank_fallback = None;
        let mut rerank_latency_ms = None;
        if let Some(provider) = &self.rerank_provider {
            let outcome = rerank(
                provider.as_ref(),
                &input.query,
                list,
                config.window,
                config.rerank_timeout,
            )
            .await;
            list = outcome.list;
            rerank_fallback = outcome.fallback;
            rerank_latency_ms = Some(outcome.latency_ms);
            stages.push(format!("rerank:{}", provider.name()));
        }

        Ok(SearchCandidatesOutput {
            results: list,
            analysis,
            stages,
            rerank_fallback,
            rerank_latency_ms,
            decomposition,
        })
    }

    /// The E-merge list for one query.
    async fn baseline_candidates(
        &self,
        tenant: &TenantContext,
        query: &str,
        fact_limit: usize,
        episode_limit: usize,
    ) -> MemcoreResult<Vec<Candidate>> {
        // The two legs are independent reads, so they run concurrently. Errors keep
        // the sequential precedence (a fact-leg error wins over an episode-leg error).
        let (facts, episodes) = tokio::join!(
            self.search_memory_with_options(
                SearchMemoryInput {
                    tenant: tenant.clone(),
                    query: query.to_string(),
                    limit: fact_limit,
                    memory_types: None,
                    metadata_filter: None,
                },
                SearchOptions {
                    mode: Some(RetrievalMode::Semantic),
                    user_names: Vec::new(),
                    include_history: false,
                },
            ),
            self.search_episodes(SearchEpisodesInput {
                tenant: tenant.clone(),
                query: query.to_string(),
                limit: episode_limit,
            }),
        );
        let facts = facts?;
        let episodes = episodes?;

        let fact_candidates = facts
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                Candidate::from_fact(result, index + 1).ok_or_else(|| {
                    MemcoreError::Internal(format!(
                        "semantic search returned fact {} without a vector score",
                        result.fact_id
                    ))
                })
            })
            .collect::<MemcoreResult<Vec<_>>>()?;
        let episode_candidates = episodes
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                Candidate::from_episode(&result.episode, result.score, index + 1)
            })
            .collect();
        Ok(merge_by_vector_score(fact_candidates, episode_candidates))
    }

    /// The server-side context for one user. With episodes on, the E-merge of semantic facts @100
    /// and episodes @100 (`baseline_candidates`, regardless of any candidate option); with
    /// episodes off, the facts @100 in search order (the facts-only form). Then the prefix at
    /// `token_budget` cl100k tokens and the reference line format (`crate::d76_context`).
    pub async fn d76_context(&self, input: D76ContextInput) -> MemcoreResult<D76ContextOutput> {
        Ok(self
            .d76_context_with_packing(input, D76Packing::Prefix)
            .await?
            .output)
    }

    /// [`Self::d76_context`] with an explicit packing. `Prefix` is the validated reference;
    /// `Fill` (opt-in per request) keeps trying later candidates after one that does not
    /// fit. Retrieval, ranking and the line format are the same for both.
    pub async fn d76_context_with_packing(
        &self,
        input: D76ContextInput,
        packing: D76Packing,
    ) -> MemcoreResult<D76PackedContextOutput> {
        self.d76_context_with_options(
            input,
            packing,
            crate::d76_context::D76ContextOptions::default(),
        )
        .await
    }

    /// [`Self::d76_context_with_packing`] with per-request options. With the default
    /// options it is exactly the validated path; `exclude_message_ids` removes matching episode
    /// candidates (exact message id; never facts) before packing.
    pub async fn d76_context_with_options(
        &self,
        input: D76ContextInput,
        packing: D76Packing,
        options: crate::d76_context::D76ContextOptions,
    ) -> MemcoreResult<D76PackedContextOutput> {
        if options.exclude_message_ids.len() > crate::d76_context::D76_MAX_EXCLUDED_MESSAGE_IDS
            || options.exclude_message_ids.iter().any(|id| {
                id.trim().is_empty()
                    || id.chars().count() > crate::d76_context::D76_MAX_MESSAGE_ID_CHARS
            })
        {
            return Err(MemcoreError::ValidationError(format!(
                "exclude_message_ids takes at most {} non-empty ids of at most {} characters",
                crate::d76_context::D76_MAX_EXCLUDED_MESSAGE_IDS,
                crate::d76_context::D76_MAX_MESSAGE_ID_CHARS
            )));
        }
        if input.token_budget == 0 || input.token_budget > D76_MAX_TOKEN_BUDGET {
            return Err(MemcoreError::ValidationError(format!(
                "token_budget must be between 1 and {D76_MAX_TOKEN_BUDGET}"
            )));
        }
        if self.search_max_limit() < D76_CANDIDATE_DEPTH {
            return Err(MemcoreError::ValidationError(format!(
                "the server-side context retrieves {D76_CANDIDATE_DEPTH} facts and {D76_CANDIDATE_DEPTH} \
                 episodes; it needs MEMCORE_SEARCH_MAX_LIMIT >= {D76_CANDIDATE_DEPTH}"
            )));
        }
        let episodes_included = self.episodes.is_some();
        let candidates = if episodes_included {
            self.baseline_candidates(
                &input.tenant,
                &input.query,
                D76_CANDIDATE_DEPTH,
                D76_CANDIDATE_DEPTH,
            )
            .await?
        } else {
            let facts = self
                .search_memory_with_options(
                    SearchMemoryInput {
                        tenant: input.tenant.clone(),
                        query: input.query.clone(),
                        limit: D76_CANDIDATE_DEPTH,
                        memory_types: None,
                        metadata_filter: None,
                    },
                    SearchOptions {
                        mode: Some(RetrievalMode::Semantic),
                        user_names: Vec::new(),
                        include_history: false,
                    },
                )
                .await?;
            facts
                .results
                .iter()
                .enumerate()
                .map(|(index, result)| {
                    Candidate::from_fact(result, index + 1).ok_or_else(|| {
                        MemcoreError::Internal(format!(
                            "semantic search returned fact {} without a vector score",
                            result.fact_id
                        ))
                    })
                })
                .collect::<MemcoreResult<Vec<_>>>()?
        };
        let (candidates, excluded_candidates) = crate::d76_context::exclude_episodes_by_message_id(
            candidates,
            &options.exclude_message_ids,
        );
        let render_started = std::time::Instant::now();
        let (context, lines_skipped) = if packing == D76Packing::Evidence {
            render_evidence(&candidates, input.token_budget)
        } else {
            let lines: Vec<D76Line> = candidates.iter().map(D76Line::from_candidate).collect();
            render_packed(&lines, input.token_budget, packing)
        };
        crate::observability::record_stage("search", "d76_render", render_started);
        let item_provenance = crate::d76_context::item_provenance(&context, &candidates);
        Ok(D76PackedContextOutput {
            output: D76ContextOutput {
                context,
                episodes_included,
                token_budget: input.token_budget,
            },
            packing,
            lines_skipped,
            excluded_candidates,
            item_provenance,
        })
    }
}
