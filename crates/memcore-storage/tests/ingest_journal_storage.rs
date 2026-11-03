//! The ingest journal on real storage (migration 0016) and the engine's recovery
//! against real SQLite / PostgreSQL fact stores and a real Qdrant. Invented data; no model calls.
//!
//! - The journal contract runs on SQLite always and on PostgreSQL with `MEMCORE_TEST_POSTGRES_URL`
//!   (a database of its own per test).
//! - The engine tests need `MEMCORE_TEST_QDRANT_URL` (a collection of their own, removed after) and
//!   run on SQLite + Qdrant, and on PostgreSQL + Qdrant when both are set; they skip otherwise.
//! - Restart: a second engine is built on fresh connections to the same database and collection;
//!   the first one's runtime is shut down with the request in flight (no rollback can run).

#![cfg(feature = "sqlite")]

#[cfg(feature = "qdrant")]
#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

#[cfg(feature = "qdrant")]
use std::sync::Arc;
#[cfg(feature = "qdrant")]
use std::time::Duration;

use chrono::Utc;
use memcore_common::MemcoreError;
use memcore_core::ports::{IngestJournalStep, IngestJournalStore, IngestOperationStatus};
use memcore_core::{Fact, FactStore, MemorySource, MemoryType, TenantContext};
use memcore_storage::{SqliteFactStore, SqliteIngestJournalStore};
use serde_json::json;
use uuid::Uuid;

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_v21_storage", user).unwrap()
}

fn sample_fact(t: &TenantContext, text: &str) -> Fact {
    let now = Utc::now();
    Fact::new(
        Uuid::new_v4(),
        t.org_id.clone(),
        t.user_id.clone(),
        MemoryType::Profile,
        text,
        None,
        MemorySource::UserMessage,
        0.9,
        0.7,
        None,
        None,
        now,
        now,
        json!({"invented": true}),
    )
    .unwrap()
}

