//! Concurrent UPDATEs of one fact must end with exactly one Qdrant point whose
//! content is the fact's final SQL text, in one process and across processes (skipped without
//! MEMCORE_TEST_QDRANT_URL; use a disposable Qdrant).
//!
//! The multi-process tests start this test binary twice more as workers (env MEMCORE_T4_WORKER)
//! that share one SQLite database file (or one Postgres database with MEMCORE_TEST_POSTGRES_URL)
//! and one Qdrant collection. Invented data only.

#[path = "../../memcore-storage/tests/support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use memcore_core::lifecycle::{LifecycleApplyResult, LifecycleContext, apply_fact_operation};
use memcore_core::ports::{VectorStore, VectorWriteIntent};
use memcore_core::{
    CandidateFact, FactOperation, FactOperationDecision, FactStore, MemoryType, TenantContext,
    VectorRecord,
};
use memcore_providers::MockEmbeddingProvider;
use memcore_storage::{
    FACT_PAYLOAD_INDEXES, MockFactStore, QdrantVectorStore, QdrantWriteMode, SqliteFactStore,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

const DIMENSIONS: usize = 8;

fn qdrant_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).unwrap()
}

fn candidate(content: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Preference, 0.9, 0.8, None, json!({})).unwrap()
}

fn decision(operation: FactOperation, target: Option<Uuid>) -> FactOperationDecision {
    FactOperationDecision {
        operation,
        target_fact_id: target,
        reason: Some("scripted".into()),
        confidence: 0.9,
    }
}

fn mode_from(name: &str) -> QdrantWriteMode {
    match name {
        "legacy" => QdrantWriteMode::Legacy,
        _ => QdrantWriteMode::Optimized,
    }
}

async fn qdrant_store(url: &str, collection: &str, mode: QdrantWriteMode) -> QdrantVectorStore {
    let store = QdrantVectorStore::connect(url, collection, DIMENSIONS)
        .await
        .unwrap()
        .with_write_mode(mode);
    store
        .ensure_payload_indexes(FACT_PAYLOAD_INDEXES)
        .await
        .unwrap();
    store
}

/// Raw points of a fact: (point id, payload content).
async fn raw_points(
    url: &str,
    collection: &str,
    t: &TenantContext,
    fact_id: Uuid,
) -> Vec<(String, String)> {
    use qdrant_client::qdrant::{Condition, Filter, ScrollPointsBuilder};
    let client = qdrant_client::Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap();
    client
        .scroll(
            ScrollPointsBuilder::new(collection)
                .filter(Filter::must([
                    Condition::matches("org_id", t.org_id.clone()),
                    Condition::matches("user_id", t.user_id.clone()),
                    Condition::matches("fact_id", fact_id.to_string()),
                ]))
                .limit(100)
                .with_payload(true),
        )
        .await
        .unwrap()
        .result
        .into_iter()
        .map(|p| {
            let content = p
                .payload
                .get("content")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            (format!("{:?}", p.id), content)
        })
        .collect()
}

async fn drop_collection(url: &str, collection: &str) {
    let client = qdrant_client::Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap();
    let _ = client.delete_collection(collection).await;
}

/// For each fact: (point count, point content == SQL content).
async fn verify(
    url: &str,
    collection: &str,
    facts: &dyn FactStore,
    t: &TenantContext,
    ids: &[Uuid],
) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    for id in ids {
        let row = facts.get_fact(t, *id).await.unwrap().expect("fact row");
        let points = raw_points(url, collection, t, *id).await;
        let agrees = points.len() == 1 && points[0].1 == row.content;
        out.push((points.len(), agrees));
    }
    out
}

// ------------------------------------------------------------------------------------------------
// 1. Reproduction (earlier replacement sequence) and the fixed store-level replacement.

