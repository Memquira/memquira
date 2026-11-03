//! Candidate pipeline through the engine, with mock stores and
//! mock rerank / decomposition providers: flags off = baseline; reranking reorders only the window
//! and keeps every candidate; any provider failure keeps the baseline order and is reported; the
//! decomposition provider is called at most once and its sub-queries widen the candidate set.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::candidates::decomposition::{
    DecompositionIdentity, DecompositionProvider, DecompositionState, FailureClass,
};
use memcore_core::candidates::rerank::RerankProvider;
use memcore_core::{
    AddMemoryInput, CandidatePipelineConfig, EpisodeComponents, MemoryEngine, MemoryMessage,
    MemorySession, MessageRole, SearchCandidatesInput, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const ORG: &str = "org_phase4_pipeline";

fn tenant(user: &str) -> TenantContext {
    TenantContext::new(ORG, user).expect("tenant")
}

fn engine() -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_episodes(Some(EpisodeComponents {
        store: Arc::new(MockEpisodeStore::new()),
        vectors: Arc::new(MockVectorStore::new()),
    }))
}

async fn seed(engine: &MemoryEngine) {
    for (user, lines) in [
        (
            "rowena",
            vec![
                "Rowena adopted a rescue greyhound called Juniper in the spring.",
                "Juniper enjoys long walks beside the canal most mornings.",
                "Rowena's sister Ixora gave her a copper kettle for her birthday.",
                "Rowena and Dunstan are repainting the kitchen a pale sage green.",
                "Rowena signed up for a pottery course at the community hall.",
            ],
        ),
        (
            "pellam",
            vec!["Pellam keeps a secret greyhound sanctuary high in the hills."],
        ),
    ] {
        let messages = lines
            .iter()
            .enumerate()
            .map(|(index, content)| MemoryMessage {
                role: MessageRole::User,
                content: content.to_string(),
                name: Some(user.to_string()),
                timestamp: None,
                message_id: Some(format!("D1:{index}")),
            })
            .collect();
        engine
            .add_memory(AddMemoryInput {
                tenant: tenant(user),
                messages,
                metadata: json!({ "session_id": "S1" }),
                session: MemorySession {
                    id: Some("S1".to_string()),
                    timestamp: Some(Utc.with_ymd_and_hms(2023, 5, 8, 13, 0, 0).unwrap()),
                },
            })
            .await
            .expect("add");
    }
}

fn input(query: &str) -> SearchCandidatesInput {
    SearchCandidatesInput {
        tenant: tenant("rowena"),
        query: query.to_string(),
        fact_limit: 50,
        episode_limit: 50,
        reference_time: None,
    }
}

fn ids(list: &[memcore_core::Candidate]) -> Vec<Uuid> {
    list.iter().map(|candidate| candidate.id).collect()
}

/// Scores documents by their position, in reverse: the last document in the window ranks first.
struct Reverse(AtomicU32);

#[async_trait]
impl RerankProvider for Reverse {
    fn name(&self) -> &'static str {
        "reverse"
    }
    async fn score(&self, _query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok((0..documents.len()).map(|index| index as f32).collect())
    }
}

struct Broken;

#[async_trait]
impl RerankProvider for Broken {
    fn name(&self) -> &'static str {
        "broken"
    }
    async fn score(&self, _query: &str, documents: &[String]) -> MemcoreResult<Vec<f32>> {
        // One score short: a corrupted response must never be applied.
        Ok(vec![0.5; documents.len().saturating_sub(1)])
    }
}

struct Unavailable;

#[async_trait]
impl RerankProvider for Unavailable {
    fn name(&self) -> &'static str {
        "unavailable"
    }
    async fn score(&self, _query: &str, _documents: &[String]) -> MemcoreResult<Vec<f32>> {
        Err(MemcoreError::ProviderError(
            "service unavailable".to_string(),
        ))
    }
}

struct CountingDecomposer(AtomicU32);

#[async_trait]
impl DecompositionProvider for CountingDecomposer {
    fn name(&self) -> &'static str {
        "counting"
    }
    fn identity(&self) -> DecompositionIdentity {
        DecompositionIdentity {
            provider: "counting".to_string(),
            model: "mock".to_string(),
        }
    }
    async fn decompose(&self, _query: &str) -> Result<Vec<String>, FailureClass> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![
            "birthday kettle from a sister".to_string(),
            "pottery course".to_string(),
        ])
    }
}

