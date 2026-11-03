//! On consistent data (every point carries its
//! live SQL row's text), search, candidates and the server-side context are byte-identical to the
//! pre-change engine.
//!
//! The golden transcript `tests/fixtures/ar023_s1_parity.golden` was written by THIS test on the
//! pre-change engine source (`MEMCORE_AR023_S1_GOLDEN_WRITE=1`); the test uses only API
//! that exists there. Ids (random per run) are replaced by fixture labels; nothing else is edited.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use memcore_core::candidates::SearchCandidatesInput;
use memcore_core::d76_context::D76ContextInput;
use memcore_core::{
    AddMemoryInput, CandidateFact, EpisodeComponents, EpisodeStore, FactSearchQuery, FactStore,
    MemoryEngine, MemoryMessage, MemoryType, MessageRole, RetrievalMode, SearchMemoryInput,
    SearchOptions, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;

const DIMS: usize = 32;
const TENANTS: [(&str, &str); 3] = [("org_a", "u1"), ("org_a", "u2"), ("org_b", "u1")];
const TOPICS: [&str; 6] = [
    "kayaking on mountain lakes",
    "baking sourdough bread",
    "restoring vintage radios",
    "kayaking trips with friends",
    "reading science fiction",
    "growing tomatoes on the balcony",
];

fn tenant(index: usize) -> TenantContext {
    TenantContext::new(TENANTS[index].0, TENANTS[index].1).expect("tenant")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ar023_s1_parity.golden")
}

struct Stores {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
}

impl Stores {
    fn engine(&self, llm: MockLlmProvider, episodes: bool) -> MemoryEngine {
        let engine = MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            Arc::new(llm),
            Arc::new(MockEmbeddingProvider::new(DIMS)),
        );
        let engine = if episodes {
            engine.with_episodes(Some(EpisodeComponents {
                store: self.episodes.clone(),
                vectors: self.episode_vectors.clone(),
            }))
        } else {
            engine
        };
        engine.with_search_max_limit(100).expect("limit")
    }
}

async fn seed() -> (Stores, BTreeMap<String, String>) {
    let stores = Stores {
        facts: Arc::new(MockFactStore::new()),
        vectors: Arc::new(MockVectorStore::new()),
        episodes: Arc::new(MockEpisodeStore::new()),
        episode_vectors: Arc::new(MockVectorStore::new()),
    };
    let mut labels = BTreeMap::new();
    for index in 0..TENANTS.len() {
        let candidates = TOPICS
            .iter()
            .enumerate()
            .map(|(n, topic)| {
                CandidateFact::new(
                    format!("T{index} fact {n} prefers {topic}"),
                    if n % 2 == 0 {
                        MemoryType::Skill
                    } else {
                        MemoryType::Preference
                    },
                    0.9,
                    0.5 + 0.05 * n as f32,
                    None,
                    json!({ "label": format!("T{index}F{n}"), "n": n }),
                )
                .expect("candidate")
            })
            .collect();
        let messages: Vec<MemoryMessage> = (0..4)
            .map(|n| MemoryMessage {
                role: if n % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                content: if n == 3 {
                    "short msg three".to_string() // stored, not embedded
                } else {
                    format!("T{index} episode {n} talks about {}", TOPICS[n])
                },
                message_id: Some(format!("m{n}")),
                ..Default::default()
            })
            .collect();
        let out = stores
            .engine(
                MockLlmProvider::new().with_extraction_candidates(candidates),
                true,
            )
            .add_memory(AddMemoryInput {
                tenant: tenant(index),
                messages,
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .expect("seed");
        assert_eq!(out.added, TOPICS.len());
        let mut query = FactSearchQuery::new(tenant(index), 100);
        query.include_deleted = true;
        for fact in stores.facts.search_facts(query).await.expect("facts") {
            labels.insert(
                fact.id.to_string(),
                fact.metadata["label"].as_str().unwrap().to_string(),
            );
        }
        for episode in stores
            .episodes
            .list_episodes(&tenant(index), 100)
            .await
            .expect("episodes")
        {
            let label = format!(
                "T{index}E{}",
                episode.message_id.clone().unwrap_or_default()
            );
            labels.insert(episode.id.to_string(), label);
        }
    }
    (stores, labels)
}

fn normalise(text: String, labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .fold(text, |acc, (id, label)| acc.replace(id.as_str(), label))
}

async fn transcript() -> String {
    let (stores, labels) = seed().await;
    let with_episodes = stores.engine(MockLlmProvider::new(), true);
    let facts_only = stores.engine(MockLlmProvider::new(), false);
    let queries: Vec<String> = vec![
        "T0 fact 0 prefers kayaking on mountain lakes".to_string(),
        "kayaking".to_string(),
        "bread radios tomatoes".to_string(),
        "T1 fact 4 prefers reading science fiction".to_string(),
        "nothing matches this query at all".to_string(),
    ];
    let mut out = String::new();
    for index in 0..2 {
        for query in &queries {
            for mode in [
                RetrievalMode::Semantic,
                RetrievalMode::Hybrid,
                RetrievalMode::Rrf,
                RetrievalMode::Fulltext,
            ] {
                for limit in [3usize, 10] {
                    let results = with_episodes
                        .search_memory_with_options(
                            SearchMemoryInput {
                                tenant: tenant(index),
                                query: query.clone(),
                                limit,
                                memory_types: None,
                                metadata_filter: None,
                            },
                            SearchOptions {
                                mode: Some(mode),
                                user_names: Vec::new(),
                                include_history: false,
                            },
                        )
                        .await
                        .expect("search")
                        .results;
                    out.push_str(&format!("search t{index} {mode:?} {limit} {query:?}\n"));
                    for result in results {
                        out.push_str(&format!("  {result:?}\n"));
                    }
                }
            }
            let candidates = with_episodes
                .search_candidates(SearchCandidatesInput {
                    tenant: tenant(index),
                    query: query.clone(),
                    fact_limit: 100,
                    episode_limit: 100,
                    reference_time: None,
                })
                .await
                .expect("candidates");
            out.push_str(&format!(
                "candidates t{index} {query:?}\n  {:?}\n",
                candidates.results
            ));
            for (name, engine) in [("emerge", &with_episodes), ("c0", &facts_only)] {
                for budget in [40usize, 1500] {
                    let context = engine
                        .d76_context(D76ContextInput {
                            tenant: tenant(index),
                            query: query.clone(),
                            token_budget: budget,
                        })
                        .await
                        .expect("context");
                    out.push_str(&format!(
                        "context {name} t{index} {budget} {query:?}\n  {:?}\n",
                        context
                    ));
                }
            }
        }
    }
    normalise(out, &labels)
}

#[tokio::test]
async fn r1_e_consistent_data_output_is_identical_to_the_pre_change_engine() {
    let first = transcript().await;
    let second = transcript().await;
    assert_eq!(
        first, second,
        "X4: the transcript must be deterministic within a run"
    );
    if std::env::var("MEMCORE_AR023_S1_GOLDEN_WRITE").as_deref() == Ok("1") {
        std::fs::create_dir_all(golden_path().parent().unwrap()).expect("dir");
        std::fs::write(golden_path(), &first).expect("write golden");
        return;
    }
    let golden = std::fs::read_to_string(golden_path()).expect("golden (written on 0599dcf)");
    assert_eq!(
        first,
        golden.replace("\r\n", "\n"),
        "R1-E: output differs from the pre-change engine"
    );
}