#[tokio::test]
async fn concurrent_replacements_reproduced_then_fixed_in_both_modes() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let t = tenant("org_r", "user_r");
    let mut report = Vec::new();
    for mode in [QdrantWriteMode::Legacy, QdrantWriteMode::Optimized] {
        let collection = format!("memcore_t4_race_{}", Uuid::new_v4().simple());
        let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
        let store = Arc::new(qdrant_store(&url, &collection, mode).await);
        let (mut worst_old, mut worst_new) = (0usize, 0usize);
        for _round in 0..6 {
            for old_sequence in [true, false] {
                let fact_id = Uuid::new_v4();
                let seed = VectorRecord {
                    id: Uuid::new_v4(),
                    fact_id,
                    org_id: t.org_id.clone(),
                    user_id: t.user_id.clone(),
                    embedding: vec![1.0; DIMENSIONS],
                    content: "seed".into(),
                    memory_type: MemoryType::Preference,
                    metadata: json!({}),
                };
                store
                    .write_vector(&t, seed.clone(), VectorWriteIntent::FreshFact)
                    .await
                    .unwrap();
                let mut handles = Vec::new();
                for k in 0..4 {
                    let (store, t) = (store.clone(), t.clone());
                    let mut rec = seed.clone();
                    rec.id = Uuid::new_v4();
                    rec.content = format!("writer {k}");
                    handles.push(tokio::spawn(async move {
                        if old_sequence {
                            // The earlier replacement: filtered delete, then a NEW random point id.
                            store.upsert_vector(&t, rec).await
                        } else {
                            store
                                .write_vector(&t, rec, VectorWriteIntent::Replace)
                                .await
                        }
                    }));
                }
                for h in handles {
                    h.await.unwrap().unwrap();
                }
                let n = raw_points(&url, &collection, &t, fact_id).await.len();
                assert!(n >= 1, "{mode:?}: never zero points");
                if old_sequence {
                    worst_old = worst_old.max(n);
                } else {
                    assert_eq!(n, 1, "{mode:?}: fixed replacement leaves exactly one point");
                    worst_new = worst_new.max(n);
                }
            }
        }
        report.push((mode, worst_old, worst_new));
        drop_collection(&url, &collection).await;
    }
    eprintln!("AR-038 (mode, worst points pre-T4 sequence, worst points fixed): {report:?}");
    assert!(
        report.iter().any(|(_, old, _)| *old > 1),
        "the pre-T4 race was reproduced at least once: {report:?}"
    );
}

// ------------------------------------------------------------------------------------------------
// 2. In-process concurrent UPDATEs through the lifecycle (both modes).

async fn concurrent_updates(
    url: &str,
    collection: &str,
    facts: Arc<dyn FactStore>,
    mode: QdrantWriteMode,
    t: &TenantContext,
    ids: &[Uuid],
    writers: usize,
    label: &str,
) -> (usize, usize) {
    let store: Arc<QdrantVectorStore> = Arc::new(
        QdrantVectorStore::connect_existing(url, collection, DIMENSIONS)
            .await
            .unwrap()
            .unwrap()
            .with_write_mode(mode),
    );
    let embedding = Arc::new(MockEmbeddingProvider::new(DIMENSIONS));
    let mut handles = Vec::new();
    for id in ids {
        for w in 0..writers {
            let (facts, store, embedding, t, id) = (
                facts.clone(),
                store.clone(),
                embedding.clone(),
                t.clone(),
                *id,
            );
            let content = format!("{label} writer {w} of fact {id}");
            handles.push(tokio::spawn(async move {
                let ctx = LifecycleContext {
                    fact_store: facts.as_ref(),
                    vector_store: store.as_ref(),
                    embedding_provider: embedding.as_ref(),
                };
                apply_fact_operation(
                    &ctx,
                    &t,
                    &candidate(&content),
                    &decision(FactOperation::Update, Some(id)),
                    &json!({}),
                    None,
                )
                .await
            }));
        }
    }
    let (mut ok, mut failed) = (0, 0);
    for h in handles {
        match h.await.unwrap() {
            Ok(LifecycleApplyResult::Updated { .. }) => ok += 1,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(error) => {
                eprintln!("{label}: update error {error:?}");
                failed += 1;
            }
        }
    }
    (ok, failed)
}

