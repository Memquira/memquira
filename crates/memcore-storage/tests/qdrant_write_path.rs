//! Qdrant write intents, write modes and retry safety (skipped without
//! MEMCORE_TEST_QDRANT_URL, which must be the gRPC URL of a disposable Qdrant).
//!
//! The ambiguous-acknowledgment tests put a TCP proxy between the real `qdrant-client` and Qdrant.
//! When armed, the proxy forwards the next request to Qdrant, swallows everything Qdrant sends back
//! (the acknowledgment included) and then drops the connection, so the client sees a transport
//! failure for a write Qdrant has already applied. The client's own channel replay then reconnects
//! through the (disarmed) proxy. Before forwarding the replay, the proxy records whether the point
//! already exists, which proves the first attempt was applied and only its acknowledgment was lost.

#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use memcore_common::MemcoreError;
use memcore_core::ports::VectorWriteIntent;
use memcore_core::{MemoryType, TenantContext};
use memcore_storage::{
    QdrantVectorStore, QdrantWriteMode, VectorRecord, VectorSearchQuery, VectorStore,
};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{Condition, CountPointsBuilder, Filter, ScrollPointsBuilder};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use uuid::Uuid;

const DIMENSIONS: usize = 4;

fn qdrant_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn raw(url: &str) -> Qdrant {
    Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .expect("raw client")
}

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).expect("tenant")
}

fn record(org: &str, user: &str, fact_id: Uuid, values: [f32; 4]) -> VectorRecord {
    VectorRecord {
        id: Uuid::new_v4(),
        fact_id,
        org_id: org.to_string(),
        user_id: user.to_string(),
        embedding: values.to_vec(),
        content: format!("{org}/{user}/{fact_id}"),
        memory_type: MemoryType::Profile,
        metadata: json!({"invented": true}),
    }
}

fn collection_name(label: &str) -> String {
    format!("memcore_t3c_{label}_{}", Uuid::new_v4().simple())
}

/// Points of one fact in one tenant: (point id, org_id, user_id).
async fn points_of(
    client: &Qdrant,
    collection: &str,
    tenant: &TenantContext,
    fact_id: Uuid,
) -> Vec<(String, String, String)> {
    let filter = Filter::must([
        Condition::matches("org_id", tenant.org_id.clone()),
        Condition::matches("user_id", tenant.user_id.clone()),
        Condition::matches("fact_id", fact_id.to_string()),
    ]);
    let points = client
        .scroll(
            ScrollPointsBuilder::new(collection)
                .filter(filter)
                .limit(100)
                .with_payload(true),
        )
        .await
        .expect("scroll")
        .result;
    points
        .into_iter()
        .map(|p| {
            let id = match p.id.and_then(|id| id.point_id_options) {
                Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
                other => format!("{other:?}"),
            };
            let get = |k: &str| {
                p.payload
                    .get(k)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default()
            };
            (id, get("org_id"), get("user_id"))
        })
        .collect()
}

async fn fact_point_count(client: &Qdrant, collection: &str, fact_id: Uuid) -> u64 {
    client
        .count(
            CountPointsBuilder::new(collection)
                .filter(Filter::must([Condition::matches(
                    "fact_id",
                    fact_id.to_string(),
                )]))
                .exact(true),
        )
        .await
        .expect("count")
        .result
        .map(|c| c.count)
        .unwrap_or(0)
}

async fn drop_collection(client: &Qdrant, collection: &str) {
    let _ = client.delete_collection(collection).await;
}

async fn store(url: &str, collection: &str, mode: QdrantWriteMode) -> QdrantVectorStore {
    let store = QdrantVectorStore::connect(url, collection, DIMENSIONS)
        .await
        .expect("connect")
        .with_write_mode(mode);
    store
        .ensure_payload_indexes(memcore_storage::FACT_PAYLOAD_INDEXES)
        .await
        .expect("indexes");
    store
}

// ------------------------------------------------------------------------------------------------
// Fault-injecting TCP proxy.