/// The store contract shared by both SQL journals. `fact_store` provides a before-image read back
/// from the same database, so the JSON round trip is checked against a real row.
async fn journal_contract(journal: &dyn IngestJournalStore, fact_store: &dyn FactStore) {
    let me = tenant("user_a");
    let other = tenant("user_b");
    let stored = fact_store
        .insert_fact(&me, sample_fact(&me, "Asha keeps bees on the roof."))
        .await
        .unwrap();
    let before = fact_store.get_fact(&me, stored.id).await.unwrap().unwrap();

    // begin + append; state and in-flight lookups.
    let op = Uuid::new_v4();
    let added = Uuid::new_v4();
    journal
        .begin(&me, op, &IngestJournalStep::Add { fact_id: added })
        .await
        .unwrap();
    let replacement = Uuid::new_v4();
    journal
        .append(
            &me,
            op,
            &IngestJournalStep::Supersede {
                before: before.clone(),
                replacement_fact_id: Some(replacement),
            },
        )
        .await
        .unwrap();
    let state = journal.operation_state(op).await.unwrap().unwrap();
    assert_eq!(state.status, IngestOperationStatus::InProgress);
    for id in [added, before.id, replacement] {
        assert_eq!(
            journal
                .find_in_flight(&me, id, Uuid::new_v4())
                .await
                .unwrap(),
            Some(op)
        );
        assert_eq!(
            journal.find_in_flight(&me, id, op).await.unwrap(),
            None,
            "own op excluded"
        );
        assert_eq!(
            journal
                .find_in_flight(&other, id, Uuid::new_v4())
                .await
                .unwrap(),
            None,
            "tenant-scoped"
        );
    }
    assert_eq!(journal.count_operations().await.unwrap(), 1);

    // Not stale yet; a sweep cannot claim it.
    let past = Utc::now() - chrono::Duration::minutes(5);
    assert!(journal.list_stale(past, 10).await.unwrap().is_empty());
    assert!(journal.claim(op, Some(past)).await.unwrap().is_none());

    // Own claim: steps come back in order, before-image intact.
    let claimed = journal.claim(op, None).await.unwrap().expect("claimed");
    assert_eq!(claimed.status, IngestOperationStatus::RollingBack);
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.tenant().unwrap(), me);
    assert_eq!(
        claimed.steps,
        vec![
            IngestJournalStep::Add { fact_id: added },
            IngestJournalStep::Supersede {
                before: before.clone(),
                replacement_fact_id: Some(replacement)
            }
        ]
    );
    assert!(
        journal.claim(op, None).await.unwrap().is_none(),
        "claimed once"
    );
    // A claimed operation refuses the request's append and commit.
    assert!(matches!(
        journal
            .append(&me, op, &IngestJournalStep::Delete { fact_id: added })
            .await,
        Err(MemcoreError::Conflict(_))
    ));
    assert!(matches!(
        journal.commit(&me, op, None).await,
        Err(MemcoreError::Conflict(_))
    ));
    // A sweep reclaims a stale rollback; release refreshes it.
    let future = Utc::now() + chrono::Duration::minutes(5);
    assert_eq!(journal.list_stale(future, 10).await.unwrap(), vec![op]);
    let again = journal
        .claim(op, Some(future))
        .await
        .unwrap()
        .expect("stale reclaim");
    assert_eq!(again.attempts, 2);
    journal.release(op).await.unwrap();
    assert!(
        journal
            .claim(op, Some(Utc::now() - chrono::Duration::seconds(30)))
            .await
            .unwrap()
            .is_none()
    );
    journal.finish(op).await.unwrap();
    assert!(journal.operation_state(op).await.unwrap().is_none());
    assert_eq!(
        journal
            .find_in_flight(&me, added, Uuid::new_v4())
            .await
            .unwrap(),
        None
    );

    // Commit of an in-progress operation deletes it; a commit by another tenant is refused.
    let op2 = Uuid::new_v4();
    journal
        .begin(
            &me,
            op2,
            &IngestJournalStep::Add {
                fact_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        journal.commit(&other, op2, None).await,
        Err(MemcoreError::Conflict(_))
    ));
    assert!(matches!(
        journal
            .append(
                &other,
                op2,
                &IngestJournalStep::Add {
                    fact_id: Uuid::new_v4()
                }
            )
            .await,
        Err(MemcoreError::Conflict(_))
    ));
    journal.commit(&me, op2, None).await.unwrap();
    assert_eq!(journal.count_operations().await.unwrap(), 0);

    // Forget-user deletes only that user's operations.
    let (mine, theirs) = (Uuid::new_v4(), Uuid::new_v4());
    journal
        .begin(
            &me,
            mine,
            &IngestJournalStep::Update {
                before: before.clone(),
            },
        )
        .await
        .unwrap();
    journal
        .begin(
            &other,
            theirs,
            &IngestJournalStep::Add {
                fact_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    assert_eq!(journal.delete_user_operations(&me).await.unwrap(), 1);
    assert!(journal.operation_state(mine).await.unwrap().is_none());
    assert!(journal.operation_state(theirs).await.unwrap().is_some());
    journal.finish(theirs).await.unwrap();
}

async fn sqlite_url(dir: &tempfile::TempDir) -> String {
    format!("sqlite://{}?mode=rwc", dir.path().join("v21.db").display()).replace('\\', "/")
}

#[tokio::test]
async fn sqlite_ingest_journal_contract() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir).await;
    let facts = SqliteFactStore::connect(&url).await.unwrap();
    let journal = SqliteIngestJournalStore::connect(&url).await.unwrap();
    journal_contract(&journal, &facts).await;
}

