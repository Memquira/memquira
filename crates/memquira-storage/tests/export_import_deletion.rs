//! Export and import keep deletion state.
//!
//! An `include_deleted` export used to carry deleted facts with nothing marking them deleted, and
//! the import stored and indexed every exported fact as active, so a deleted memory came back,
//! and with it the source message that deletion withholds from retrieval. Now:
//!
//! - an export marks each deleted fact (`deleted_facts`) and lists the exported messages a deleted
//!   memory cites (`withheld_message_ids`);
//! - an import stores a deleted fact deleted, with its provenance, and never indexes it. It also
//!   treats as deleted a fact the export carries a delete event for (exports written before the
//!   marker) and, in replace mode, a fact the target already holds deleted;
//! - a withheld message is restored only when the import stores, deleted, a fact that cites it,
//!   so retrieval keeps withholding it; otherwise it is skipped.
//!
//! Every scenario runs on the mock store and on SQLite, and on PostgreSQL with
//! `MEMQUIRA_TEST_POSTGRES_URL` and the `postgres` feature. Invented data only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use memquira_common::{MemquiraError, MemquiraResult};
use memquira_core::d76_context::D76ContextInput;
use memquira_core::ports::{
    EmbeddingProvider, FactSearchQuery, FactStore, SummarizationInput, VectorSearchQuery,
    VectorStore,
};
use memquira_core::{
    AddMemoryInput, CandidateFact, CompleteExportInput, DeleteMemoryInput, EpisodeComponents,
    EpisodeStore, ExportUserDataInput, Fact, FactClassificationInput, FactExtractionInput,
    FactExtractionOutput, FactOperation, FactOperationDecision, FactStructure, ImportMode,
    ImportUserDataInput, ImportUserDataOutput, LlmProvider, MemoryEngine, MemoryMessage,
    MemorySession, MemorySource, MemoryType, MessageRole, SearchEpisodesInput, SearchMemoryInput,
    StructuredExtractionConfig, TenantContext, UserMemoryExport,
};
use memquira_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
use memquira_storage::{
    MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore, SqliteEpisodeStore,
    SqliteFactStore,
};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

const ORG: &str = "org_export_deletion";
const ALICE: &str = "alice";
const QUERY: &str = "What do we know about the user?";
const DIMS: usize = 64;

/// The deleted memory, the message it came from, and what must never come back.
const SECRET_FACT: &str = "Avery's locker code is 4417.";
const SECRET_MESSAGE: &str = "My locker code is 4417.";
const SECRET: &str = "4417";
const KEPT_FACT: &str = "Avery prefers green tea.";
const KEPT: &str = "green tea";
const HISTORY: &str = "Avery lived in Lisbon.";

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

fn candidate(content: &str, cites: &[&str]) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.8, None, json!({}))
        .unwrap()
        .with_structure(FactStructure {
            subject: Some("Avery".to_string()),
            source_message_ids: cites.iter().map(|id| id.to_string()).collect(),
            ..FactStructure::default()
        })
}

/// Extraction returns the next scripted candidate list; classification is always an Add.
#[derive(Default)]
struct Scripted {
    queue: Mutex<VecDeque<Vec<CandidateFact>>>,
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn extract_facts(
        &self,
        _input: FactExtractionInput,
    ) -> MemquiraResult<FactExtractionOutput> {
        let next = self.queue.lock().unwrap().pop_front().unwrap_or_default();
        Ok(FactExtractionOutput::from_candidates(next))
    }

    async fn classify_fact_operation(
        &self,
        _input: FactClassificationInput,
    ) -> MemquiraResult<FactOperationDecision> {
        Ok(FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: None,
            confidence: 0.9,
        })
    }

    async fn summarize_memory(&self, _input: SummarizationInput) -> MemquiraResult<String> {
        Ok(String::new())
    }
}