#[derive(Default)]
struct ProxyState {
    /// Arm: the next connection to send client data is "doomed".
    armed: AtomicBool,
    /// Reject: close new connections immediately, before any byte reaches Qdrant.
    reject_new: AtomicBool,
    connections: AtomicUsize,
    rejected: AtomicUsize,
    doomed: AtomicUsize,
    /// Bytes Qdrant sent on the doomed connection that the proxy swallowed (the acknowledgment).
    swallowed: AtomicU64,
    /// Fact whose point the proxy looks up before forwarding a connection opened after a doom.
    watch_fact: Mutex<Option<(String, Uuid)>>,
    /// Point count of `watch_fact` seen when the replay connection arrived (first replay only).
    count_at_replay: Mutex<Option<u64>>,
}

struct Proxy {
    url: String,
    state: Arc<ProxyState>,
}

async fn start_proxy(upstream_url: &str) -> Proxy {
    let upstream = upstream_url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let state = Arc::new(ProxyState::default());
    let upstream_url = upstream_url.to_string();
    let accept_state = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let state = accept_state.clone();
            let upstream = upstream.clone();
            let upstream_url = upstream_url.clone();
            tokio::spawn(async move {
                state.connections.fetch_add(1, Ordering::SeqCst);
                if state.reject_new.load(Ordering::SeqCst) {
                    state.rejected.fetch_add(1, Ordering::SeqCst);
                    drop(client);
                    return;
                }
                if state.doomed.load(Ordering::SeqCst) > 0 {
                    // A connection opened after a doomed one: the client's replay. Record whether
                    // the first attempt was applied before forwarding anything.
                    let watch = state.watch_fact.lock().await.clone();
                    if let Some((collection, fact_id)) = watch {
                        let mut seen = state.count_at_replay.lock().await;
                        if seen.is_none() {
                            *seen = Some(
                                fact_point_count(&raw(&upstream_url), &collection, fact_id).await,
                            );
                        }
                    }
                }
                let Ok(server) = TcpStream::connect(&upstream).await else {
                    return;
                };
                pipe(client, server, state).await;
            });
        }
    });
    Proxy {
        url: format!("http://{addr}"),
        state,
    }
}