#[tokio::test]
async fn sqlite_migration_0016_applies_on_a_version_15_database_with_data() {
    use memcore_storage::migrations::{MigrationRunner, SqliteMigrationRunner, sqlite_migrations};
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir).await;
    let pool = memcore_storage::connect_sqlite_pool(&url).await.unwrap();
    let runner = SqliteMigrationRunner::new(pool.clone());
    let upto15: Vec<_> = sqlite_migrations()
        .into_iter()
        .filter(|m| m.version <= 15)
        .collect();
    assert!(
        runner
            .apply_pending_migrations(&upto15)
            .await
            .unwrap()
            .clean
    );
    let facts = SqliteFactStore::new(pool.clone());
    let me = tenant("user_m");
    let fact = facts
        .insert_fact(&me, sample_fact(&me, "Asha owns a red kayak."))
        .await
        .unwrap();
    let report = runner
        .apply_pending_migrations(&sqlite_migrations())
        .await
        .unwrap();
    assert!(report.clean, "{report:?}");
    assert_eq!(
        facts.get_fact(&me, fact.id).await.unwrap().unwrap(),
        fact,
        "existing rows unchanged"
    );
    let journal = SqliteIngestJournalStore::new(pool);
    assert_eq!(journal.count_operations().await.unwrap(), 0);
}

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use memcore_storage::migrations::{
        MigrationRunner, PostgresMigrationRunner, postgres_migrations,
    };
    use memcore_storage::{PostgresFactStore, PostgresIngestJournalStore};
    use sqlx::postgres::PgPool;

    pub(super) async fn fresh_db(test: &str) -> Option<(PgPool, PgPool, String, String)> {
        let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ => {
                eprintln!("skipping postgres test `{test}`: MEMCORE_TEST_POSTGRES_URL not set");
                return None;
            }
        };
        let admin = PgPool::connect(&url).await.expect("admin connection");
        let name = format!("memcore_v21_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .unwrap();
        let base = url.rsplit_once('/').map(|(b, _)| b).unwrap();
        let db_url = format!("{base}/{name}");
        let pool = PgPool::connect(&db_url).await.expect("test database");
        Some((admin, pool, name, db_url))
    }

    pub(super) async fn drop_db(admin: PgPool, pool: PgPool, name: String) {
        pool.close().await;
        let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .execute(&admin)
            .await;
    }

    #[tokio::test]
    async fn postgres_ingest_journal_contract() {
        let Some((admin, pool, name, _)) = fresh_db("postgres_ingest_journal_contract").await
        else {
            return;
        };
        let runner = PostgresMigrationRunner::new(pool.clone());
        assert!(
            runner
                .apply_pending_migrations(&postgres_migrations())
                .await
                .unwrap()
                .clean
        );
        let facts = PostgresFactStore::new(pool.clone());
        let journal = PostgresIngestJournalStore::new(pool.clone());
        journal_contract(&journal, &facts).await;
        drop_db(admin, pool, name).await;
    }

    #[tokio::test]
    async fn postgres_migration_0016_applies_on_a_version_15_database_with_data() {
        let Some((admin, pool, name, _)) = fresh_db("postgres_migration_0016").await else {
            return;
        };
        let runner = PostgresMigrationRunner::new(pool.clone());
        let upto15: Vec<_> = postgres_migrations()
            .into_iter()
            .filter(|m| m.version <= 15)
            .collect();
        assert!(
            runner
                .apply_pending_migrations(&upto15)
                .await
                .unwrap()
                .clean
        );
        let facts = PostgresFactStore::new(pool.clone());
        let me = tenant("user_m");
        let fact = facts
            .insert_fact(&me, sample_fact(&me, "Asha owns a red kayak."))
            .await
            .unwrap();
        let report = runner
            .apply_pending_migrations(&postgres_migrations())
            .await
            .unwrap();
        assert!(report.clean, "{report:?}");
        assert_eq!(
            facts.get_fact(&me, fact.id).await.unwrap().unwrap().content,
            fact.content
        );
        assert_eq!(
            PostgresIngestJournalStore::new(pool.clone())
                .count_operations()
                .await
                .unwrap(),
            0
        );
        drop_db(admin, pool, name).await;
    }
}

// ------------------------------------------------------------------ engine on real backends

#[cfg(feature = "qdrant")]
mod engine {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use memcore_common::MemcoreResult;
    use memcore_core::ports::{
        EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider, PointScope,
        SummarizationInput, VectorPointState, VectorStore,
    };
    use memcore_core::{
        AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, FactExtractionOutput,
        FactOperation, FactOperationDecision, FactSearchQuery, IngestJournalComponents,
        MemoryEngine, MemoryMessage, MessageRole, VectorRecord, VectorSearchQuery,
        VectorSearchResult,
    };
    use memcore_storage::QdrantVectorStore;
    use tokio::sync::Notify;

    const DIM: usize = 16;

    fn vector(text: &str) -> Vec<f32> {
        let mut v = [0.0_f32; DIM];
        for word in text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let h = word
                .bytes()
                .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
            v[(h % DIM as u32) as usize] += 1.0;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter().map(|x| x / n).collect()
    }

    struct Embed;