/// The deterministic test embedding of the provider mocks (bytes hashed into slots, normalised).
fn embed(text: &str) -> Vec<f32> {
    let mut embedding = vec![0.0_f32; DIMS];
    for (index, byte) in text.as_bytes().iter().enumerate() {
        let slot = (index.wrapping_mul(17) ^ (*byte as usize).wrapping_mul(31)) % DIMS;
        embedding[slot] += f32::from(*byte) / 255.0;
    }
    let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        embedding.iter_mut().for_each(|v| *v /= norm);
    } else {
        embedding[0] = 1.0;
    }
    embedding
}

struct Embed;

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemquiraResult<Vec<f32>> {
        Ok(embed(text))
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemquiraResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| embed(text)).collect())
    }

    fn dimensions(&self) -> usize {
        DIMS
    }
}

/// One server: its durable stores (fact and episode rows on the backend under test; vectors and
/// events in memory) and the engine serving them.
struct Server {
    facts: Arc<dyn FactStore>,
    vectors: Arc<MockVectorStore>,
    engine: MemoryEngine,
    llm: Arc<Scripted>,
}

impl Server {
    fn new(facts: Arc<dyn FactStore>, episodes: Arc<dyn EpisodeStore>) -> Self {
        let vectors = Arc::new(MockVectorStore::new());
        let llm = Arc::new(Scripted::default());
        let engine =
            MemoryEngine::new(facts.clone(), vectors.clone(), llm.clone(), Arc::new(Embed))
                .with_episodes(Some(EpisodeComponents {
                    store: episodes,
                    vectors: Arc::new(MockVectorStore::new()),
                }))
                .with_event_store(Arc::new(MockMemoryEventStore::new()))
                .with_structured_extraction(Some(StructuredExtractionConfig::default()))
                .with_search_max_limit(100)
                .expect("search limit");
        Self {
            facts,
            vectors,
            engine,
            llm,
        }
    }

    async fn remember(&self, user: &str, id: &str, said: &str, fact: &str) -> Uuid {
        self.llm
            .queue
            .lock()
            .unwrap()
            .push_back(vec![candidate(fact, &[id])]);
        let stored = self
            .engine
            .add_memory(AddMemoryInput {
                tenant: tenant(user),
                messages: vec![message(id, said)],
                metadata: json!({}),
                session: MemorySession {
                    id: Some("s1".to_string()),
                    timestamp: None,
                },
            })
            .await
            .expect("add")
            .memories;
        assert_eq!(stored.len(), 1, "one memory per message");
        stored[0].id
    }

    async fn delete(&self, user: &str, id: Uuid) {
        self.engine
            .delete_memory(DeleteMemoryInput {
                tenant: tenant(user),
                memory_id: id,
            })
            .await
            .expect("delete");
    }

    async fn export(&self, user: &str, include_deleted: bool) -> UserMemoryExport {
        self.engine
            .export_user_data(ExportUserDataInput {
                tenant: tenant(user),
                include_events: true,
                include_deleted,
            })
            .await
            .expect("export")
    }

    async fn export_complete(&self, user: &str) -> UserMemoryExport {
        self.engine
            .export_user_data_complete(CompleteExportInput {
                tenant: tenant(user),
                include_deleted: true,
                page_size: 2,
                max_records: 1_000,
            })
            .await
            .expect("complete export")
    }

    async fn import(
        &self,
        user: &str,
        export: &UserMemoryExport,
        mode: ImportMode,
        dry_run: bool,
    ) -> MemquiraResult<ImportUserDataOutput> {
        self.engine
            .import_user_data(ImportUserDataInput {
                tenant: tenant(user),
                export: export.clone(),
                mode,
                restore_events: true,
                dry_run,
            })
            .await
    }

    /// Dry run, then the import: both must report the same counts.
    async fn import_checked(
        &self,
        user: &str,
        export: &UserMemoryExport,
        mode: ImportMode,
    ) -> ImportUserDataOutput {
        let planned = self
            .import(user, export, mode, true)
            .await
            .expect("dry run");
        let done = self
            .import(user, export, mode, false)
            .await
            .expect("import");
        assert_eq!(
            (
                planned.imported_facts,
                planned.imported_deleted_facts,
                planned.imported_episodes,
                planned.skipped_episodes,
            ),
            (
                done.imported_facts,
                done.imported_deleted_facts,
                done.imported_episodes,
                done.skipped_episodes,
            ),
            "the dry run reports what the import does"
        );
        done
    }