async fn seed(
    url: &str,
    collection: &str,
    facts: &dyn FactStore,
    mode: QdrantWriteMode,
    t: &TenantContext,
    n: usize,
) -> Vec<Uuid> {
    let store = QdrantVectorStore::connect_existing(url, collection, DIMENSIONS)
        .await
        .unwrap()
        .unwrap()
        .with_write_mode(mode);
    let embedding = MockEmbeddingProvider::new(DIMENSIONS);
    let ctx = LifecycleContext {
        fact_store: facts,
        vector_store: &store,
        embedding_provider: &embedding,
    };
    let mut ids = Vec::new();
    for i in 0..n {
        match apply_fact_operation(
            &ctx,
            t,
            &candidate(&format!("seed fact {i}")),
            &decision(FactOperation::Add, None),
            &json!({}),
            None,
        )
        .await
        .unwrap()
        {
            LifecycleApplyResult::Added(f) => ids.push(f.id),
            other => panic!("{other:?}"),
        }
    }
    ids
}

#[tokio::test]
async fn concurrent_updates_in_one_process_converge_in_both_modes() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    for mode in [QdrantWriteMode::Legacy, QdrantWriteMode::Optimized] {
        let collection = format!("memcore_t4_inproc_{}", Uuid::new_v4().simple());
        let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
        drop(qdrant_store(&url, &collection, mode).await);
        let facts: Arc<dyn FactStore> = Arc::new(MockFactStore::new());
        let a = tenant("org_a", "user_a");
        let b = tenant("org_a", "user_b");
        let ids_a = seed(&url, &collection, facts.as_ref(), mode, &a, 4).await;
        let ids_b = seed(&url, &collection, facts.as_ref(), mode, &b, 2).await;

        // 8 concurrent updates of each of user A's facts, while user B updates independently.
        let (ra, rb) = tokio::join!(
            concurrent_updates(&url, &collection, facts.clone(), mode, &a, &ids_a, 8, "a"),
            concurrent_updates(&url, &collection, facts.clone(), mode, &b, &ids_b, 1, "b"),
        );
        eprintln!("{mode:?}: user A ok/failed {ra:?}, user B {rb:?}");
        assert_eq!(rb, (2, 0), "independent user updates are unaffected");
        for (t, ids) in [(&a, &ids_a), (&b, &ids_b)] {
            for (n, agrees) in verify(&url, &collection, facts.as_ref(), t, ids).await {
                assert_eq!(n, 1, "{mode:?}: exactly one point");
                assert!(agrees, "{mode:?}: the point carries the final SQL text");
            }
        }
        drop_collection(&url, &collection).await;
    }
}

#[tokio::test]
async fn a_cancelled_update_never_leaves_zero_points_and_the_next_update_converges() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collection = format!("memcore_t4_cancel_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    drop(qdrant_store(&url, &collection, QdrantWriteMode::Optimized).await);
    let facts: Arc<dyn FactStore> = Arc::new(MockFactStore::new());
    let t = tenant("org_c", "user_c");
    let ids = seed(
        &url,
        &collection,
        facts.as_ref(),
        QdrantWriteMode::Optimized,
        &t,
        1,
    )
    .await;
    let store = QdrantVectorStore::connect_existing(&url, &collection, DIMENSIONS)
        .await
        .unwrap()
        .unwrap()
        .with_write_mode(QdrantWriteMode::Optimized);
    let embedding = MockEmbeddingProvider::new(DIMENSIONS);
    let ctx = LifecycleContext {
        fact_store: facts.as_ref(),
        vector_store: &store,
        embedding_provider: &embedding,
    };
    // Cancel updates at several points of their progress (the future is dropped, as at a deadline).
    for micros in [50u64, 300, 1_000, 3_000, 10_000] {
        let _ = tokio::time::timeout(
            std::time::Duration::from_micros(micros),
            apply_fact_operation(
                &ctx,
                &t,
                &candidate(&format!("cancelled after {micros} us")),
                &decision(FactOperation::Update, Some(ids[0])),
                &json!({}),
                None,
            ),
        )
        .await;
        let n = raw_points(&url, &collection, &t, ids[0]).await.len();
        assert!(
            n >= 1,
            "a cancelled update never leaves the fact without a vector"
        );
    }
    apply_fact_operation(
        &ctx,
        &t,
        &candidate("final text"),
        &decision(FactOperation::Update, Some(ids[0])),
        &json!({}),
        None,
    )
    .await
    .unwrap();
    let state = verify(&url, &collection, facts.as_ref(), &t, &ids).await;
    assert_eq!(state, vec![(1, true)]);
    drop_collection(&url, &collection).await;
}

// ------------------------------------------------------------------------------------------------
// 3. Multiple OS processes sharing the SQL database and the Qdrant collection.