    #[async_trait]
    impl EmbeddingProvider for Embed {
        async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
            Ok(vector(text))
        }
        async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| vector(t)).collect())
        }
        fn dimensions(&self) -> usize {
            DIM
        }
    }

    #[derive(Default)]
    struct Llm {
        queue: Mutex<Vec<Vec<&'static str>>>,
        hold_on: Mutex<Option<&'static str>>,
        reached: Notify,
        release: Notify,
        delay_ms: std::sync::atomic::AtomicU64,
    }

    #[async_trait]
    impl LlmProvider for Llm {
        async fn extract_facts(
            &self,
            _: FactExtractionInput,
        ) -> MemcoreResult<FactExtractionOutput> {
            let next = self.queue.lock().unwrap().remove(0);
            Ok(FactExtractionOutput::from_candidates(
                next.into_iter()
                    .map(|t| {
                        CandidateFact::new(t, MemoryType::Profile, 0.9, 0.9, None, json!({}))
                            .unwrap()
                    })
                    .collect(),
            ))
        }
        async fn classify_fact_operation(
            &self,
            input: FactClassificationInput,
        ) -> MemcoreResult<FactOperationDecision> {
            let held = self
                .hold_on
                .lock()
                .unwrap()
                .is_some_and(|h| h == input.candidate_fact.content);
            if held {
                self.reached.notify_one();
                self.release.notified().await;
            }
            let delay = self.delay_ms.load(std::sync::atomic::Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Ok(FactOperationDecision {
                operation: FactOperation::Add,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            })
        }
        async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
            Ok(String::new())
        }
    }

    /// The real Qdrant store, with an upsert failure for one text (a write that never reached it).
    struct FaultyQdrant {
        inner: QdrantVectorStore,
        fail_upsert_text: Mutex<Option<String>>,
    }

    #[async_trait]
    impl VectorStore for FaultyQdrant {
        async fn upsert_vector(
            &self,
            t: &TenantContext,
            record: VectorRecord,
        ) -> MemcoreResult<()> {
            if self
                .fail_upsert_text
                .lock()
                .unwrap()
                .take_if(|f| *f == record.content)
                .is_some()
            {
                return Err(MemcoreError::StorageError(
                    "qdrant unavailable (injected)".to_string(),
                ));
            }
            self.inner.upsert_vector(t, record).await
        }
        async fn write_vector(
            &self,
            t: &TenantContext,
            record: VectorRecord,
            intent: memcore_core::ports::VectorWriteIntent,
        ) -> MemcoreResult<()> {
            if self
                .fail_upsert_text
                .lock()
                .unwrap()
                .take_if(|f| *f == record.content)
                .is_some()
            {
                return Err(MemcoreError::StorageError(
                    "qdrant unavailable (injected)".to_string(),
                ));
            }
            self.inner.write_vector(t, record, intent).await
        }
        async fn search_vectors(
            &self,
            q: VectorSearchQuery,
        ) -> MemcoreResult<Vec<VectorSearchResult>> {
            self.inner.search_vectors(q).await
        }
        async fn delete_by_fact_id(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
            self.inner.delete_by_fact_id(t, id).await
        }
        async fn delete_by_user(&self, t: &TenantContext) -> MemcoreResult<()> {
            self.inner.delete_by_user(t).await
        }
        async fn fact_vector_points(
            &self,
            t: &TenantContext,
            id: Uuid,
        ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
            self.inner.fact_vector_points(t, id).await
        }
        async fn count_points(&self, scope: &PointScope) -> MemcoreResult<u64> {
            self.inner.count_points(scope).await
        }
    }

    /// One backend combination: the SQL side opened fresh on every `open` (a restart).
    #[derive(Clone)]
    enum Sql {
        Sqlite(String),
        #[cfg(feature = "postgres")]
        Postgres(String),
    }

    struct Stores {
        facts: Arc<dyn FactStore>,
        journal: Arc<dyn IngestJournalStore>,
        vectors: Arc<FaultyQdrant>,
    }

    async fn open(sql: &Sql, qdrant_url: &str, collection: &str) -> Stores {
        let (facts, journal): (Arc<dyn FactStore>, Arc<dyn IngestJournalStore>) = match sql {
            Sql::Sqlite(url) => (
                Arc::new(SqliteFactStore::connect(url).await.unwrap()),
                Arc::new(SqliteIngestJournalStore::connect(url).await.unwrap()),
            ),
            #[cfg(feature = "postgres")]
            Sql::Postgres(url) => {
                let facts = memcore_storage::PostgresFactStore::connect(url)
                    .await
                    .unwrap();
                let pool = memcore_storage::connect_postgres_pool(url).await.unwrap();
                (
                    Arc::new(facts),
                    Arc::new(memcore_storage::PostgresIngestJournalStore::new(pool)),
                )
            }
        };
        let inner = QdrantVectorStore::connect(qdrant_url, collection, DIM)
            .await
            .expect("qdrant");
        inner
            .ensure_payload_indexes(memcore_storage::FACT_PAYLOAD_INDEXES)
            .await
            .unwrap();
        Stores {
            facts,
            journal,
            vectors: Arc::new(FaultyQdrant {
                inner,
                fail_upsert_text: Mutex::new(None),
            }),
        }
    }

    fn engine(stores: &Stores, llm: Arc<Llm>, stale_after: Duration) -> Arc<MemoryEngine> {
        Arc::new(
            MemoryEngine::new(
                stores.facts.clone(),
                stores.vectors.clone(),
                llm,
                Arc::new(Embed),
            )
            .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
                enabled: false,
                ..Default::default()
            })
            .with_ingest_journal(Some(IngestJournalComponents {
                store: stores.journal.clone(),
                stale_after,
                inflight_wait: Duration::from_secs(10),
                receipt_ttl: memcore_core::DEFAULT_INGEST_RECEIPT_TTL,
            })),
        )
    }

    async fn add(
        engine: Arc<MemoryEngine>,
        t: TenantContext,
    ) -> MemcoreResult<memcore_core::AddMemoryOutput> {
        engine
            .add_memory(AddMemoryInput {
                tenant: t,
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "Asha's week.".to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
    }

    /// Live SQL rows of the tenant, and every row's point count in Qdrant.
    async fn state(stores: &Stores, t: &TenantContext) -> (Vec<String>, usize, usize) {
        let all = stores
            .facts
            .search_facts(FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 1000,
                cursor: None,
                include_deleted: true,
            })
            .await
            .unwrap();
        let live: Vec<Fact> = stores
            .facts
            .search_facts(FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 1000,
                cursor: None,
                include_deleted: false,
            })
            .await
            .unwrap();
        let mut missing = 0;
        for fact in &live {
            let points = stores
                .vectors
                .fact_vector_points(t, fact.id)
                .await
                .unwrap()
                .unwrap_or_default();
            if points.len() != 1 || points[0].content != fact.content {
                missing += 1;
            }
        }
        let live_ids: std::collections::HashSet<Uuid> = live.iter().map(|f| f.id).collect();
        let mut orphans = 0;
        for fact in all.iter().filter(|f| !live_ids.contains(&f.id)) {
            orphans += stores
                .vectors
                .fact_vector_points(t, fact.id)
                .await
                .unwrap()
                .unwrap_or_default()
                .len();
        }
        let mut texts: Vec<String> = live.into_iter().map(|f| f.content).collect();
        texts.sort();
        (texts, missing, orphans)
    }

    async fn age_journal(sql: &Sql) {
        match sql {
            Sql::Sqlite(url) => {
                let pool = memcore_storage::connect_sqlite_pool(url).await.unwrap();
                sqlx::query("UPDATE ingest_operations SET updated_at_ms = 0")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            #[cfg(feature = "postgres")]
            Sql::Postgres(url) => {
                let pool = memcore_storage::connect_postgres_pool(url).await.unwrap();
                sqlx::query("UPDATE ingest_operations SET updated_at_ms = 0")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
    }

    const A: &str = "Asha keeps bees on the roof.";
    const B: &str = "Asha repairs bicycles on Sundays.";
    const C: &str = "Asha sings in a harbour choir.";

    /// The full recovery scenario set on one backend combination.
    fn scenarios(sql: Sql, label: &'static str) {
        let Ok(qdrant_url) = std::env::var("MEMCORE_TEST_QDRANT_URL") else {
            eprintln!("skipping `{label}`: MEMCORE_TEST_QDRANT_URL not set");
            return;
        };
        let collection = format!("memcore_test_v21_{}", Uuid::new_v4().simple());
        let _guard = qdrant_cleanup::TestCollections::owning(&collection);
        let rt = || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
        };

        // 1. Failure after the SQL row, before its Qdrant upsert: every add rolled back.
        let main = rt();
        main.block_on(async {
            let t = tenant("user_fail");
            let s = open(&sql, &qdrant_url, &collection).await;
            let llm = Arc::new(Llm::default());
            llm.queue.lock().unwrap().push(vec![A, B, C]);
            *s.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
            assert!(
                add(engine(&s, llm, Duration::from_secs(300)), t.clone())
                    .await
                    .is_err()
            );
            assert_eq!(
                state(&s, &t).await,
                (vec![], 0, 0),
                "{label}: nothing live, no orphan point"
            );
            assert_eq!(s.journal.count_operations().await.unwrap(), 0);

            // 2. Concurrent identical requests: one fact, one point, all succeed.
            let t = tenant("user_conc");
            let llm = Arc::new(Llm::default());
            llm.delay_ms.store(30, std::sync::atomic::Ordering::SeqCst);
            for _ in 0..6 {
                llm.queue.lock().unwrap().push(vec![A]);
            }
            let e = engine(&s, llm, Duration::from_secs(300));
            let tasks: Vec<_> = (0..6)
                .map(|_| tokio::spawn(add(e.clone(), t.clone())))
                .collect();
            for task in tasks {
                task.await
                    .unwrap()
                    .expect("each identical request succeeds");
            }
            assert_eq!(state(&s, &t).await, (vec![A.to_string()], 0, 0), "{label}");

            // 3. Cancellation: rolled back by the request's own task.
            let t = tenant("user_cancel");
            let llm = Arc::new(Llm::default());
            llm.queue.lock().unwrap().push(vec![A, B]);
            *llm.hold_on.lock().unwrap() = Some(B);
            let request = tokio::spawn(add(
                engine(&s, llm.clone(), Duration::from_secs(300)),
                t.clone(),
            ));
            llm.reached.notified().await;
            request.abort();
            let _ = request.await;
            for _ in 0..200 {
                if s.journal.count_operations().await.unwrap() == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                state(&s, &t).await,
                (vec![], 0, 0),
                "{label}: cancelled write undone"
            );
        });
        drop(main);

        // 4. Process stop mid-request, then a restart on fresh connections.
        let t = tenant("user_restart");
        let first = rt();
        let llm = Arc::new(Llm::default());
        first.block_on(async {
            let s = open(&sql, &qdrant_url, &collection).await;
            llm.queue.lock().unwrap().push(vec![A, B]);
            *llm.hold_on.lock().unwrap() = Some(B);
            tokio::spawn(add(
                engine(&s, llm.clone(), Duration::from_secs(300)),
                t.clone(),
            ));
            llm.reached.notified().await;
        });
        first.shutdown_background();
        std::thread::sleep(Duration::from_millis(200));
        let second = rt();
        second.block_on(async {
            let s = open(&sql, &qdrant_url, &collection).await;
            assert_eq!(
                state(&s, &t).await.0,
                vec![A.to_string()],
                "{label}: the partial write survived the stop"
            );
            assert_eq!(s.journal.count_operations().await.unwrap(), 1);
            let e = engine(&s, Arc::new(Llm::default()), Duration::from_secs(300));
            assert_eq!(
                e.recover_stale_ingest_operations(100)
                    .await
                    .unwrap()
                    .rolled_back,
                0,
                "not stale yet"
            );
            age_journal(&sql).await;
            let report = e.recover_stale_ingest_operations(100).await.unwrap();
            assert_eq!(
                (report.rolled_back, report.pending_after),
                (1, 0),
                "{label}: {report:?}"
            );
            assert_eq!(
                state(&s, &t).await,
                (vec![], 0, 0),
                "{label}: restart recovery converged"
            );
            let again = e.recover_stale_ingest_operations(100).await.unwrap();
            assert_eq!(again.examined, 0, "repeated recovery does nothing");
        });
    }

    #[test]
    fn sqlite_and_qdrant_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("engine.db").display()
        )
        .replace('\\', "/");
        scenarios(Sql::Sqlite(url), "sqlite+qdrant");
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_and_qdrant_recovery() {
        let setup = tokio::runtime::Runtime::new().unwrap();
        let Some((admin, pool, name, db_url)) =
            setup.block_on(super::postgres::fresh_db("postgres_and_qdrant_recovery"))
        else {
            return;
        };
        scenarios(Sql::Postgres(db_url), "postgres+qdrant");
        setup.block_on(super::postgres::drop_db(admin, pool, name));
    }
}
