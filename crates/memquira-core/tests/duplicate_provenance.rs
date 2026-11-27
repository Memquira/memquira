//! Duplicate decisions, provenance and deletion, on deterministic fixtures.
//!
//! Deleting a memory withholds exactly the messages its `source_message_ids` cite. These cases
//! pin what that means when ingestion's duplicate decision is wrong, when a message is repeated,
//! when provenance is missing, when one message yields several memories, and when the message
//! itself asks to forget. Scripted extraction, classification and embeddings; no model calls;
//! invented data only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memquira_common::MemquiraResult;
use memquira_core::d76_context::D76ContextInput;
use memquira_core::ports::{EmbeddingProvider, SummarizationInput};
use memquira_core::{
    AddMemoryInput, CandidateFact, DeleteMemoryInput, EpisodeComponents, FactClassificationInput,
    FactExtractionInput, FactExtractionOutput, FactOperation, FactOperationDecision, FactStructure,
    ListMemoriesInput, LlmProvider, MemoryEngine, MemoryMessage, MemorySession, MemoryType,
    MessageRole, SearchEpisodesInput, SearchMemoryInput, StructuredExtractionConfig, TenantContext,
};
use memquira_providers::deterministic_embedding;
use memquira_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const ORG: &str = "org_duplicate_provenance";
const USER: &str = "avery";
const QUERY: &str = "What do we know about the user?";
const DIMS: usize = 64;

fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

fn message(id: &str, content: &str) -> MemoryMessage {
    MemoryMessage {
        role: MessageRole::User,
        content: content.to_string(),
        name: Some("Avery".to_string()),
        timestamp: None,
        message_id: Some(id.to_string()),
    }
}

fn candidate(content: &str, cites: &[&str]) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Project, 0.9, 0.8, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some("Avery".to_string()),
            source_message_ids: cites.iter().map(|id| id.to_string()).collect(),
            ..FactStructure::default()
        })
}

/// A unit vector along one axis: texts given the same axis are exact semantic duplicates (cosine
/// 1.0), texts on different axes are unrelated (cosine 0.0).
fn axis(n: usize) -> Vec<f32> {
    let mut v = vec![0.0; DIMS];
    v[n] = 1.0;
    v
}

/// Texts containing a pinned phrase get that phrase's vector; every other text the mocks'
/// deterministic embedding.
#[derive(Default)]
struct Embed {
    pinned: Mutex<Vec<(String, Vec<f32>)>>,
}

impl Embed {
    fn pin(&self, phrase: &str, vector: Vec<f32>) {
        self.pinned
            .lock()
            .unwrap()
            .push((phrase.to_string(), vector));
    }

    fn vector(&self, text: &str) -> MemquiraResult<Vec<f32>> {
        for (phrase, vector) in self.pinned.lock().unwrap().iter() {
            if text.contains(phrase.as_str()) {
                return Ok(vector.clone());
            }
        }
        deterministic_embedding(text, DIMS)
    }
}

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemquiraResult<Vec<f32>> {
        self.vector(text)
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemquiraResult<Vec<Vec<f32>>> {
        texts.iter().map(|text| self.vector(text)).collect()
    }

    fn dimensions(&self) -> usize {
        DIMS
    }
}

/// Extraction returns the next scripted candidate list; classification the next scripted
/// decision (an Add when none is queued).
#[derive(Default)]
struct Scripted {
    extractions: Mutex<VecDeque<Vec<CandidateFact>>>,
    decisions: Mutex<VecDeque<(FactOperation, Option<Uuid>)>>,
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn extract_facts(
        &self,
        _input: FactExtractionInput,
    ) -> MemquiraResult<FactExtractionOutput> {
        let next = self
            .extractions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        Ok(FactExtractionOutput::from_candidates(next))
    }

    async fn classify_fact_operation(
        &self,
        _input: FactClassificationInput,
    ) -> MemquiraResult<FactOperationDecision> {
        let (operation, target_fact_id) = self
            .decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or((FactOperation::Add, None));
        Ok(FactOperationDecision {
            operation,
            target_fact_id,
            reason: None,
            confidence: 0.9,
        })
    }