#[tokio::test]
async fn reranking_reorders_only_the_window_and_keeps_every_candidate() {
    let base = engine();
    seed(&base).await;
    let baseline = base
        .search_candidates(input("greyhound walks"))
        .await
        .unwrap();
    assert!(baseline.stages.is_empty() && baseline.rerank_latency_ms.is_none());
    assert!(baseline.results.len() >= 4);

    let window = 3;
    let reverse = Arc::new(Reverse(AtomicU32::new(0)));
    let reranking = base
        .with_candidate_pipeline(CandidatePipelineConfig {
            window,
            ..CandidatePipelineConfig::default()
        })
        .with_rerank_provider(Some(reverse.clone()));
    let reranked = reranking
        .search_candidates(input("greyhound walks"))
        .await
        .unwrap();
    assert_eq!(reverse.0.load(Ordering::SeqCst), 1);
    assert_eq!(reranked.stages, ["rerank:reverse"]);
    assert!(reranked.rerank_fallback.is_none());
    assert!(reranked.rerank_latency_ms.is_some());
    let before = ids(&baseline.results);
    let after = ids(&reranked.results);
    let mut expected: Vec<Uuid> = before[..window].iter().rev().copied().collect();
    expected.extend(&before[window..]);
    assert_eq!(after, expected);
    // The window carries the provider's scores; nothing else does.
    assert!(baseline.results.iter().all(|c| c.rerank_score.is_none()));
    assert!(
        reranked.results[..window]
            .iter()
            .all(|c| c.rerank_score.is_some())
    );
    assert!(
        reranked.results[window..]
            .iter()
            .all(|c| c.rerank_score.is_none())
    );
    // No other user's candidate appears, before or after reranking.
    assert!(
        reranked
            .results
            .iter()
            .all(|c| !c.content.contains("secret greyhound sanctuary"))
    );
}

#[tokio::test]
async fn provider_failures_keep_the_baseline_order_with_an_audit_trail() {
    let base = engine();
    seed(&base).await;
    let baseline = ids(&base
        .search_candidates(input("kitchen paint"))
        .await
        .unwrap()
        .results);

    let broken = engine().with_rerank_provider(Some(Arc::new(Broken)));
    seed(&broken).await;
    let out = broken
        .search_candidates(input("kitchen paint"))
        .await
        .unwrap();
    assert_eq!(out.results.len(), baseline.len());
    assert!(out.rerank_fallback.unwrap().starts_with("broken: "));

    let unavailable = base.with_rerank_provider(Some(Arc::new(Unavailable)));
    let out = unavailable
        .search_candidates(input("kitchen paint"))
        .await
        .unwrap();
    assert_eq!(ids(&out.results), baseline, "baseline order kept");
    assert_eq!(
        out.rerank_fallback.as_deref(),
        Some("unavailable: provider error: service unavailable")
    );
}

#[tokio::test]
async fn decomposition_is_called_once_and_widens_the_candidate_set() {
    let base = engine();
    seed(&base).await;
    let small = |engine: &MemoryEngine| {
        let _ = engine;
        SearchCandidatesInput {
            fact_limit: 1,
            episode_limit: 1,
            ..input("greyhound walks")
        }
    };
    let baseline = base.search_candidates(small(&base)).await.unwrap();

    let decomposer = Arc::new(CountingDecomposer(AtomicU32::new(0)));
    let decomposing = base
        .with_candidate_pipeline(CandidatePipelineConfig {
            decomposition_timeout: Duration::from_secs(5),
            ..CandidatePipelineConfig::default()
        })
        .with_decomposition_provider(Some(decomposer.clone()));
    let out = decomposing
        .search_candidates(small(&decomposing))
        .await
        .unwrap();
    assert_eq!(decomposer.0.load(Ordering::SeqCst), 1, "exactly one call");
    let audit = out.decomposition.expect("audit");
    assert_eq!(
        (audit.state, audit.operations),
        (DecompositionState::Called, 1)
    );
    assert_eq!(audit.accepted.len(), 2);
    assert_eq!(out.stages, ["decomposition:counting"]);
    // The original query's candidates are all still present, and the union is larger.
    for id in ids(&baseline.results) {
        assert!(ids(&out.results).contains(&id));
    }
    assert!(out.results.len() > baseline.results.len());
    let unique: std::collections::HashSet<Uuid> = ids(&out.results).into_iter().collect();
    assert_eq!(unique.len(), out.results.len(), "no duplicates");

    // The other speaker's request for the same question (same organisation, another
    // user) reuses the result; the logical question made one decomposition operation in total.
    let other_speaker = SearchCandidatesInput {
        tenant: tenant("dunstan"),
        ..small(&decomposing)
    };
    let second = decomposing.search_candidates(other_speaker).await.unwrap();
    let second_audit = second.decomposition.expect("audit");
    assert_eq!(
        (second_audit.state, second_audit.operations),
        (DecompositionState::CacheHit, 0)
    );
    assert_eq!(second_audit.fingerprint, audit.fingerprint);
    assert_eq!(second_audit.accepted, audit.accepted);
    assert_eq!(
        decomposer.0.load(Ordering::SeqCst),
        1,
        "one operation per question"
    );
}
