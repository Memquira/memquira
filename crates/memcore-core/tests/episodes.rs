//! Episodes: source-message-level memory.
//!
//! Flag off changes nothing. Flag on stores every added message before extraction (without changing
//! the extraction request), embeds messages of at least 20 characters into their own vector store,
//! and serves a semantic episode search with a stable tie order. Lifecycle: forget, retention,
//! export v1 / v2, import, org summary.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::SummarizationInput;
use memcore_core::retention::{ApplyRetentionInput, RetentionPolicy};
use memcore_core::{
    AddMemoryInput, Episode, EpisodeComponents, EpisodeStore, ExportUserDataInput,
    FactClassificationInput, FactExtractionInput, FactExtractionOutput, FactOperation,
    FactOperationDecision, ForgetUserInput, ImportMode, ImportUserDataInput, LlmProvider,
    MemoryEngine, MemoryMessage, MemorySession, MessageRole, OrgSummaryInput, SearchEpisodesInput,
    SearchMemoryInput, StructuredExtractionConfig, TenantContext, USER_EXPORT_FORMAT_VERSION,
    USER_EXPORT_FORMAT_VERSION_V2, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const ORG: &str = "org_episodes";
const USER: &str = "user_episodes";

fn tenant() -> TenantContext {
    TenantContext::new(ORG, USER).expect("tenant")
}

fn message(id: &str, content: &str) -> MemoryMessage {
    MemoryMessage {
        role: MessageRole::User,
        content: content.to_string(),
        name: Some("Priya".to_string()),
        timestamp: None,
        message_id: Some(id.to_string()),
    }
}

fn add_input(messages: Vec<MemoryMessage>, session: MemorySession) -> AddMemoryInput {
    AddMemoryInput {
        tenant: tenant(),
        messages,
        metadata: json!({ "session_id": "S1" }),
        session,
    }
}

fn session(id: &str) -> MemorySession {
    MemorySession {
        id: Some(id.to_string()),
        timestamp: Some(Utc.with_ymd_and_hms(2023, 5, 8, 13, 0, 0).unwrap()),
    }
}

/// Records every extraction request exactly (serialized), delegating to the mock provider.
#[derive(Default)]
struct CapturingProvider {
    inner: MockLlmProvider,
    requests: Mutex<Vec<String>>,
}

#[async_trait]
impl LlmProvider for CapturingProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_string(&input).unwrap());
        self.inner.extract_facts(input).await
    }

    async fn classify_fact_operation(
        &self,
        _input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        Ok(FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: None,
            confidence: 0.9,
        })
    }

    async fn summarize_memory(&self, _input: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

struct Harness {
    engine: MemoryEngine,
    store: Arc<MockEpisodeStore>,
    vectors: Arc<MockVectorStore>,
}

fn components(store: &Arc<MockEpisodeStore>, vectors: &Arc<MockVectorStore>) -> EpisodeComponents {
    EpisodeComponents {
        store: store.clone(),
        vectors: vectors.clone(),
    }
}

fn harness_with(llm: Arc<dyn LlmProvider>) -> Harness {
    let store = Arc::new(MockEpisodeStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm,
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_episodes(Some(components(&store, &vectors)));
    Harness {
        engine,
        store,
        vectors,
    }
}

fn harness() -> Harness {
    harness_with(Arc::new(MockLlmProvider::new()))
}

async fn stored(store: &MockEpisodeStore) -> Vec<Episode> {
    store.list_episodes(&tenant(), 1_000).await.unwrap()
}

fn search(query: &str, limit: usize) -> SearchEpisodesInput {
    SearchEpisodesInput {
        tenant: tenant(),
        query: query.to_string(),
        limit,
    }
}

#[tokio::test]
async fn with_the_flag_off_nothing_is_stored_and_episode_search_is_rejected() {
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    assert!(!engine.episodes_enabled());
    engine
        .add_memory(add_input(
            vec![message("D1:1", "I adopted a rescue greyhound last spring.")],
            session("S1"),
        ))
        .await
        .unwrap();
    assert!(matches!(
        engine.search_episodes(search("greyhound", 10)).await,
        Err(MemcoreError::ValidationError(_))
    ));
    // A v1 export, byte-compatible with the pre-episode format.
    let export = engine
        .export_user_data(ExportUserDataInput {
            tenant: tenant(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap();
    assert_eq!(export.format_version, USER_EXPORT_FORMAT_VERSION);
    assert!(export.episodes.is_none());
    assert!(
        !serde_json::to_string(&export)
            .unwrap()
            .contains("\"episodes\"")
    );
}

#[tokio::test]
async fn the_extraction_request_is_identical_with_episodes_on_and_off() {
    for structured in [None, Some(StructuredExtractionConfig::default())] {
        let messages = vec![
            message("D1:1", "I adopted a rescue greyhound last spring."),
            message("D1:2", "Ok!"),
        ];
        let off = Arc::new(CapturingProvider::default());
        let on = Arc::new(CapturingProvider::default());
        let engine_off = MemoryEngine::new(
            Arc::new(MockFactStore::new()),
            Arc::new(MockVectorStore::new()),
            off.clone(),
            Arc::new(MockEmbeddingProvider::new(8)),
        )
        .with_structured_extraction(structured);
        let h = harness_with(on.clone());
        let engine_on = h.engine.with_structured_extraction(structured);

        let out_off = engine_off
            .add_memory(add_input(messages.clone(), session("S1")))
            .await
            .unwrap();
        let out_on = engine_on
            .add_memory(add_input(messages, session("S1")))
            .await
            .unwrap();

        assert_eq!(*off.requests.lock().unwrap(), *on.requests.lock().unwrap());
        assert!(!off.requests.lock().unwrap().is_empty());
        assert_eq!(out_off.added, out_on.added);
        assert_eq!(stored(&h.store).await.len(), 2);
    }
}

#[tokio::test]
async fn episodes_are_stored_before_extraction_and_kept_when_it_fails() {
    let h = harness_with(Arc::new(
        MockLlmProvider::new().with_fail_error(MemcoreError::ProviderError("down".to_string())),
    ));
    let result = h
        .engine
        .add_memory(add_input(
            vec![message("D1:1", "I adopted a rescue greyhound last spring.")],
            session("S1"),
        ))
        .await;
    assert!(result.is_err());
    let episodes = stored(&h.store).await;
    assert_eq!(episodes.len(), 1);
    assert!(episodes[0].embedded);
}

#[tokio::test]
async fn observed_at_is_the_message_then_the_session_time_and_never_the_receipt_time() {
    let h = harness();
    let message_time = Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap();
    let mut timed = message("D1:1", "I adopted a rescue greyhound last spring.");
    timed.timestamp = Some(message_time);
    let untimed = message("D1:2", "She loves long walks in the park.");
    h.engine
        .add_memory(add_input(vec![timed, untimed], session("S1")))
        .await
        .unwrap();
    let no_session_time = message("D2:1", "We named her Juniper after the tree.");
    h.engine
        .add_memory(add_input(
            vec![no_session_time],
            MemorySession {
                id: Some("S2".to_string()),
                timestamp: None,
            },
        ))
        .await
        .unwrap();

    let by_id = |id: &str, episodes: &[Episode]| {
        episodes
            .iter()
            .find(|episode| episode.message_id.as_deref() == Some(id))
            .cloned()
            .unwrap()
    };
    let episodes = stored(&h.store).await;
    assert_eq!(by_id("D1:1", &episodes).observed_at, Some(message_time));
    assert_eq!(
        by_id("D1:2", &episodes).observed_at,
        session("S1").timestamp
    );
    let bare = by_id("D2:1", &episodes);
    assert_eq!(bare.observed_at, None);
    assert!(bare.recorded_at > Utc::now() - Duration::minutes(1));
    assert_eq!(bare.session_id.as_deref(), Some("S2"));
    assert_eq!(bare.speaker.as_deref(), Some("Priya"));
}

#[tokio::test]
async fn content_is_pii_redacted_before_storage_and_embedding() {
    let h = harness();
    let engine = h.engine.with_pii_redaction(true);
    engine
        .add_memory(add_input(
            vec![message(
                "D1:1",
                "Write to me at priya@example.com any time.",
            )],
            session("S1"),
        ))
        .await
        .unwrap();
    let episodes = stored(&h.store).await;
    assert!(!episodes[0].content.contains("priya@example.com"));
    assert!(episodes[0].content.contains("[REDACTED_EMAIL]"));
    let hits = engine
        .search_episodes(search("write to me", 5))
        .await
        .unwrap();
    assert_eq!(hits.results.len(), 1);
    assert!(
        !hits.results[0]
            .episode
            .content
            .contains("priya@example.com")
    );
}

#[tokio::test]
async fn short_messages_are_stored_but_not_embedded() {
    let h = harness();
    h.engine
        .add_memory(add_input(
            vec![
                message("D1:1", "Thanks!"),
                message("D1:2", "I adopted a rescue greyhound last spring."),
            ],
            session("S1"),
        ))
        .await
        .unwrap();
    let episodes = stored(&h.store).await;
    assert_eq!(episodes.len(), 2);
    let short = episodes.iter().find(|e| e.content == "Thanks!").unwrap();
    assert!(!short.embedded);
    let hits = h
        .engine
        .search_episodes(search("Thanks!", 10))
        .await
        .unwrap();
    assert_eq!(hits.results.len(), 1);
    assert_eq!(hits.results[0].episode.message_id.as_deref(), Some("D1:2"));
}

#[tokio::test]
async fn a_retry_stores_nothing_twice_and_embeds_what_a_failure_left_unembedded() {
    let store = Arc::new(MockEpisodeStore::new());
    let vectors = Arc::new(MockVectorStore::new());
    let facts = Arc::new(MockFactStore::new());
    let failing = MemoryEngine::new(
        facts.clone(),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(
            MockEmbeddingProvider::new(8)
                .with_fail_error(MemcoreError::ProviderError("down".to_string())),
        ),
    )
    .with_episodes(Some(components(&store, &vectors)));
    let request = || {
        add_input(
            vec![message("D1:1", "I adopted a rescue greyhound last spring.")],
            session("S1"),
        )
    };
    assert!(failing.add_memory(request()).await.is_err());
    let episodes = stored(&store).await;
    assert_eq!(episodes.len(), 1);
    assert!(!episodes[0].embedded);

    let working = MemoryEngine::new(
        facts,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_episodes(Some(components(&store, &vectors)));
    working.add_memory(request()).await.unwrap();
    working.add_memory(request()).await.unwrap();
    let episodes = stored(&store).await;
    assert_eq!(episodes.len(), 1, "a repeated message ID is stored once");
    assert!(episodes[0].embedded);
    assert_eq!(
        working
            .search_episodes(search("greyhound", 10))
            .await
            .unwrap()
            .results
            .len(),
        1
    );
}

#[tokio::test]
async fn the_same_message_id_in_another_session_or_user_is_a_separate_episode() {
    let h = harness();
    let text = "I adopted a rescue greyhound last spring.";
    h.engine
        .add_memory(add_input(vec![message("D1:1", text)], session("S1")))
        .await
        .unwrap();
    h.engine
        .add_memory(add_input(vec![message("D1:1", text)], session("S2")))
        .await
        .unwrap();
    let other_user = TenantContext::new(ORG, "someone_else").unwrap();
    h.engine
        .add_memory(AddMemoryInput {
            tenant: other_user.clone(),
            ..add_input(vec![message("D1:1", text)], session("S1"))
        })
        .await
        .unwrap();
    assert_eq!(stored(&h.store).await.len(), 2);
    assert_eq!(
        h.store.list_episodes(&other_user, 10).await.unwrap().len(),
        1
    );
    // Search is scoped to the user.
    let hits = h.engine.search_episodes(search(text, 10)).await.unwrap();
    assert_eq!(hits.results.len(), 2);
    assert!(
        hits.results
            .iter()
            .all(|result| result.episode.user_id == USER)
    );
}

/// Three messages with identical text tie exactly; two others do not.
fn tie_messages() -> Vec<MemoryMessage> {
    vec![
        message("D3:9", "Priya thanked Sam for all the support."),
        message("D1:4", "Priya thanked Sam for all the support."),
        message("D2:2", "Priya thanked Sam for all the support."),
        message("D4:1", "The greyhound's name is Juniper."),
        message("D5:5", "Sam plans a trip to Lisbon in June."),
    ]
}

async fn tie_order(order: &[usize]) -> Vec<(String, Option<String>)> {
    let h = harness();
    let messages = tie_messages();
    for &index in order {
        h.engine
            .add_memory(add_input(vec![messages[index].clone()], session("S1")))
            .await
            .unwrap();
    }
    h.engine
        .search_episodes(search("Priya thanked Sam for all the support.", 10))
        .await
        .unwrap()
        .results
        .into_iter()
        .map(|result| (result.episode.content, result.episode.message_id))
        .collect()
}

#[tokio::test]
async fn tied_episode_scores_have_one_order_across_rebuilds_and_insertion_orders() {
    let first = tie_order(&[0, 1, 2, 3, 4]).await;
    for order in [[4, 3, 2, 1, 0], [2, 0, 4, 1, 3], [1, 4, 0, 3, 2]] {
        assert_eq!(tie_order(&order).await, first);
    }
    // The three tied episodes lead, ordered by message ID.
    let ids: Vec<_> = first[..3]
        .iter()
        .map(|(_, id)| id.clone().unwrap())
        .collect();
    assert_eq!(ids, ["D1:4", "D2:2", "D3:9"]);
}

#[tokio::test]
async fn non_tied_episode_scores_keep_their_score_order() {
    let h = harness();
    h.engine
        .add_memory(add_input(tie_messages(), session("S1")))
        .await
        .unwrap();
    let results = h
        .engine
        .search_episodes(search("Sam plans a trip to Lisbon in June.", 10))
        .await
        .unwrap()
        .results;
    assert_eq!(
        results[0].episode.content,
        "Sam plans a trip to Lisbon in June."
    );
    assert!(
        results
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score)
    );
}

#[tokio::test]
async fn episode_search_follows_the_memory_search_limit_rules() {
    let h = harness();
    assert!(h.engine.search_episodes(search("dogs", 0)).await.is_err());
    assert!(h.engine.search_episodes(search("dogs", 50)).await.is_ok());
    assert!(h.engine.search_episodes(search("dogs", 51)).await.is_err());
    let raised = h.engine.with_search_max_limit(100).unwrap();
    assert!(raised.search_episodes(search("dogs", 100)).await.is_ok());
    assert!(raised.search_episodes(search("dogs", 101)).await.is_err());
}

#[tokio::test]
async fn fact_search_is_unchanged_by_episodes() {
    let messages = vec![
        message("D1:1", "I adopted a rescue greyhound last spring."),
        message("D1:2", "Sam plans a trip to Lisbon in June."),
    ];
    let plain = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    let h = harness();
    for engine in [&plain, &h.engine] {
        engine
            .add_memory(add_input(messages.clone(), session("S1")))
            .await
            .unwrap();
    }
    let query = || SearchMemoryInput {
        tenant: tenant(),
        query: "greyhound".to_string(),
        limit: 10,
        memory_types: None,
        metadata_filter: None,
    };
    let contents = |output: memcore_core::SearchMemoryOutput| {
        output
            .results
            .into_iter()
            .map(|result| (result.content, result.score))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        contents(plain.search_memory(query()).await.unwrap()),
        contents(h.engine.search_memory(query()).await.unwrap())
    );
}

#[tokio::test]
async fn forget_user_removes_episodes_and_their_vectors() {
    let h = harness();
    h.engine
        .add_memory(add_input(tie_messages(), session("S1")))
        .await
        .unwrap();
    h.engine
        .forget_user(ForgetUserInput { tenant: tenant() })
        .await
        .unwrap();
    assert!(stored(&h.store).await.is_empty());
    let leftover = h
        .vectors
        .search_vectors(memcore_core::VectorSearchQuery {
            tenant: tenant(),
            embedding: vec![0.1; 8],
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .unwrap();
    assert!(leftover.is_empty());
}

#[tokio::test]
async fn retention_deletes_episodes_stored_before_the_fact_window() {
    let h = harness();
    h.engine
        .add_memory(add_input(
            vec![message("D1:1", "I adopted a rescue greyhound last spring.")],
            session("S1"),
        ))
        .await
        .unwrap();
    // An episode stored long ago (inserted directly with an old `recorded_at`).
    let old = Episode {
        id: Uuid::new_v4(),
        org_id: ORG.to_string(),
        user_id: USER.to_string(),
        session_id: Some("S0".to_string()),
        message_id: Some("D0:1".to_string()),
        speaker: None,
        role: MessageRole::User,
        content: "An old message from last year.".to_string(),
        observed_at: None,
        recorded_at: Utc::now() - Duration::days(400),
        embedded: false,
        metadata: json!({}),
    };
    h.store
        .insert_episodes(&tenant(), vec![old.clone()])
        .await
        .unwrap();
    let policy = RetentionPolicy {
        enabled: true,
        fact_retention_days: Some(365),
        event_retention_days: None,
    };
    let dry = h
        .engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant(),
            policy,
            dry_run: true,
        })
        .await
        .unwrap();
    assert_eq!(dry.episodes_matched, Some(1));
    assert_eq!(dry.episodes_deleted, Some(0));
    assert_eq!(stored(&h.store).await.len(), 2);

    let applied = h
        .engine
        .apply_retention(ApplyRetentionInput {
            tenant: tenant(),
            policy,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(applied.episodes_deleted, Some(1));
    let left = stored(&h.store).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].message_id.as_deref(), Some("D1:1"));
}

async fn exported(engine: &MemoryEngine) -> memcore_core::UserMemoryExport {
    engine
        .export_user_data(ExportUserDataInput {
            tenant: tenant(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap()
}

fn import(export: memcore_core::UserMemoryExport, mode: ImportMode) -> ImportUserDataInput {
    ImportUserDataInput {
        tenant: tenant(),
        export,
        mode,
        restore_events: false,
        dry_run: false,
    }
}

#[tokio::test]
async fn export_is_v2_with_episodes_and_imports_into_enabled_and_disabled_servers() {
    let source = harness();
    source
        .engine
        .add_memory(add_input(tie_messages(), session("S1")))
        .await
        .unwrap();
    let export = exported(&source.engine).await;
    assert_eq!(export.format_version, USER_EXPORT_FORMAT_VERSION_V2);
    assert_eq!(export.episodes.as_ref().unwrap().len(), 5);

    // Into a server with episodes: stored again and embedded, so searchable.
    let target = harness();
    let output = target
        .engine
        .import_user_data(import(export.clone(), ImportMode::Append))
        .await
        .unwrap();
    assert_eq!(output.imported_episodes, Some(5));
    assert_eq!(output.skipped_episodes, Some(0));
    assert_eq!(stored(&target.store).await.len(), 5);
    assert!(
        !target
            .engine
            .search_episodes(search("Juniper", 10))
            .await
            .unwrap()
            .results
            .is_empty()
    );
    // Importing the same export again stores nothing twice. (Contract
    // change): its facts are already stored with identical content, so the re-application is
    // rejected before any write; before R7 it succeeded, re-keying the facts into duplicates, and
    // only the episodes were skipped by their client message IDs.
    let err = target
        .engine
        .import_user_data(import(export.clone(), ImportMode::Append))
        .await
        .expect_err("re-application rejected");
    assert!(err.is_import_already_applied());
    assert_eq!(stored(&target.store).await.len(), 5);

    // Into an engine with no episode storage at all: facts import, episodes are counted as skipped.
    // (The API always wires episode storage; restores them there even with episodes off.)
    let disabled = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    let output = disabled
        .import_user_data(import(export, ImportMode::Append))
        .await
        .unwrap();
    assert_eq!(output.imported_episodes, Some(0));
    assert_eq!(output.skipped_episodes, Some(5));
}

#[tokio::test]
async fn a_v1_export_imports_into_an_episode_server_and_replace_clears_episodes() {
    let v1_source = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    v1_source
        .add_memory(add_input(
            vec![message("D1:1", "I adopted a rescue greyhound last spring.")],
            session("S1"),
        ))
        .await
        .unwrap();
    let v1 = exported(&v1_source).await;
    assert_eq!(v1.format_version, USER_EXPORT_FORMAT_VERSION);

    let target = harness();
    target
        .engine
        .add_memory(add_input(tie_messages(), session("S1")))
        .await
        .unwrap();
    let output = target
        .engine
        .import_user_data(import(v1, ImportMode::Replace))
        .await
        .unwrap();
    assert!(output.replaced_existing);
    assert_eq!(output.imported_episodes, None);
    assert!(stored(&target.store).await.is_empty());
}

#[tokio::test]
async fn the_org_summary_counts_episodes_separately() {
    let h = harness();
    h.engine
        .add_memory(add_input(tie_messages(), session("S1")))
        .await
        .unwrap();
    let summary = h
        .engine
        .get_org_summary(OrgSummaryInput {
            org_id: ORG.to_string(),
        })
        .await
        .unwrap();
    assert_eq!(summary.total_episodes, Some(5));

    let plain = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(8)),
    );
    let summary = plain
        .get_org_summary(OrgSummaryInput {
            org_id: ORG.to_string(),
        })
        .await
        .unwrap();
    assert_eq!(summary.total_episodes, None);
}