    async fn summarize_memory(&self, _input: SummarizationInput) -> MemquiraResult<String> {
        Ok(String::new())
    }
}

struct Fixture {
    engine: MemoryEngine,
    llm: Arc<Scripted>,
    embed: Arc<Embed>,
}

impl Fixture {
    fn new(structured: bool) -> Self {
        let llm = Arc::new(Scripted::default());
        let embed = Arc::new(Embed::default());
        let engine = MemoryEngine::new(
            Arc::new(MockFactStore::new()),
            Arc::new(MockVectorStore::new()),
            llm.clone(),
            embed.clone(),
        )
        .with_episodes(Some(EpisodeComponents {
            store: Arc::new(MockEpisodeStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
        }))
        .with_structured_extraction(structured.then(StructuredExtractionConfig::default))
        .with_search_max_limit(100)
        .expect("search limit");
        Self { engine, llm, embed }
    }

    /// Sends `messages` as one request whose extraction returns `facts`; returns the ids of the
    /// memories it stored.
    async fn add(&self, messages: Vec<MemoryMessage>, facts: Vec<CandidateFact>) -> Vec<Uuid> {
        self.llm.extractions.lock().unwrap().push_back(facts);
        self.engine
            .add_memory(AddMemoryInput {
                tenant: tenant(),
                messages,
                metadata: json!({}),
                session: MemorySession {
                    id: Some("s1".to_string()),
                    timestamp: None,
                },
            })
            .await
            .expect("add")
            .memories
            .iter()
            .map(|fact| fact.id)
            .collect()
    }

    async fn delete(&self, id: Uuid) {
        self.engine
            .delete_memory(DeleteMemoryInput {
                tenant: tenant(),
                memory_id: id,
            })
            .await
            .expect("delete");
    }

    /// Everything ordinary retrieval returns, as one text, with each source labelled.
    async fn retrieved(&self) -> String {
        let mut text = Vec::new();
        for hit in self
            .engine
            .search_episodes(SearchEpisodesInput {
                tenant: tenant(),
                query: QUERY.to_string(),
                limit: 100,
            })
            .await
            .expect("episodes")
            .results
        {
            text.push(format!("episode: {}", hit.episode.content));
        }
        let context = self
            .engine
            .d76_context(D76ContextInput {
                tenant: tenant(),
                query: QUERY.to_string(),
                token_budget: 4_000,
            })
            .await
            .expect("context")
            .context
            .text;
        text.push(format!("context: {context}"));
        for hit in self
            .engine
            .search_memory(SearchMemoryInput {
                tenant: tenant(),
                query: QUERY.to_string(),
                limit: 100,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("search")
            .results
        {
            text.push(format!("fact: {}", hit.content));
        }
        for fact in self
            .engine
            .list_memories(ListMemoriesInput {
                tenant: tenant(),
                memory_type: None,
                query_text: None,
                limit: 100,
                cursor: None,
                include_deleted: false,
            })
            .await
            .expect("list")
            .memories
        {
            text.push(format!("listed: {}", fact.content));
        }
        text.join("\n")
    }

    async fn source_ids(&self) -> HashMap<String, Vec<String>> {
        self.engine
            .list_memories(ListMemoriesInput {
                tenant: tenant(),
                memory_type: None,
                query_text: None,
                limit: 100,
                cursor: None,
                include_deleted: true,
            })
            .await
            .expect("list")
            .memories
            .into_iter()
            .map(|fact| (fact.content, fact.structure.source_message_ids))
            .collect()
    }
}

/// The Claude Code residual case, reproduced. The codename message is judged a semantic duplicate
/// of an unrelated memory (the test stack's 8-dimensional embedding did this), so it is recorded as
/// a source of THAT memory. Deleting the codename memory stored later withholds only its own
/// message; the first codename message comes back until the memory it was attributed to is
/// deleted. Deletion follows provenance exactly: the defect is the duplicate decision.
#[tokio::test]
async fn a_false_duplicate_attributes_the_message_to_the_memory_it_was_merged_into() {
    let f = Fixture::new(true);
    f.embed.pin("stack is Rust", axis(1));
    f.embed.pin("codename is Bluefin", axis(1)); // wrongly "the same" as the stack
    f.embed.pin("codename: Bluefin", axis(2));

    let stack = f
        .add(
            vec![message("m1", "We write everything in Rust.")],
            vec![candidate("Avery's stack is Rust.", &["m1"])],
        )
        .await;
    let merged = f
        .add(
            vec![message("m2", "Our internal launch codename is Bluefin.")],
            vec![candidate("The launch codename is Bluefin.", &["m2"])],
        )
        .await;
    assert!(merged.is_empty(), "discarded as a duplicate of the stack");
    assert_eq!(
        f.source_ids().await["Avery's stack is Rust."],
        vec!["m1".to_string(), "m2".to_string()],
        "the codename message is now a source of the stack memory"
    );
    let codename = f
        .add(
            vec![message("m3", "Remember: the codename is Bluefin.")],
            vec![candidate("Avery's launch codename: Bluefin.", &["m3"])],
        )
        .await;
    assert_eq!(codename.len(), 1);

    f.delete(codename[0]).await;
    let after = f.retrieved().await;
    assert!(
        after.contains("episode: Our internal launch codename is Bluefin."),
        "the misattributed message still comes back:\n{after}"
    );
    assert!(!after.contains("Remember: the codename"), "{after}");

    // Deleting the memory it was attributed to withholds it.
    f.delete(stack[0]).await;
    assert!(!f.retrieved().await.contains("Bluefin"));
}

/// The same messages with a correct duplicate decision: nothing comes back.
#[tokio::test]
async fn with_a_correct_duplicate_decision_the_codename_does_not_come_back() {
    let f = Fixture::new(true);
    f.embed.pin("stack is Rust", axis(1));
    f.embed.pin("codename is Bluefin", axis(2));
    f.embed.pin("codename: Bluefin", axis(2)); // a true restatement

    f.add(
        vec![message("m1", "We write everything in Rust.")],
        vec![candidate("Avery's stack is Rust.", &["m1"])],
    )
    .await;
    let codename = f
        .add(
            vec![message("m2", "Our internal launch codename is Bluefin.")],
            vec![candidate("The launch codename is Bluefin.", &["m2"])],
        )
        .await;
    let restated = f
        .add(
            vec![message("m3", "Remember: the codename is Bluefin.")],
            vec![candidate("Avery's launch codename: Bluefin.", &["m3"])],
        )
        .await;
    assert!(restated.is_empty(), "the restatement is a duplicate");
    assert_eq!(
        f.source_ids().await["The launch codename is Bluefin."],
        vec!["m2".to_string(), "m3".to_string()]
    );

    f.delete(codename[0]).await;
    let after = f.retrieved().await;
    assert!(!after.contains("Bluefin"), "{after}");
    assert!(after.contains("Rust"), "{after}");
}

/// A repeated message (same text, new id) and a retried message (same id) are withheld with the
/// memory they restate.
#[tokio::test]
async fn repeated_and_retried_messages_are_withheld_with_the_memory() {
    let f = Fixture::new(true);
    let secret = f
        .add(
            vec![message("m1", "My locker code is 4417.")],
            vec![candidate("Avery's locker code is 4417.", &["m1"])],
        )
        .await;
    let repeat = f
        .add(
            vec![message("m2", "My locker code is 4417.")],
            vec![candidate("Avery's locker code is 4417.", &["m2"])],
        )
        .await;
    assert!(repeat.is_empty(), "an exact duplicate");
    // The client retries the first request: the same message id and text.
    f.add(
        vec![message("m1", "My locker code is 4417.")],
        vec![candidate("Avery's locker code is 4417.", &["m1"])],
    )
    .await;

    f.delete(secret[0]).await;
    let after = f.retrieved().await;
    assert!(!after.contains("4417"), "{after}");
}

/// Missing provenance, pinned (known limitation): a memory from a request of several messages that
/// the model did not attribute cites no message, so deleting it withholds none of them. With the
/// citation the message is withheld.
#[tokio::test]
async fn a_memory_without_provenance_withholds_no_message() {
    let f = Fixture::new(true);
    let uncited = f
        .add(
            vec![
                message("m1", "My locker code is 4417."),
                message("m2", "Also, I take the early train."),
            ],
            vec![candidate("Avery's locker code is 4417.", &[])],
        )
        .await;
    assert!(f.source_ids().await["Avery's locker code is 4417."].is_empty());
    f.delete(uncited[0]).await;
    let after = f.retrieved().await;
    assert!(
        after.contains("episode: My locker code is 4417."),
        "without provenance the message stays retrievable:\n{after}"
    );

    let g = Fixture::new(true);
    let cited = g
        .add(
            vec![
                message("m1", "My locker code is 4417."),
                message("m2", "Also, I take the early train."),
            ],
            vec![candidate("Avery's locker code is 4417.", &["m1"])],
        )
        .await;
    g.delete(cited[0]).await;
    let after = g.retrieved().await;
    assert!(!after.contains("4417"), "{after}");
    assert!(after.contains("early train"), "{after}");
}

/// Missing provenance, pinned (known limitation): the unstructured extraction
/// (`MEMQUIRA_STRUCTURED_EXTRACTION_ENABLED=false`) records no source messages, so a delete
/// withholds nothing.
#[tokio::test]
async fn the_unstructured_extraction_records_no_provenance() {
    let f = Fixture::new(false);
    let secret = f
        .add(
            vec![message("m1", "My locker code is 4417.")],
            vec![
                CandidateFact::new(
                    "Avery's locker code is 4417.",
                    MemoryType::Project,
                    0.9,
                    0.8,
                    None,
                    json!({}),
                )
                .unwrap(),
            ],
        )
        .await;
    f.delete(secret[0]).await;
    assert!(
        f.retrieved()
            .await
            .contains("episode: My locker code is 4417.")
    );
}

/// Several memories from one message: deleting one withholds the message; the others stay
/// retrievable as memories.
#[tokio::test]
async fn several_memories_from_one_message() {
    let f = Fixture::new(true);
    let stored = f
        .add(
            vec![message(
                "m1",
                "My locker code is 4417 and I take the early train.",
            )],
            vec![
                candidate("Avery's locker code is 4417.", &["m1"]),
                candidate("Avery takes the early train.", &["m1"]),
            ],
        )
        .await;
    assert_eq!(stored.len(), 2);
    f.delete(stored[0]).await;
    let after = f.retrieved().await;
    assert!(!after.contains("4417"), "{after}");
    assert!(
        after.contains("fact: Avery takes the early train."),
        "{after}"
    );
}

/// A message that asks to forget a memory usually repeats it. When classification turns it into
/// a delete of that memory, the message is recorded as a source of the memory first, so it is
/// withheld with it.
#[tokio::test]
async fn a_message_asking_to_forget_is_withheld_with_the_memory() {
    let f = Fixture::new(true);
    let secret = f
        .add(
            vec![message("m1", "My locker code is 4417.")],
            vec![candidate("Avery's locker code is 4417.", &["m1"])],
        )
        .await;
    f.llm
        .decisions
        .lock()
        .unwrap()
        .push_back((FactOperation::Delete, Some(secret[0])));
    f.add(
        vec![message("m2", "Please forget my locker code, 4417.")],
        vec![candidate(
            "Avery's locker code 4417 should be forgotten.",
            &["m2"],
        )],
    )
    .await;

    assert!(
        f.source_ids().await["Avery's locker code is 4417."].contains(&"m2".to_string()),
        "the request to forget is a source of the deleted memory"
    );
    let after = f.retrieved().await;
    assert!(!after.contains("4417"), "{after}");
}