/// Worker entry: does nothing unless started by a multi-process test.
#[tokio::test]
async fn t4_worker_process_entry() {
    let Ok(role) = std::env::var("MEMCORE_T4_WORKER") else {
        return;
    };
    let url = std::env::var("MEMCORE_TEST_QDRANT_URL").unwrap();
    let collection = std::env::var("MEMCORE_T4_COLLECTION").unwrap();
    let database = std::env::var("MEMCORE_T4_DATABASE").unwrap();
    let mode = mode_from(&std::env::var("MEMCORE_T4_MODE").unwrap());
    let ids: Vec<Uuid> = std::env::var("MEMCORE_T4_FACTS")
        .unwrap()
        .split(',')
        .map(|s| Uuid::parse_str(s).unwrap())
        .collect();
    let facts = open_fact_store(&database).await;
    let t = tenant("org_mp", "user_mp");
    let (mut ok, mut failed) = (0, 0);
    for round in 0..3 {
        let (o, f) = concurrent_updates(
            &url,
            &collection,
            facts.clone(),
            mode,
            &t,
            &ids,
            4,
            &format!("process {role} round {round}"),
        )
        .await;
        ok += o;
        failed += f;
    }
    println!("T4_WORKER_RESULT role={role} ok={ok} failed={failed}");
}

async fn open_fact_store(database: &str) -> Arc<dyn FactStore> {
    if database.starts_with("postgres") {
        #[cfg(feature = "postgres")]
        {
            return Arc::new(
                memcore_storage::PostgresFactStore::connect(database)
                    .await
                    .unwrap(),
            );
        }
        #[cfg(not(feature = "postgres"))]
        panic!("postgres feature not enabled");
    }
    Arc::new(SqliteFactStore::connect(database).await.unwrap())
}

async fn multi_process_case(database: String, mode: &str) {
    let url = qdrant_url().unwrap();
    let collection = format!("memcore_t4_multi_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    drop(qdrant_store(&url, &collection, mode_from(mode)).await);
    let facts = open_fact_store(&database).await;
    let t = tenant("org_mp", "user_mp");
    let ids = seed(&url, &collection, facts.as_ref(), mode_from(mode), &t, 3).await;
    let fact_list = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for role in ["p1", "p2"] {
        children.push(
            std::process::Command::new(&exe)
                .args([
                    "t4_worker_process_entry",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("MEMCORE_T4_WORKER", role)
                .env("MEMCORE_T4_COLLECTION", &collection)
                .env("MEMCORE_T4_DATABASE", &database)
                .env("MEMCORE_T4_MODE", mode)
                .env("MEMCORE_T4_FACTS", &fact_list)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut results = BTreeMap::new();
    for (i, child) in children.into_iter().enumerate() {
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.find("T4_WORKER_RESULT").map(|at| &l[at..]))
            .unwrap_or_else(|| {
                panic!(
                    "worker {i} printed no result:
{stdout}
{}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
            .to_string();
        assert!(
            output.status.success(),
            "worker {i} failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        results.insert(i, line);
    }
    eprintln!(
        "{mode} {database_kind}: {results:?}",
        database_kind = if database.starts_with("postgres") {
            "postgres"
        } else {
            "sqlite"
        }
    );
    for (n, agrees) in verify(&url, &collection, facts.as_ref(), &t, &ids).await {
        assert_eq!(
            n, 1,
            "exactly one point after concurrent updates from two processes"
        );
        assert!(agrees, "the point carries the final SQL text");
    }
    drop_collection(&url, &collection).await;
}

#[tokio::test]
async fn concurrent_updates_from_two_processes_sqlite() {
    if qdrant_url().is_none() || std::env::var("MEMCORE_T4_WORKER").is_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.db");
    let database = format!(
        "sqlite://{}?mode=rwc",
        path.to_string_lossy().replace('\\', "/")
    );
    for mode in ["legacy", "optimized"] {
        multi_process_case(database.clone(), mode).await;
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn concurrent_updates_from_two_processes_postgres() {
    if qdrant_url().is_none() || std::env::var("MEMCORE_T4_WORKER").is_ok() {
        return;
    }
    let Ok(database) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    for mode in ["legacy", "optimized"] {
        multi_process_case(database.clone(), mode).await;
    }
}
