//! The Qdrant primitives `verify-vectors` (R5) relies on, on a REAL Qdrant: paged `scroll_points` /
//! exact `count_points` (Q1), the unattributed-point filter (Q2), tenant-filtered `delete_points`
//! (Q3, Q4) and the read-only `inspect_identity` (Q5). Skipped without `MEMCORE_TEST_QDRANT_URL`;
//! every collection is test-owned and removed on drop. Invented data only.

#![cfg(feature = "qdrant")]

#[path = "support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use std::collections::BTreeSet;

use async_trait::async_trait;
use memcore_common::MemcoreResult;
use memcore_core::ports::{
    EmbeddingIdentity, EmbeddingProvider, PointScope, VectorIdentityStatus, VectorStore,
};
use memcore_core::{MemoryType, TenantContext};
use memcore_storage::{QdrantVectorStore, VectorRecord, identity_collection_name};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CountPointsBuilder, GetPointsBuilder, PointStruct, UpsertPointsBuilder,
};
use serde_json::json;
use uuid::Uuid;

const DIMS: usize = 8;

fn qdrant_url(test: &str) -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            eprintln!("skipping qdrant test `{test}`: MEMCORE_TEST_QDRANT_URL not set");
            None
        }
    }
}

fn ta() -> TenantContext {
    TenantContext::new("org_t2a", "u1").unwrap()
}
fn tb() -> TenantContext {
    TenantContext::new("org_t2a", "u2").unwrap()
}

fn raw(url: &str) -> Qdrant {
    Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap()
}

fn vector(seed: u64, text: &str) -> Vec<f32> {
    let mut h: u64 = 1469598103934665603 ^ seed;
    (0..DIMS)
        .map(|i| {
            for b in text.bytes().chain([i as u8]) {
                h ^= u64::from(b);
                h = h.wrapping_mul(1099511628211);
            }
            ((h % 2000) as f32 / 1000.0) - 1.0
        })
        .collect()
}

struct Fake(u64);

#[async_trait]
impl EmbeddingProvider for Fake {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        Ok(vector(self.0, text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector(self.0, t)).collect())
    }
    fn dimensions(&self) -> usize {
        DIMS
    }
}

fn record(tenant: &TenantContext, point: Uuid, fact: Uuid, content: &str) -> VectorRecord {
    VectorRecord {
        id: point,
        fact_id: fact,
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        embedding: vector(1, content),
        content: content.to_string(),
        memory_type: MemoryType::Preference,
        metadata: json!({}),
    }
}

async fn store(url: &str, guard: &qdrant_cleanup::TestCollections, tag: &str) -> QdrantVectorStore {
    let name = guard.name(tag);
    QdrantVectorStore::connect(url, &name, DIMS)
        .await
        .expect("connect")
}