async fn pipe(client: TcpStream, server: TcpStream, state: Arc<ProxyState>) {
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let doomed = Arc::new(AtomicBool::new(false));

    let up_doomed = doomed.clone();
    let up_state = state.clone();
    let upstream = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match client_read.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            if up_state.armed.swap(false, Ordering::SeqCst) {
                up_doomed.store(true, Ordering::SeqCst);
                up_state.doomed.fetch_add(1, Ordering::SeqCst);
            }
            if server_write.write_all(&buf[..n]).await.is_err() {
                return;
            }
        }
    });

    let mut buf = vec![0u8; 64 * 1024];
    let mut doomed_since: Option<tokio::time::Instant> = None;
    loop {
        let read =
            tokio::time::timeout(Duration::from_millis(50), server_read.read(&mut buf)).await;
        if doomed.load(Ordering::SeqCst) {
            let since = *doomed_since.get_or_insert_with(tokio::time::Instant::now);
            if let Ok(Ok(n)) = read {
                state.swallowed.fetch_add(n as u64, Ordering::SeqCst);
            }
            // Give Qdrant time to apply the write (wait=true answers after apply), swallowing its
            // acknowledgment, then drop the connection.
            if since.elapsed() >= Duration::from_millis(1500) {
                upstream.abort();
                return;
            }
            continue;
        }
        match read {
            Err(_) => continue,
            Ok(Ok(0)) | Ok(Err(_)) => {
                upstream.abort();
                return;
            }
            Ok(Ok(n)) => {
                if client_write.write_all(&buf[..n]).await.is_err() {
                    upstream.abort();
                    return;
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Ambiguous acknowledgment.

async fn ambiguous_ack_case(mode: QdrantWriteMode, intent: VectorWriteIntent) {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("ack");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    drop(store(&url, &collection, mode).await); // create + index directly

    let proxy = start_proxy(&url).await;
    let through_proxy = QdrantVectorStore::connect_existing(&proxy.url, &collection, DIMENSIONS)
        .await
        .expect("connect through proxy")
        .expect("collection exists")
        .with_write_mode(mode);
    // Warm the channel so the armed request is the write itself.
    let t = tenant("org_ack", "user_ack");
    through_proxy
        .search_vectors(VectorSearchQuery {
            tenant: t.clone(),
            embedding: vec![1.0, 0.0, 0.0, 0.0],
            limit: 1,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("warm-up search");

    let fact_id = Uuid::new_v4();
    let mut rec = record("org_ack", "user_ack", fact_id, [1.0, 0.0, 0.0, 0.0]);
    if intent == VectorWriteIntent::StablePointId {
        rec.id = fact_id;
    }
    let point_id = rec.id;
    *proxy.state.watch_fact.lock().await = Some((collection.clone(), fact_id));
    let ops_before = through_proxy.write_operation_count();
    proxy.state.armed.store(true, Ordering::SeqCst);

    let result = through_proxy.write_vector(&t, rec, intent).await;

    let doomed = proxy.state.doomed.load(Ordering::SeqCst);
    let swallowed = proxy.state.swallowed.load(Ordering::SeqCst);
    let replay_seen = *proxy.state.count_at_replay.lock().await;
    let connections = proxy.state.connections.load(Ordering::SeqCst);
    let points = points_of(&client, &collection, &t, fact_id).await;
    eprintln!(
        "mode={mode:?} intent={intent:?} result={result:?} doomed={doomed} swallowed={swallowed} \
         connections={connections} count_at_replay={replay_seen:?} write_ops={} points={points:?}",
        through_proxy.write_operation_count() - ops_before
    );

    assert_eq!(doomed, 1, "exactly one connection lost its acknowledgment");
    assert!(swallowed > 0, "Qdrant answered on the doomed connection");
    assert!(connections >= 2, "the client reconnected to replay");
    result.expect("the client's replay completes the write");
    match mode {
        // The single upsert was the doomed request: it was applied before the replay arrived.
        QdrantWriteMode::Optimized => assert_eq!(
            replay_seen,
            Some(1),
            "the write was applied before its replay (ambiguous acknowledgment)"
        ),
        // Legacy: the doomed request is the pre-delete (nothing to delete for a fresh fact).
        QdrantWriteMode::Legacy => assert!(replay_seen.is_some()),
    }
    assert_eq!(
        points.len(),
        1,
        "exactly one point for the fact after the replay"
    );
    assert_eq!(
        points[0].0,
        point_id.to_string(),
        "the replay reused the point id"
    );
    assert_eq!(
        (points[0].1.as_str(), points[0].2.as_str()),
        ("org_ack", "user_ack")
    );
    assert_eq!(fact_point_count(&client, &collection, fact_id).await, 1);
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn ambiguous_ack_fresh_fact_optimized_leaves_one_point() {
    ambiguous_ack_case(QdrantWriteMode::Optimized, VectorWriteIntent::FreshFact).await;
}

#[tokio::test]
async fn ambiguous_ack_episode_optimized_leaves_one_point() {
    ambiguous_ack_case(QdrantWriteMode::Optimized, VectorWriteIntent::StablePointId).await;
}

#[tokio::test]
async fn ambiguous_ack_fresh_fact_legacy_leaves_one_point() {
    ambiguous_ack_case(QdrantWriteMode::Legacy, VectorWriteIntent::FreshFact).await;
}

/// A replacement whose first request (the filtered delete) loses its acknowledgment.
#[tokio::test]
async fn ambiguous_ack_on_replace_optimized_leaves_one_point() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("ack_replace");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let direct = store(&url, &collection, QdrantWriteMode::Optimized).await;
    let t = tenant("org_ack", "user_ack");
    let fact_id = Uuid::new_v4();
    let seed = record("org_ack", "user_ack", fact_id, [1.0, 0.0, 0.0, 0.0]);
    let seed_point = seed.id;
    direct
        .write_vector(&t, seed, VectorWriteIntent::FreshFact)
        .await
        .unwrap();

    let proxy = start_proxy(&url).await;
    let through_proxy = QdrantVectorStore::connect_existing(&proxy.url, &collection, DIMENSIONS)
        .await
        .unwrap()
        .unwrap()
        .with_write_mode(QdrantWriteMode::Optimized);
    // Warm up, then arm: in optimized Replace the first request is the filtered delete and the
    // second the upsert. Arm while the delete is in flight is not deterministic, so instead doom
    // the first request (the delete) and check the replayed delete + upsert still give one point.
    through_proxy
        .delete_by_user(&tenant("org_none", "user_none"))
        .await
        .unwrap();
    proxy.state.armed.store(true, Ordering::SeqCst);
    let replacement = record("org_ack", "user_ack", fact_id, [0.0, 1.0, 0.0, 0.0]);
    through_proxy
        .write_vector(&t, replacement, VectorWriteIntent::Replace)
        .await
        .expect("replace completes after the replay");
    let points = points_of(&client, &collection, &t, fact_id).await;
    assert_eq!(proxy.state.doomed.load(Ordering::SeqCst), 1);
    assert_eq!(points.len(), 1);
    // A replacement keeps the fact's existing point id.
    assert_eq!(points[0].0, seed_point.to_string());
    drop_collection(&client, &collection).await;
}

// ------------------------------------------------------------------------------------------------
// Rejection, concurrency, tenants, rollback, operation counts.

#[tokio::test]
async fn rejected_write_writes_nothing_and_a_retry_of_the_same_record_leaves_one_point() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("reject");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    drop(store(&url, &collection, QdrantWriteMode::Optimized).await);
    let proxy = start_proxy(&url).await;
    let through_proxy = QdrantVectorStore::connect_existing(&proxy.url, &collection, DIMENSIONS)
        .await
        .unwrap()
        .unwrap()
        .with_write_mode(QdrantWriteMode::Optimized);
    let t = tenant("org_rej", "user_rej");
    through_proxy
        .delete_by_user(&tenant("org_none", "user_none"))
        .await
        .unwrap();

    // Every new connection is refused and the current one is dropped: the client's replay fails too.
    proxy.state.reject_new.store(true, Ordering::SeqCst);
    proxy.state.armed.store(true, Ordering::SeqCst);
    let fact_id = Uuid::new_v4();
    let rec = record("org_rej", "user_rej", fact_id, [1.0, 0.0, 0.0, 0.0]);
    let first = through_proxy
        .write_vector(&t, rec.clone(), VectorWriteIntent::FreshFact)
        .await;
    // The doomed request may have been applied before the drop; what matters is that the
    // caller sees the failure and that a retry of the SAME record converges to one point.
    assert!(first.is_err(), "the write is reported as failed: {first:?}");
    let after_failure = fact_point_count(&client, &collection, fact_id).await;
    assert!(after_failure <= 1);

    proxy.state.reject_new.store(false, Ordering::SeqCst);
    through_proxy
        .write_vector(&t, rec.clone(), VectorWriteIntent::FreshFact)
        .await
        .expect("retry succeeds");
    let points = points_of(&client, &collection, &t, fact_id).await;
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].0, rec.id.to_string());
    eprintln!(
        "rejected={} after_failure={after_failure} points={points:?}",
        proxy.state.rejected.load(Ordering::SeqCst)
    );
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn concurrent_attempts_of_the_same_record_leave_one_point() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("concurrent");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let s = Arc::new(store(&url, &collection, QdrantWriteMode::Optimized).await);
    let t = tenant("org_c", "user_c");
    let fact_id = Uuid::new_v4();
    let rec = record("org_c", "user_c", fact_id, [1.0, 0.0, 0.0, 0.0]);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let (s, t, rec) = (s.clone(), t.clone(), rec.clone());
        handles.push(tokio::spawn(async move {
            s.write_vector(&t, rec, VectorWriteIntent::FreshFact).await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    assert_eq!(points_of(&client, &collection, &t, fact_id).await.len(), 1);

    drop_collection(&client, &collection).await;
}

/// Concurrent REPLACEMENTS of one fact used to leave up to N
/// points in both modes (filtered delete, then a NEW random point id each). The replacement
/// reuses the fact's existing point and deletes only observed extras, so the store-level race ends
/// with exactly one point.
#[tokio::test]
async fn concurrent_replacements_of_one_fact_characterised() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("concurrent_replace");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    for mode in [QdrantWriteMode::Legacy, QdrantWriteMode::Optimized] {
        let s = Arc::new(store(&url, &collection, mode).await);
        let t = tenant("org_c", "user_c");
        let mut worst = 0;
        for _round in 0..5 {
            let fact_id = Uuid::new_v4();
            s.write_vector(
                &t,
                record("org_c", "user_c", fact_id, [1.0, 0.0, 0.0, 0.0]),
                VectorWriteIntent::FreshFact,
            )
            .await
            .unwrap();
            let mut handles = Vec::new();
            for k in 0..4 {
                let (s, t) = (s.clone(), t.clone());
                handles.push(tokio::spawn(async move {
                    s.write_vector(
                        &t,
                        record("org_c", "user_c", fact_id, [1.0, k as f32, 0.0, 0.0]),
                        VectorWriteIntent::Replace,
                    )
                    .await
                }));
            }
            for handle in handles {
                handle.await.unwrap().unwrap();
            }
            let after = fact_point_count(&client, &collection, fact_id).await;
            assert_eq!(
                after, 1,
                "{mode:?}: concurrent replacements leave exactly one point"
            );
            worst = worst.max(after);
            // Sequential replacements always converge back to one point.
            s.write_vector(
                &t,
                record("org_c", "user_c", fact_id, [0.0, 0.0, 1.0, 0.0]),
                VectorWriteIntent::Replace,
            )
            .await
            .unwrap();
            assert_eq!(fact_point_count(&client, &collection, fact_id).await, 1);
        }
        eprintln!(
            "AR-038 {mode:?}: 4 concurrent replacements of one fact left up to {worst} point(s)"
        );
    }
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn same_fact_id_in_two_tenants_stays_isolated() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("tenants");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    for mode in [QdrantWriteMode::Optimized, QdrantWriteMode::Legacy] {
        let s = store(&url, &collection, mode).await;
        let fact_id = Uuid::new_v4();
        let tenants = [
            tenant("org_a", "user_1"),
            tenant("org_b", "user_1"),
            tenant("org_a", "user_2"),
        ];
        for t in &tenants {
            s.write_vector(
                t,
                record(&t.org_id, &t.user_id, fact_id, [1.0, 0.0, 0.0, 0.0]),
                VectorWriteIntent::FreshFact,
            )
            .await
            .unwrap();
        }
        // Replace in one tenant, delete in another: the third is untouched.
        s.write_vector(
            &tenants[0],
            record("org_a", "user_1", fact_id, [0.0, 1.0, 0.0, 0.0]),
            VectorWriteIntent::Replace,
        )
        .await
        .unwrap();
        s.delete_by_fact_id(&tenants[1], fact_id).await.unwrap();
        assert_eq!(
            points_of(&client, &collection, &tenants[0], fact_id)
                .await
                .len(),
            1
        );
        assert_eq!(
            points_of(&client, &collection, &tenants[1], fact_id)
                .await
                .len(),
            0
        );
        assert_eq!(
            points_of(&client, &collection, &tenants[2], fact_id)
                .await
                .len(),
            1
        );
        // A record for another tenant is refused before any Qdrant call.
        let foreign = record("org_b", "user_1", fact_id, [1.0, 0.0, 0.0, 0.0]);
        for intent in [
            VectorWriteIntent::FreshFact,
            VectorWriteIntent::StablePointId,
            VectorWriteIntent::Replace,
        ] {
            let before = s.write_operation_count();
            let err = s
                .write_vector(&tenants[0], foreign.clone(), intent)
                .await
                .expect_err("foreign record");
            assert!(matches!(err, MemcoreError::Forbidden), "{err:?}");
            assert_eq!(s.write_operation_count(), before);
        }
    }
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn operation_counts_per_mode_and_intent() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("ops");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let mut table = Vec::new();
    for mode in [QdrantWriteMode::Legacy, QdrantWriteMode::Optimized] {
        let s = store(&url, &collection, mode).await;
        let t = tenant("org_ops", "user_ops");
        let fact_id = Uuid::new_v4();
        let ops = |s: &QdrantVectorStore| s.write_operation_count();

        let before = ops(&s);
        s.write_vector(
            &t,
            record("org_ops", "user_ops", fact_id, [1.0, 0.0, 0.0, 0.0]),
            VectorWriteIntent::FreshFact,
        )
        .await
        .unwrap();
        let fresh = ops(&s) - before;

        let before = ops(&s);
        s.write_vector(
            &t,
            record("org_ops", "user_ops", fact_id, [0.0, 1.0, 0.0, 0.0]),
            VectorWriteIntent::Replace,
        )
        .await
        .unwrap();
        let replace = ops(&s) - before;
        assert_eq!(points_of(&client, &collection, &t, fact_id).await.len(), 1);

        let episode_id = Uuid::new_v4();
        let mut episode = record("org_ops", "user_ops", episode_id, [0.0, 0.0, 1.0, 0.0]);
        episode.id = episode_id;
        let before = ops(&s);
        s.write_vector(&t, episode.clone(), VectorWriteIntent::StablePointId)
            .await
            .unwrap();
        let episode_ops = ops(&s) - before;
        // Episode retry (same id): still one point.
        s.write_vector(&t, episode, VectorWriteIntent::StablePointId)
            .await
            .unwrap();
        assert_eq!(
            points_of(&client, &collection, &t, episode_id).await.len(),
            1
        );

        // A stable-id claim whose point id is not the fact id is treated as a replacement.
        let mismatched = record("org_ops", "user_ops", episode_id, [0.0, 0.0, 0.0, 1.0]);
        let mismatched_id = mismatched.id;
        s.write_vector(&t, mismatched, VectorWriteIntent::StablePointId)
            .await
            .unwrap();
        let pts = points_of(&client, &collection, &t, episode_id).await;
        assert_eq!(pts.len(), 1);
        // Optimized: treated as a replacement, which keeps the existing point. Legacy: the
        // earlier filtered pre-delete + upsert of the record's own id.
        let expected = match mode {
            QdrantWriteMode::Optimized => episode_id,
            QdrantWriteMode::Legacy => mismatched_id,
        };
        assert_eq!(pts[0].0, expected.to_string(), "{mode:?}");

        table.push((mode, fresh, replace, episode_ops));
    }
    eprintln!("ops (mode, fresh, replace, episode): {table:?}");
    // A replacement is a point listing + upsert in both modes (+ a delete only for extras).
    assert_eq!(table[0], (QdrantWriteMode::Legacy, 2, 2, 2));
    assert_eq!(table[1], (QdrantWriteMode::Optimized, 1, 2, 1));
    drop_collection(&client, &collection).await;
}

/// Why the fresh claim may only be made once per allocated point id: two FreshFact writes of the
/// same fact with DIFFERENT point ids leave two points in optimized mode (legacy leaves one). The
/// lifecycle allocates the point id exactly once per new fact (`apply_add`), and no retry path
/// allocates another.
#[tokio::test]
async fn fresh_claim_with_a_second_point_id_is_not_self_healing() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("misuse");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    for (mode, expected) in [
        (QdrantWriteMode::Optimized, 2),
        (QdrantWriteMode::Legacy, 1),
    ] {
        let s = store(&url, &collection, mode).await;
        let t = tenant("org_m", "user_m");
        let fact_id = Uuid::new_v4();
        for _ in 0..2 {
            s.write_vector(
                &t,
                record("org_m", "user_m", fact_id, [1.0, 0.0, 0.0, 0.0]),
                VectorWriteIntent::FreshFact,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            points_of(&client, &collection, &t, fact_id).await.len(),
            expected,
            "{mode:?}"
        );
    }
    drop_collection(&client, &collection).await;
}

#[tokio::test]
async fn rollback_to_legacy_on_a_collection_written_by_optimized_mode() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = raw(&url);
    let collection = collection_name("rollback");
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let optimized = store(&url, &collection, QdrantWriteMode::Optimized).await;
    let t = tenant("org_r", "user_r");
    let facts: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
    for (i, fact_id) in facts.iter().enumerate() {
        optimized
            .write_vector(
                &t,
                record("org_r", "user_r", *fact_id, [1.0, i as f32, 0.0, 0.0]),
                VectorWriteIntent::FreshFact,
            )
            .await
            .unwrap();
    }
    // Same collection, legacy store (the rollback): updates, deletes and fresh writes.
    let legacy = QdrantVectorStore::connect_existing(&url, &collection, DIMENSIONS)
        .await
        .unwrap()
        .unwrap()
        .with_write_mode(QdrantWriteMode::Legacy);
    legacy
        .write_vector(
            &t,
            record("org_r", "user_r", facts[0], [0.0, 0.0, 1.0, 0.0]),
            VectorWriteIntent::Replace,
        )
        .await
        .unwrap();
    legacy.delete_by_fact_id(&t, facts[1]).await.unwrap();
    for fact_id in &facts[2..] {
        legacy
            .upsert_vector(
                &t,
                record("org_r", "user_r", *fact_id, [0.5, 0.5, 0.0, 0.0]),
            )
            .await
            .unwrap();
    }
    assert_eq!(points_of(&client, &collection, &t, facts[0]).await.len(), 1);
    assert_eq!(points_of(&client, &collection, &t, facts[1]).await.len(), 0);
    for fact_id in &facts[2..] {
        assert_eq!(points_of(&client, &collection, &t, *fact_id).await.len(), 1);
    }
    drop_collection(&client, &collection).await;
}