    /// The user's fact rows: (live contents, deleted contents), each sorted.
    async fn rows(&self, user: &str) -> (Vec<String>, Vec<String>) {
        let read = |include_deleted| {
            let mut query = FactSearchQuery::new(tenant(user), 100);
            query.include_deleted = include_deleted;
            self.facts.search_facts(query)
        };
        let all = read(true).await.expect("all rows");
        let live = read(false).await.expect("live rows");
        let live_ids: Vec<Uuid> = live.iter().map(|f| f.id).collect();
        let mut deleted: Vec<String> = all
            .iter()
            .filter(|f| !live_ids.contains(&f.id))
            .map(|f| f.content.clone())
            .collect();
        let mut live: Vec<String> = live.into_iter().map(|f| f.content).collect();
        live.sort();
        deleted.sort();
        (live, deleted)
    }

    async fn fact_named(&self, user: &str, content: &str) -> Fact {
        let mut query = FactSearchQuery::new(tenant(user), 100);
        query.include_deleted = true;
        self.facts
            .search_facts(query)
            .await
            .expect("rows")
            .into_iter()
            .find(|f| f.content == content)
            .unwrap_or_else(|| panic!("no row {content:?}"))
    }

    /// The contents of the user's indexed fact vectors.
    async fn indexed(&self, user: &str) -> Vec<String> {
        self.vectors
            .search_vectors(VectorSearchQuery {
                tenant: tenant(user),
                embedding: embed(QUERY),
                limit: 1_000,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .expect("vectors")
            .into_iter()
            .map(|hit| hit.content)
            .collect()
    }

    /// Everything ordinary retrieval returns for the user, as one text: episode search, the
    /// server-side context, fact search and the memory list.
    async fn retrieved(&self, user: &str) -> String {
        let mut text = Vec::new();
        for hit in self
            .engine
            .search_episodes(SearchEpisodesInput {
                tenant: tenant(user),
                query: QUERY.to_string(),
                limit: 100,
            })
            .await
            .expect("episodes")
            .results
        {
            text.push(hit.episode.content);
        }
        text.push(
            self.engine
                .d76_context(D76ContextInput {
                    tenant: tenant(user),
                    query: QUERY.to_string(),
                    token_budget: 4_000,
                })
                .await
                .expect("context")
                .context
                .text,
        );
        for hit in self
            .engine
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
        {
            text.push(hit.content);
        }
        for fact in self
            .engine
            .list_memories(memquira_core::ListMemoriesInput {
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
        {
            text.push(fact.content);
        }
        text.join("\n")
    }

    /// The deleted memory and its message stay out of ordinary retrieval; the kept one is there.
    async fn assert_clean(&self, user: &str, label: &str) {
        let retrieved = self.retrieved(user).await;
        assert!(
            !retrieved.contains(SECRET),
            "{label}: the deleted memory came back:\n{retrieved}"
        );
        assert!(
            retrieved.contains(KEPT),
            "{label}: the kept memory is missing:\n{retrieved}"
        );
        assert!(
            !self.indexed(user).await.iter().any(|c| c.contains(SECRET)),
            "{label}: the deleted memory is indexed"
        );
    }
}

/// The source user: a kept memory, a historical (superseded) one, and a memory deleted after it
/// was stored. Returns the deleted memory's id.
async fn seed(server: &Server) -> Uuid {
    let secret = server
        .remember(ALICE, "m1", SECRET_MESSAGE, SECRET_FACT)
        .await;
    server
        .remember(ALICE, "m2", "I prefer green tea.", KEPT_FACT)
        .await;
    let earlier = Utc::now() - Duration::days(30);
    let history = Fact::new(
        Uuid::new_v4(),
        ORG,
        ALICE,
        MemoryType::Profile,
        HISTORY,
        None,
        MemorySource::UserMessage,
        0.9,
        0.5,
        None,
        Some(earlier + Duration::days(10)),
        earlier,
        earlier,
        json!({}),
    )
    .unwrap();
    server
        .facts
        .insert_fact(
            &tenant(ALICE),
            Fact {
                superseded_at: Some(earlier + Duration::days(10)),
                ..history
            },
        )
        .await
        .expect("history");
    secret
}

/// The state every import of the deleted memory must leave: two live rows (the kept memory and
/// the history), the deleted one stored deleted with its provenance, and retrieval clean.
async fn assert_restored(server: &Server, user: &str, label: &str) {
    let (live, deleted) = server.rows(user).await;
    assert_eq!(
        live,
        vec![HISTORY.to_string(), KEPT_FACT.to_string()],
        "{label}: live rows"
    );
    assert_eq!(
        deleted,
        vec![SECRET_FACT.to_string()],
        "{label}: deleted rows"
    );
    let secret = server.fact_named(user, SECRET_FACT).await;
    assert_eq!(
        secret.structure.source_message_ids,
        vec!["m1".to_string()],
        "{label}: provenance"
    );
    let history = server.fact_named(user, HISTORY).await;
    assert!(
        history.superseded_at.is_some() && history.invalid_at.is_some(),
        "{label}: the history stays history"
    );
    server.assert_clean(user, label).await;
}

fn conflict(result: MemquiraResult<ImportUserDataOutput>, code: &str, label: &str) {
    match result {
        Err(MemquiraError::Conflict(message)) => {
            assert!(message.contains(code), "{label}: {message}")
        }
        other => panic!("{label}: expected {code}, got {other:?}"),
    }
}

enum Backend {
    Mock,
    Sqlite,
    #[cfg(feature = "postgres")]
    Postgres(String),
}

impl Backend {
    async fn server(&self) -> Server {
        match self {
            Backend::Mock => Server::new(
                Arc::new(MockFactStore::new()),
                Arc::new(MockEpisodeStore::new()),
            ),
            Backend::Sqlite => {
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect("sqlite::memory:")
                    .await
                    .unwrap();
                SqliteMigrationRunner::new(pool.clone())
                    .apply_pending_migrations(&sqlite_migrations())
                    .await
                    .unwrap();
                Server::new(
                    Arc::new(SqliteFactStore::new(pool.clone())),
                    Arc::new(SqliteEpisodeStore::new(pool)),
                )
            }
            #[cfg(feature = "postgres")]
            Backend::Postgres(url) => {
                use memquira_storage::{
                    PostgresEpisodeStore, PostgresFactStore, PostgresMigrationRunner,
                    postgres_migrations,
                };
                use sqlx::postgres::PgPool;
                let admin = PgPool::connect(url).await.expect("admin");
                let name = format!("export_deletion_{}", Uuid::new_v4().simple());
                sqlx::query(&format!("CREATE DATABASE {name}"))
                    .execute(&admin)
                    .await
                    .unwrap();
                admin.close().await;
                let db_url = format!("{}/{name}", url.rsplit_once('/').unwrap().0);
                let pool = PgPool::connect(&db_url).await.unwrap();
                PostgresMigrationRunner::new(pool.clone())
                    .apply_pending_migrations(&postgres_migrations())
                    .await
                    .unwrap();
                Server::new(
                    Arc::new(PostgresFactStore::new(pool.clone())),
                    Arc::new(PostgresEpisodeStore::new(pool)),
                )
            }
        }
    }
}

/// Every scenario, each on fresh servers of the backend.
async fn scenarios(backend: &Backend) {
    exports_mark_deletions(backend).await;
    append_keeps_deleted_and_repeats_are_refused(backend).await;
    replace_keeps_deleted_and_repeats_converge(backend).await;
    complete_export_round_trip(backend).await;
    export_without_deleted_memories_does_not_restore_their_messages(backend).await;
    older_export_with_delete_events_keeps_deletion(backend).await;
    older_snapshot_does_not_resurrect_on_the_same_server(backend).await;
    pre_deletion_snapshot_on_a_server_without_the_deletion_restores_it(backend).await;
}

async fn exports_mark_deletions(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    source.delete(ALICE, secret).await;
    source.assert_clean(ALICE, "source after delete").await;

    let full = source.export(ALICE, true).await;
    assert_eq!(full.facts.len(), 3);
    assert_eq!(
        full.deleted_facts.keys().copied().collect::<Vec<_>>(),
        vec![secret],
        "only the deleted fact is marked"
    );
    assert_eq!(full.withheld_message_ids, vec!["m1".to_string()]);

    let live = source.export(ALICE, false).await;
    assert_eq!(
        live.facts.len(),
        2,
        "no deleted fact without include_deleted"
    );
    assert!(live.deleted_facts.is_empty());
    assert_eq!(
        live.withheld_message_ids,
        vec!["m1".to_string()],
        "the message is exported, and listed as withheld"
    );
    assert_eq!(live.episodes.as_ref().map(Vec::len), Some(2));

    let complete = source.export_complete(ALICE).await;
    assert_eq!(
        complete.deleted_facts.keys().copied().collect::<Vec<_>>(),
        vec![secret]
    );
    assert_eq!(complete.withheld_message_ids, vec!["m1".to_string()]);
}

async fn append_keeps_deleted_and_repeats_are_refused(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    source.delete(ALICE, secret).await;
    let export = source.export(ALICE, true).await;

    let target = backend.server().await;
    let done = target
        .import_checked(ALICE, &export, ImportMode::Append)
        .await;
    assert_eq!(done.imported_facts, 3);
    assert_eq!(done.imported_deleted_facts, 1);
    assert_eq!(
        (done.imported_episodes, done.skipped_episodes),
        (Some(2), Some(0)),
        "the deleted memory's message is restored: its deleted fact withholds it"
    );
    assert_restored(&target, ALICE, "append").await;

    // A repeated append is refused before any write and changes nothing.
    conflict(
        target
            .import(ALICE, &export, ImportMode::Append, false)
            .await,
        memquira_common::error::IMPORT_ALREADY_APPLIED_PREFIX,
        "repeated append",
    );
    assert_restored(&target, ALICE, "after the refused repeat").await;

    // An export of only the deleted fact, applied twice: the repeat is also a re-application.
    let only_deleted = UserMemoryExport {
        facts: export
            .facts
            .iter()
            .filter(|f| f.id == secret)
            .cloned()
            .collect(),
        episodes: None,
        memory_events: Vec::new(),
        ..export.clone()
    };
    let other = backend.server().await;
    let done = other
        .import_checked(ALICE, &only_deleted, ImportMode::Append)
        .await;
    assert_eq!((done.imported_facts, done.imported_deleted_facts), (1, 1));
    conflict(
        other
            .import(ALICE, &only_deleted, ImportMode::Append, false)
            .await,
        memquira_common::error::IMPORT_ALREADY_APPLIED_PREFIX,
        "repeated append of a deleted fact",
    );
}

async fn replace_keeps_deleted_and_repeats_converge(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    source.delete(ALICE, secret).await;
    let export = source.export(ALICE, true).await;

    // Onto another server, twice: the same state each time.
    let target = backend.server().await;
    for round in 1..=2 {
        let done = target
            .import_checked(ALICE, &export, ImportMode::Replace)
            .await;
        assert!(done.replaced_existing);
        assert_eq!((done.imported_facts, done.imported_deleted_facts), (3, 1));
        assert_restored(&target, ALICE, &format!("replace round {round}")).await;
    }

    // Onto the source itself.
    source
        .import_checked(ALICE, &export, ImportMode::Replace)
        .await;
    assert_restored(&source, ALICE, "replace onto the source").await;
}

async fn complete_export_round_trip(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    source.delete(ALICE, secret).await;
    let export = source.export_complete(ALICE).await;

    let target = backend.server().await;
    let done = target
        .import_checked(ALICE, &export, ImportMode::Append)
        .await;
    assert!(done.manifest_consistent);
    assert_eq!(done.imported_deleted_facts, 1);
    assert_restored(&target, ALICE, "complete export").await;
}

async fn export_without_deleted_memories_does_not_restore_their_messages(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    source.delete(ALICE, secret).await;
    let export = source.export(ALICE, false).await;

    for mode in [ImportMode::Append, ImportMode::Replace] {
        let target = backend.server().await;
        let done = target.import_checked(ALICE, &export, mode).await;
        assert_eq!((done.imported_facts, done.imported_deleted_facts), (2, 0));
        assert_eq!(
            (done.imported_episodes, done.skipped_episodes),
            (Some(1), Some(1)),
            "{mode:?}: the withheld message is not restored"
        );
        let (live, deleted) = target.rows(ALICE).await;
        assert_eq!(live, vec![HISTORY.to_string(), KEPT_FACT.to_string()]);
        assert!(deleted.is_empty());
        target.assert_clean(ALICE, &format!("{mode:?}")).await;
    }
}

async fn older_export_with_delete_events_keeps_deletion(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    source.delete(ALICE, secret).await;
    // As written before the deletion markers: deleted facts unmarked, no withheld list, but the
    // delete event is in the export.
    let old = UserMemoryExport {
        deleted_facts: Default::default(),
        withheld_message_ids: Vec::new(),
        ..source.export(ALICE, true).await
    };
    assert!(old.memory_events.iter().any(|e| e.operation
        == memquira_core::MemoryEventOperation::Delete
        && e.fact_id == Some(secret)));

    let target = backend.server().await;
    let done = target.import_checked(ALICE, &old, ImportMode::Append).await;
    assert_eq!(done.imported_deleted_facts, 1);
    assert_restored(&target, ALICE, "older export with events").await;
}

async fn older_snapshot_does_not_resurrect_on_the_same_server(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    // A backup taken before the deletion: the memory is live in it.
    let before = source.export(ALICE, false).await;
    assert!(before.deleted_facts.is_empty() && before.withheld_message_ids.is_empty());
    source.delete(ALICE, secret).await;

    // Append is refused (the deleted id), before any write.
    let result = source
        .import(ALICE, &before, ImportMode::Append, false)
        .await;
    assert!(
        matches!(result, Err(MemquiraError::Conflict(_))),
        "append of an older snapshot: {result:?}"
    );
    source.assert_clean(ALICE, "refused append").await;

    // Replace keeps the deletion this server holds.
    let done = source
        .import_checked(ALICE, &before, ImportMode::Replace)
        .await;
    assert_eq!(done.imported_deleted_facts, 1);
    assert_eq!(
        (done.imported_episodes, done.skipped_episodes),
        (Some(2), Some(0))
    );
    assert_restored(&source, ALICE, "replace with an older snapshot").await;
}

/// Known limitation, pinned: a backup taken BEFORE the deletion, restored onto a server that
/// never saw the deletion (a new server, or after the user's data was erased), restores the
/// memory. Nothing in the backup or on that server records that it was deleted later.
async fn pre_deletion_snapshot_on_a_server_without_the_deletion_restores_it(backend: &Backend) {
    let source = backend.server().await;
    let secret = seed(&source).await;
    let before = source.export(ALICE, false).await;
    source.delete(ALICE, secret).await;

    let elsewhere = backend.server().await;
    let done = elsewhere
        .import_checked(ALICE, &before, ImportMode::Append)
        .await;
    assert_eq!(done.imported_deleted_facts, 0);
    assert!(elsewhere.retrieved(ALICE).await.contains(SECRET));
}

#[tokio::test]
async fn deletion_state_survives_export_and_import_on_the_mock_store() {
    scenarios(&Backend::Mock).await;
}

#[tokio::test]
async fn deletion_state_survives_export_and_import_on_sqlite() {
    scenarios(&Backend::Sqlite).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn deletion_state_survives_export_and_import_on_postgres() {
    let url = match memquira_common::env::var("MEMQUIRA_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("EXPORT_DELETION_POSTGRES_SKIPPED: MEMQUIRA_TEST_POSTGRES_URL not set");
            return;
        }
    };
    eprintln!("EXPORT_DELETION_POSTGRES_RAN");
    scenarios(&Backend::Postgres(url)).await;
}