/// Every point id in `scope`, paging with `limit`; asserts termination and no repeat.
async fn scroll_all(
    store: &QdrantVectorStore,
    scope: &PointScope,
    limit: usize,
) -> (Vec<String>, usize) {
    let mut ids = Vec::new();
    let mut cursor = None;
    let mut pages = 0usize;
    loop {
        let page = store
            .scroll_points(scope, cursor, limit)
            .await
            .expect("scroll");
        pages += 1;
        assert!(page.points.len() <= limit, "page bounded by limit");
        assert!(pages <= 10_000, "the scan must terminate");
        ids.extend(page.points.iter().map(|p| p.point_id.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let unique: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "no point repeated across pages (limit {limit})"
    );
    (ids, pages)
}

#[tokio::test]
async fn g2_q1_scroll_and_count_are_complete_and_exact_on_real_qdrant() {
    let Some(url) = qdrant_url("g2_q1") else {
        return;
    };
    let guard = qdrant_cleanup::TestCollections::new(url.clone());
    let store = store(&url, &guard, "t2q1").await;
    let empty = self::store(&url, &guard, "t2q1empty").await;
    let (a, b) = (ta(), tb());
    let mut planted_a = BTreeSet::new();
    for n in 0..600u32 {
        let point = Uuid::new_v4();
        planted_a.insert(point.to_string());
        store
            .upsert_vector(
                &a,
                record(&a, point, Uuid::new_v4(), &format!("TA point {n}")),
            )
            .await
            .unwrap();
    }
    // Three points on one fact (a divergent state verify-vectors must enumerate). The store's own
    // upsert keeps one point per fact (legacy replacement), so they are planted with the raw client.
    let shared_fact = Uuid::new_v4();
    let mut on_fact = BTreeSet::new();
    let mut shared = Vec::new();
    for n in 0..3u32 {
        let point = Uuid::new_v4();
        on_fact.insert(point.to_string());
        planted_a.insert(point.to_string());
        shared.push(PointStruct::new(
            point.to_string(),
            vector(1, &format!("shared {n}")),
            qdrant_client::Payload::try_from(json!({
                "id": point.to_string(),
                "fact_id": shared_fact.to_string(),
                "org_id": a.org_id,
                "user_id": a.user_id,
                "content": format!("TA shared fact point {n}"),
                "memory_type": "preference",
                "metadata": {}
            }))
            .unwrap(),
        ));
    }
    raw(&url)
        .upsert_points(UpsertPointsBuilder::new(store.collection_name(), shared).wait(true))
        .await
        .unwrap();
    let mut planted_b = BTreeSet::new();
    for n in 0..40u32 {
        let point = Uuid::new_v4();
        planted_b.insert(point.to_string());
        store
            .upsert_vector(
                &b,
                record(&b, point, Uuid::new_v4(), &format!("TB point {n}")),
            )
            .await
            .unwrap();
    }
    let tenant = PointScope::Tenant(a.clone());
    assert_eq!(
        store.count_points(&tenant).await.unwrap(),
        603,
        "exact count"
    );
    assert_eq!(
        store
            .count_points(&PointScope::Tenant(b.clone()))
            .await
            .unwrap(),
        40
    );
    let mut pages_by_limit = Vec::new();
    for limit in [7usize, 256, 1000] {
        let (ids, pages) = scroll_all(&store, &tenant, limit).await;
        let got: BTreeSet<String> = ids.into_iter().collect();
        assert_eq!(
            got, planted_a,
            "limit {limit}: every TA point exactly once, no TB point"
        );
        pages_by_limit.push((limit, pages));
    }
    // Page size 1: the first pages advance one point at a time with an opaque cursor.
    let mut cursor = None;
    let mut first = Vec::new();
    for _ in 0..5 {
        let page = store.scroll_points(&tenant, cursor, 1).await.unwrap();
        assert_eq!(page.points.len(), 1);
        first.push(page.points[0].point_id.clone());
        cursor = page.next_cursor;
        assert!(cursor.is_some());
    }
    assert_eq!(
        first.iter().collect::<BTreeSet<_>>().len(),
        5,
        "page size 1 advances"
    );
    let entity = PointScope::TenantEntity(a.clone(), shared_fact);
    assert_eq!(store.count_points(&entity).await.unwrap(), 3);
    let (ids, _) = scroll_all(&store, &entity, 2).await;
    assert_eq!(
        ids.into_iter().collect::<BTreeSet<_>>(),
        on_fact,
        "tenant-entity scope exact"
    );
    assert_eq!(empty.count_points(&tenant).await.unwrap(), 0);
    let page = empty.scroll_points(&tenant, None, 10).await.unwrap();
    assert!(
        page.points.is_empty() && page.next_cursor.is_none(),
        "empty collection"
    );
    eprintln!("G2 Q1: 603 TA + 40 TB points; pages by limit {pages_by_limit:?}");
}

#[tokio::test]
async fn g2_q2_unattributed_filter_finds_exactly_the_points_without_tenant_payload() {
    let Some(url) = qdrant_url("g2_q2") else {
        return;
    };
    let guard = qdrant_cleanup::TestCollections::new(url.clone());
    let store = store(&url, &guard, "t2q2").await;
    let client = raw(&url);
    let variants = [
        (
            "org_missing",
            json!({"user_id": "u1", "fact_id": Uuid::new_v4().to_string(), "content": "x"}),
        ),
        (
            "user_missing",
            json!({"org_id": "org_t2a", "fact_id": Uuid::new_v4().to_string(), "content": "x"}),
        ),
        (
            "org_empty",
            json!({"org_id": "", "user_id": "u1", "fact_id": Uuid::new_v4().to_string(), "content": "x"}),
        ),
        (
            "user_empty",
            json!({"org_id": "org_t2a", "user_id": "", "fact_id": Uuid::new_v4().to_string(), "content": "x"}),
        ),
        (
            "both_missing",
            json!({"fact_id": Uuid::new_v4().to_string(), "content": "x"}),
        ),
        (
            "org_null",
            json!({"org_id": null, "user_id": "u1", "fact_id": Uuid::new_v4().to_string(), "content": "x"}),
        ),
        (
            "valid",
            json!({"org_id": "org_t2a", "user_id": "u1", "fact_id": Uuid::new_v4().to_string(), "content": "VALID-CONTENT", "memory_type": "preference"}),
        ),
    ];
    let mut by_label = std::collections::BTreeMap::new();
    let mut points = Vec::new();
    for (label, payload) in &variants {
        let id = Uuid::new_v4();
        by_label.insert(*label, id.to_string());
        points.push(PointStruct::new(
            id.to_string(),
            vector(1, label),
            qdrant_client::Payload::try_from(payload.clone()).unwrap(),
        ));
    }
    client
        .upsert_points(UpsertPointsBuilder::new(store.collection_name(), points).wait(true))
        .await
        .unwrap();
    let (found, _) = scroll_all(&store, &PointScope::Unattributed, 2).await;
    let found: BTreeSet<String> = found.into_iter().collect();
    let expected: BTreeSet<String> = by_label
        .iter()
        .filter(|(label, _)| **label != "valid")
        .map(|(_, id)| id.clone())
        .collect();
    let missing: Vec<&str> = by_label
        .iter()
        .filter(|(l, id)| **l != "valid" && !found.contains(*id))
        .map(|(l, _)| *l)
        .collect();
    eprintln!(
        "G2 Q2: found {} unattributed; missing {missing:?}",
        found.len()
    );
    assert!(
        missing.is_empty(),
        "unattributed points not discovered: {missing:?}"
    );
    assert!(
        !found.contains(&by_label["valid"]),
        "a valid point classified unattributed"
    );
    assert_eq!(found, expected);
    assert_eq!(
        store.count_points(&PointScope::Unattributed).await.unwrap(),
        6,
        "count agrees"
    );
    let page = store
        .scroll_points(&PointScope::Unattributed, None, 100)
        .await
        .unwrap();
    assert!(
        page.points.iter().all(|p| p.content.is_none()),
        "content withheld for unattributed points"
    );
}

#[tokio::test]
async fn g2_q3_q4_tenant_filtered_delete_never_touches_another_tenant() {
    let Some(url) = qdrant_url("g2_q3") else {
        return;
    };
    let guard = qdrant_cleanup::TestCollections::new(url.clone());
    let store = store(&url, &guard, "t2q3").await;
    let (a, b) = (ta(), tb());
    let fact_a = Uuid::new_v4();
    let (p1, p2, episode_point, p_keep) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    store
        .upsert_vector(&a, record(&a, p1, fact_a, "TA fact point one"))
        .await
        .unwrap();
    store
        .upsert_vector(&a, record(&a, p2, fact_a, "TA fact point two"))
        .await
        .unwrap();
    store
        .upsert_vector(
            &a,
            record(&a, episode_point, episode_point, "TA episode stable id"),
        )
        .await
        .unwrap();
    store
        .upsert_vector(&a, record(&a, p_keep, Uuid::new_v4(), "TA unlisted point"))
        .await
        .unwrap();
    // Q4: TB's live fact, with TB's own point, and a TA-attributed point naming TB's fact.
    let fact_b = Uuid::new_v4();
    let (p_b, p_conflict) = (Uuid::new_v4(), Uuid::new_v4());
    store
        .upsert_vector(&b, record(&b, p_b, fact_b, "TB private point"))
        .await
        .unwrap();
    store
        .upsert_vector(
            &a,
            record(&a, p_conflict, fact_b, "TA payload naming TB fact"),
        )
        .await
        .unwrap();
    let absent = Uuid::new_v4();
    let before_b = scroll_all(&store, &PointScope::Tenant(b.clone()), 10)
        .await
        .0;
    store
        .delete_points(
            &a,
            &[p1, p2, episode_point, p_conflict, p_b, absent]
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
        )
        .await
        .expect("an absent id is not an error");
    let left_a: BTreeSet<String> = scroll_all(&store, &PointScope::Tenant(a.clone()), 10)
        .await
        .0
        .into_iter()
        .collect();
    assert_eq!(
        left_a,
        BTreeSet::from([p_keep.to_string()]),
        "only TA's listed points deleted"
    );
    let after_b = scroll_all(&store, &PointScope::Tenant(b.clone()), 10)
        .await
        .0;
    assert_eq!(
        after_b, before_b,
        "TB's point survives a TA delete that lists its id"
    );
    assert_eq!(
        store
            .count_points(&PointScope::Tenant(a.clone()))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .count_points(&PointScope::Tenant(b.clone()))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .count_points(&PointScope::TenantEntity(b.clone(), fact_b))
            .await
            .unwrap(),
        1,
        "TB's point on its own fact unchanged by the conflict point's removal"
    );
}

#[tokio::test]
async fn g2_q5_identity_inspection_is_read_only_and_classifies_every_state() {
    let Some(url) = qdrant_url("g2_q5") else {
        return;
    };
    let guard = qdrant_cleanup::TestCollections::new(url.clone());
    let client = raw(&url);
    let id_a = EmbeddingIdentity::remote("t2_fake", "model-a", DIMS);
    let id_b = EmbeddingIdentity::remote("t2_fake", "model-b", DIMS);
    let fake = Fake(7);
    let other_model = Fake(99);
    let valid_name = guard.name("t2q5valid");
    let valid =
        QdrantVectorStore::connect_verified(&url, &valid_name, DIMS, Some(&id_a), Some(&fake))
            .await
            .unwrap();
    let noprobe_name = guard.name("t2q5noprobe");
    let noprobe = QdrantVectorStore::connect_verified(&url, &noprobe_name, DIMS, Some(&id_a), None)
        .await
        .unwrap();
    let other_name = guard.name("t2q5other");
    let other = QdrantVectorStore::connect_verified(&url, &other_name, DIMS, Some(&id_b), None)
        .await
        .unwrap();
    let unrec_name = guard.name("t2q5unrec");
    let unrecorded = QdrantVectorStore::connect(&url, &unrec_name, DIMS)
        .await
        .unwrap();
    unrecorded
        .upsert_vector(
            &ta(),
            record(&ta(), Uuid::new_v4(), Uuid::new_v4(), "legacy point"),
        )
        .await
        .unwrap();

    async fn state(client: &Qdrant, names: &[&str]) -> String {
        let mut collections: Vec<String> = client
            .list_collections()
            .await
            .unwrap()
            .collections
            .into_iter()
            .map(|c| c.name)
            .filter(|n| names.iter().any(|base| n.starts_with(base)))
            .collect();
        collections.sort();
        let mut out = format!("{collections:?}");
        for name in &collections {
            let count = client
                .count(CountPointsBuilder::new(name).exact(true))
                .await
                .unwrap()
                .result
                .unwrap()
                .count;
            out.push_str(&format!("|{name}={count}"));
            if name.ends_with("__embedding_identity") {
                let p = client
                    .get_points(GetPointsBuilder::new(name, vec![1u64.into()]).with_payload(true))
                    .await
                    .unwrap();
                out.push_str(&format!(
                    "|{:?}",
                    p.result.first().map(|p| {
                        let mut k: Vec<_> = p.payload.keys().cloned().collect();
                        k.sort();
                        k
                    })
                ));
            }
        }
        out
    }
    let names = [
        valid_name.as_str(),
        noprobe_name.as_str(),
        other_name.as_str(),
        unrec_name.as_str(),
    ];
    let before = state(&client, &names).await;
    let results = vec![
        (
            "recorded valid + probe",
            valid
                .inspect_identity(Some(&id_a), Some(DIMS), Some(&fake))
                .await,
            VectorIdentityStatus::Verified,
        ),
        (
            "recorded valid, no probe given",
            valid.inspect_identity(Some(&id_a), Some(DIMS), None).await,
            VectorIdentityStatus::Verified,
        ),
        (
            "recorded, fingerprint from another model",
            valid
                .inspect_identity(Some(&id_a), Some(DIMS), Some(&other_model))
                .await,
            VectorIdentityStatus::Mismatch,
        ),
        (
            "recorded without probe vector",
            noprobe
                .inspect_identity(Some(&id_a), Some(DIMS), Some(&fake))
                .await,
            VectorIdentityStatus::Verified,
        ),
        (
            "recorded with another identity",
            other.inspect_identity(Some(&id_a), Some(DIMS), None).await,
            VectorIdentityStatus::Mismatch,
        ),
        (
            "unrecorded (legacy) store",
            unrecorded
                .inspect_identity(Some(&id_a), Some(DIMS), None)
                .await,
            VectorIdentityStatus::Unrecorded,
        ),
        (
            "dimension mismatch",
            valid
                .inspect_identity(Some(&id_a), Some(DIMS + 1), None)
                .await,
            VectorIdentityStatus::Mismatch,
        ),
        (
            "recorded, no configured identity",
            valid.inspect_identity(None, Some(DIMS), None).await,
            VectorIdentityStatus::Unavailable,
        ),
    ];
    let after = state(&client, &names).await;
    for (label, got, want) in &results {
        eprintln!("G2 Q5 {label}: {got:?}");
        assert_eq!(got, want, "{label}");
    }
    assert_eq!(
        after, before,
        "inspection wrote nothing (collections, counts, identity records)"
    );
    assert!(
        !client
            .collection_exists(identity_collection_name(&unrec_name))
            .await
            .unwrap(),
        "no identity record created by inspection"
    );
}
