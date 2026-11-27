//! Deleting one memory withholds the messages it was extracted from.
//!
//! With episodes on, retrieval merges facts with the stored messages themselves (episode search,
//! candidates and the server-side context). Before this fix a deleted fact disappeared from search,
//! lists and the fact leg, while the message it came from still returned the same information as
//! an episode. Now episode retrieval leaves out every message a deleted memory of the same user
//! cites in `source_message_ids`: the episode rows are kept (export, retention, full erasure are
//! unchanged), other memories from the same message stay live facts, and the state is derived from
//! stored rows only, so it survives a restart. Invented data only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memquira_common::MemquiraResult;
use memquira_core::d76_context::D76ContextInput;
use memquira_core::ports::SummarizationInput;
use memquira_core::{
    AddMemoryInput, CandidateFact, DeleteMemoryInput, EpisodeComponents, EpisodeStore,
    FactClassificationInput, FactExtractionInput, FactExtractionOutput, FactOperation,
    FactOperationDecision, FactStructure, ForgetUserInput, ListMemoriesInput, LlmProvider,
    MemoryEngine, MemoryMessage, MemorySession, MemoryType, MessageRole, SearchEpisodesInput,
    SearchMemoryInput, StructuredExtractionConfig, TenantContext,
};
use memquira_providers::{MockEmbeddingProvider, MockLlmProvider};
use memquira_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const ORG: &str = "org_delete_episodes";
const ALICE: &str = "alice";
const BOB: &str = "bob";
const QUERY: &str = "What do we know about the user?";

fn tenant(user: &str) -> TenantContext {
    TenantContext::new(ORG, user).expect("tenant")
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

/// An extraction scripted per request: the next queued candidate list (with the provenance the
/// test gives it), else the mock's structured default (one fact per user message, citing it).
/// Classification is an Add, or a NoOp against the first related fact when `noop` is set.
#[derive(Default)]
struct Scripted {
    inner: MockLlmProvider,
    queue: Mutex<VecDeque<Vec<CandidateFact>>>,
    noop: Mutex<bool>,
}

impl Scripted {
    fn then(&self, candidates: Vec<CandidateFact>) {
        self.queue.lock().unwrap().push_back(candidates);
    }
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemquiraResult<FactExtractionOutput> {
        if let Some(candidates) = self.queue.lock().unwrap().pop_front() {
            return Ok(FactExtractionOutput::from_candidates(candidates));
        }
        self.inner.extract_facts(input).await
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemquiraResult<FactOperationDecision> {
        let noop = *self.noop.lock().unwrap();
        Ok(match input.existing_facts.first() {
            Some(existing) if noop => FactOperationDecision {
                operation: FactOperation::NoOp,
                target_fact_id: Some(existing.id),
                reason: Some("already known".to_string()),
                confidence: 0.9,
            },
            _ => FactOperationDecision {
                operation: FactOperation::Add,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            },
        })
    }

    async fn summarize_memory(&self, _input: SummarizationInput) -> MemquiraResult<String> {
        Ok(String::new())
    }
}

fn candidate(content: &str, cites: &[&str]) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.8, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some("Avery".to_string()),
            source_message_ids: cites.iter().map(|id| id.to_string()).collect(),
            ..FactStructure::default()
        })
}

/// The durable stores of one deployment; `engine()` is one process serving them.
struct Deployment {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_vectors: Arc<MockVectorStore>,
    llm: Arc<Scripted>,
}

impl Deployment {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
            episodes: Arc::new(MockEpisodeStore::new()),
            episode_vectors: Arc::new(MockVectorStore::new()),
            llm: Arc::new(Scripted::default()),
        }
    }

    fn engine(&self) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            self.llm.clone(),
            Arc::new(MockEmbeddingProvider::new(64)),
        )
        .with_episodes(Some(EpisodeComponents {
            store: self.episodes.clone(),
            vectors: self.episode_vectors.clone(),
        }))
        .with_structured_extraction(Some(StructuredExtractionConfig::default()))
        .with_search_max_limit(100)
        .expect("search limit")
    }
}

async fn add(
    engine: &MemoryEngine,
    user: &str,
    session: &str,
    messages: Vec<MemoryMessage>,
) -> Vec<Uuid> {
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant(user),
            messages,
            metadata: json!({}),
            session: MemorySession {
                id: Some(session.to_string()),
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

async fn delete(engine: &MemoryEngine, user: &str, id: Uuid) {
    engine
        .delete_memory(DeleteMemoryInput {
            tenant: tenant(user),
            memory_id: id,
        })
        .await
        .expect("delete");
}

/// Everything one user's retrieval returns for the query: the episode texts, the server-side
/// context, the fact search and the memory list.
struct Retrieved {
    episodes: Vec<String>,
    context: String,
    facts: Vec<String>,
    listed: Vec<String>,
}

impl Retrieved {
    fn mentions(&self, text: &str) -> bool {
        self.episodes.iter().any(|e| e.contains(text))
            || self.context.contains(text)
            || self.facts.iter().any(|f| f.contains(text))
            || self.listed.iter().any(|f| f.contains(text))
    }
}

async fn retrieve(engine: &MemoryEngine, user: &str) -> Retrieved {
    let episodes = engine
        .search_episodes(SearchEpisodesInput {
            tenant: tenant(user),
            query: QUERY.to_string(),
            limit: 100,
        })
        .await
        .expect("episodes")
        .results
        .into_iter()
        .map(|result| result.episode.content)
        .collect();
    let context = engine
        .d76_context(D76ContextInput {
            tenant: tenant(user),
            query: QUERY.to_string(),
            token_budget: 4000,
        })
        .await
        .expect("context")
        .context
        .text;
    let facts = engine
        .search_memory(SearchMemoryInput {
            tenant: tenant(user),
            query: QUERY.to_string(),
            limit: 100,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search")
        .results
        .into_iter()
        .map(|result| result.content)
        .collect();
    let listed = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant(user),
            memory_type: None,
            query_text: None,
            limit: 100,
            cursor: None,
            include_deleted: false,
        })
        .await
        .expect("list")
        .memories
        .into_iter()
        .map(|fact| fact.content)
        .collect();
    Retrieved {
        episodes,
        context,
        facts,
        listed,
    }
}

const WHEEL: &str = "I keep a pottery wheel in the garage.";
const RABBIT: &str = "My rabbit is called Pebble.";

/// 1, 4, 5: one fact from one message. After the delete neither the fact nor its message comes
/// back through any retrieval path (episode search, context, fact search, list).
#[tokio::test]
async fn one_fact_in_one_message_is_gone_from_every_retrieval_path() {
    let d = Deployment::new();
    let engine = d.engine();
    let ids = add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    assert_eq!(ids.len(), 1);
    let before = retrieve(&engine, ALICE).await;
    assert!(
        before.episodes.iter().any(|e| e.as_str() == WHEEL)
            && before.context.contains("pottery wheel")
    );

    delete(&engine, ALICE, ids[0]).await;
    let after = retrieve(&engine, ALICE).await;
    assert!(!after.mentions("pottery wheel"), "{}", after.context);
    // The source message is withheld, not erased.
    assert_eq!(
        d.episodes
            .list_episodes(&tenant(ALICE), 100)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// 2: two independent facts from one message. Deleting one withholds the message (it states both)
/// but the other memory stays a live, retrievable fact.
#[tokio::test]
async fn deleting_one_of_two_facts_from_a_message_keeps_the_other_fact() {
    let d = Deployment::new();
    let engine = d.engine();
    d.llm.then(vec![
        candidate("Avery keeps a pottery wheel in the garage.", &["m1"]),
        candidate("Avery has a rabbit called Pebble.", &["m1"]),
    ]);
    let both = format!("{WHEEL} {RABBIT}");
    let ids = add(&engine, ALICE, "s1", vec![message("m1", &both)]).await;
    assert_eq!(ids.len(), 2);

    delete(&engine, ALICE, ids[0]).await;
    let after = retrieve(&engine, ALICE).await;
    assert!(!after.mentions("pottery wheel"), "{}", after.context);
    assert!(after.facts.iter().any(|f| f.contains("Pebble")));
    assert!(after.listed.iter().any(|f| f.contains("Pebble")));
    assert!(
        after.context.contains("rabbit called Pebble"),
        "{}",
        after.context
    );
}

/// 3: the same fact stated in several messages. A request whose two messages both state it cites
/// both; a later restatement the engine stores nothing new for (an exact duplicate) is recorded as
/// a further source. Deleting the memory withholds all three messages.
#[tokio::test]
async fn the_same_fact_in_several_messages_withholds_every_source() {
    let d = Deployment::new();
    let engine = d.engine();
    d.llm.then(vec![candidate(
        "Avery keeps a pottery wheel.",
        &["m1", "m2"],
    )]);
    let ids = add(
        &engine,
        ALICE,
        "s1",
        vec![
            message("m1", WHEEL),
            message("m2", "The pottery wheel lives in my garage, as I said."),
        ],
    )
    .await;
    assert_eq!(ids.len(), 1);
    // A restatement in another session: an exact duplicate of the stored fact.
    d.llm
        .then(vec![candidate("Avery keeps a pottery wheel.", &["m3"])]);
    let again = add(
        &engine,
        ALICE,
        "s2",
        vec![message("m3", "Still have that pottery wheel.")],
    )
    .await;
    assert!(again.is_empty(), "the restatement stores nothing new");
    assert_eq!(retrieve(&engine, ALICE).await.episodes.len(), 3);

    delete(&engine, ALICE, ids[0]).await;
    let after = retrieve(&engine, ALICE).await;
    assert!(after.episodes.is_empty(), "{:?}", after.episodes);
    assert!(!after.mentions("pottery wheel"), "{}", after.context);
}

/// 3 (classifier): a restatement the classifier judges already known (a no-op against the stored
/// fact) is recorded as a further source too.
#[tokio::test]
async fn a_restatement_the_classifier_ignores_is_withheld_with_the_fact() {
    let d = Deployment::new();
    let engine = d.engine();
    d.llm.then(vec![candidate(
        "Avery keeps a pottery wheel in the garage.",
        &["m1"],
    )]);
    let ids = add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    *d.llm.noop.lock().unwrap() = true;
    // Related to the stored fact (the legacy matcher's text search finds it), not a duplicate.
    d.llm
        .then(vec![candidate("Avery keeps a pottery wheel", &["m2"])]);
    let again = add(
        &engine,
        ALICE,
        "s1",
        vec![message("m2", "Yes, the pottery wheel is mine.")],
    )
    .await;
    *d.llm.noop.lock().unwrap() = false;
    assert!(again.is_empty());

    delete(&engine, ALICE, ids[0]).await;
    let after = retrieve(&engine, ALICE).await;
    assert!(
        !after.mentions("pottery wheel"),
        "{:?} / {}",
        after.episodes,
        after.context
    );
}

/// A fact the model did not attribute, from a one-message request, cites that message.
#[tokio::test]
async fn an_unattributed_fact_from_a_single_message_cites_it() {
    let d = Deployment::new();
    let engine = d.engine();
    d.llm
        .then(vec![candidate("Avery keeps a pottery wheel.", &[])]);
    let ids = add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    let listed = engine
        .list_memories(ListMemoriesInput {
            tenant: tenant(ALICE),
            memory_type: None,
            query_text: None,
            limit: 10,
            cursor: None,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(listed.memories[0].structure.source_message_ids, vec!["m1"]);
    delete(&engine, ALICE, ids[0]).await;
    assert!(!retrieve(&engine, ALICE).await.mentions("pottery wheel"));
}

/// 6: the withholding is derived from stored rows, so a new process serving the same stores
/// (a restart) withholds the same messages.
#[tokio::test]
async fn deletion_holds_after_a_restart() {
    let d = Deployment::new();
    let ids = add(
        &d.engine(),
        ALICE,
        "s1",
        vec![message("m1", WHEEL), message("m2", RABBIT)],
    )
    .await;
    delete(&d.engine(), ALICE, ids[0]).await;
    let restarted = d.engine();
    let after = retrieve(&restarted, ALICE).await;
    assert!(!after.mentions("pottery wheel"), "{}", after.context);
    assert!(after.mentions("Pebble"));
}

/// Repeated deletion is safe: a second delete of the same id changes nothing.
#[tokio::test]
async fn deleting_twice_is_safe() {
    let d = Deployment::new();
    let engine = d.engine();
    let ids = add(
        &engine,
        ALICE,
        "s1",
        vec![message("m1", WHEEL), message("m2", RABBIT)],
    )
    .await;
    delete(&engine, ALICE, ids[0]).await;
    let second = engine
        .delete_memory(DeleteMemoryInput {
            tenant: tenant(ALICE),
            memory_id: ids[0],
        })
        .await;
    assert!(second.is_ok() || matches!(second, Err(memquira_common::MemquiraError::NotFound(_))));
    let after = retrieve(&engine, ALICE).await;
    assert!(!after.mentions("pottery wheel"));
    assert!(after.mentions("Pebble"));
}

/// 7: explicit re-ingestion. Resending the same message (same id) stores no new episode and keeps
/// it withheld; telling the information again in a new message stores it again, and the new
/// message and memory are retrievable while the old message stays withheld.
#[tokio::test]
async fn re_ingestion_after_a_delete() {
    let d = Deployment::new();
    let engine = d.engine();
    let ids = add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    delete(&engine, ALICE, ids[0]).await;

    // The same message again (a client retry): its episode is the stored one, still withheld.
    add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    let resent = retrieve(&engine, ALICE).await;
    assert!(resent.episodes.is_empty(), "{:?}", resent.episodes);
    assert_eq!(
        d.episodes
            .list_episodes(&tenant(ALICE), 100)
            .await
            .unwrap()
            .len(),
        1
    );

    // Told again in a new message: stored and retrievable again, as a new memory.
    let told = "I still keep a pottery wheel, in the shed now.";
    let new_ids = add(&engine, ALICE, "s2", vec![message("m9", told)]).await;
    assert_eq!(new_ids.len(), 1);
    assert_ne!(new_ids[0], ids[0]);
    let after = retrieve(&engine, ALICE).await;
    assert_eq!(after.episodes, vec![told.to_string()]);
    assert!(after.facts.iter().any(|f| f.as_str() == told));
}

/// 8: full-user erasure still removes everything, and a user who starts again afterwards is not
/// affected by deletions made before the erasure.
#[tokio::test]
async fn full_user_erasure_is_unchanged() {
    let d = Deployment::new();
    let engine = d.engine();
    let ids = add(
        &engine,
        ALICE,
        "s1",
        vec![message("m1", WHEEL), message("m2", RABBIT)],
    )
    .await;
    delete(&engine, ALICE, ids[0]).await;
    engine
        .forget_user(ForgetUserInput {
            tenant: tenant(ALICE),
        })
        .await
        .expect("forget");
    assert!(
        d.episodes
            .list_episodes(&tenant(ALICE), 100)
            .await
            .unwrap()
            .is_empty()
    );
    let erased = retrieve(&engine, ALICE).await;
    assert!(erased.episodes.is_empty() && erased.facts.is_empty() && erased.listed.is_empty());

    // The same client message id after erasure belongs to new data and is retrievable.
    add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    assert!(
        retrieve(&engine, ALICE)
            .await
            .episodes
            .iter()
            .any(|e| e.as_str() == WHEEL)
    );
}

/// 9: tenant isolation. Alice's deletion never withholds Bob's messages, even with the same client
/// message id, and Bob cannot delete Alice's memory.
#[tokio::test]
async fn deletions_are_isolated_per_user() {
    let d = Deployment::new();
    let engine = d.engine();
    let alice = add(&engine, ALICE, "s1", vec![message("m1", WHEEL)]).await;
    add(
        &engine,
        BOB,
        "s1",
        vec![message("m1", "I keep a pottery wheel on my balcony.")],
    )
    .await;

    assert!(
        engine
            .delete_memory(DeleteMemoryInput {
                tenant: tenant(BOB),
                memory_id: alice[0],
            })
            .await
            .is_err()
    );
    assert!(retrieve(&engine, ALICE).await.mentions("pottery wheel"));

    delete(&engine, ALICE, alice[0]).await;
    assert!(!retrieve(&engine, ALICE).await.mentions("pottery wheel"));
    let bob = retrieve(&engine, BOB).await;
    assert!(bob.episodes.iter().any(|e| e.contains("balcony")));
    assert!(bob.context.contains("balcony"));

    // Another organization with the same user and message ids is unaffected too.
    let other = TenantContext::new("org_other", ALICE).unwrap();
    engine
        .add_memory(AddMemoryInput {
            tenant: other.clone(),
            messages: vec![message("m1", WHEEL)],
            metadata: json!({}),
            session: MemorySession {
                id: Some("s1".to_string()),
                timestamp: None,
            },
        })
        .await
        .unwrap();
    let found = engine
        .search_episodes(SearchEpisodesInput {
            tenant: other,
            query: QUERY.to_string(),
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(found.results.len(), 1);
}

/// 10: unrelated memories and messages are untouched by a delete.
#[tokio::test]
async fn unrelated_memories_and_messages_are_preserved() {
    let d = Deployment::new();
    let engine = d.engine();
    let ids = add(
        &engine,
        ALICE,
        "s1",
        vec![
            message("m1", WHEEL),
            message("m2", RABBIT),
            message("m3", "I teach Spanish on Tuesday evenings."),
        ],
    )
    .await;
    assert_eq!(ids.len(), 3);
    let before = retrieve(&engine, ALICE).await;

    delete(&engine, ALICE, ids[0]).await;
    let after = retrieve(&engine, ALICE).await;
    assert_eq!(after.episodes.len(), before.episodes.len() - 1);
    assert_eq!(after.facts.len(), before.facts.len() - 1);
    for kept in ["Pebble", "Spanish"] {
        assert!(after.episodes.iter().any(|e| e.contains(kept)), "{kept}");
        assert!(after.facts.iter().any(|f| f.contains(kept)), "{kept}");
        assert!(after.context.contains(kept), "{kept}: {}", after.context);
    }
    assert!(!after.mentions("pottery wheel"));
}
