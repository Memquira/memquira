//! User-export completeness. Exports honour their documented per-export limits exactly (facts
//! 100, memory events 100, episodes 10,000), never export the store's look-ahead row, and say
//! explicitly when a section was truncated. Mock stores here; SQLite in
//! `memcore-api/tests/export_completeness_sqlite.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use memcore_core::{
    EXPORT_EPISODES_LIMIT, EXPORT_EVENTS_LIMIT, EXPORT_FACTS_LIMIT, Episode, EpisodeComponents,
    EpisodeStore, ExportUserDataInput, Fact, FactStore, MemoryEngine, MemoryEvent,
    MemoryEventOperation, MemoryEventStore, MemorySource, MemoryType, MessageRole, TenantContext,
    UserMemoryExport,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockEpisodeStore, MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

const SIZES: [usize; 6] = [0, 1, 99, 100, 101, 250];

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).expect("tenant")
}

struct Stores {
    facts: Arc<dyn FactStore>,
    events: Arc<dyn MemoryEventStore>,
}

async fn stores() -> Stores {
    Stores {
        facts: Arc::new(MockFactStore::new()),
        events: Arc::new(MockMemoryEventStore::new()),
    }
}

fn engine(stores: &Stores) -> MemoryEngine {
    MemoryEngine::new(
        stores.facts.clone(),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(stores.events.clone())
}

/// `n` facts and `n` events for `tenant`, all with ONE identical timestamp (exact ties), so order
/// falls to the id tie-break.
async fn seed(stores: &Stores, tenant: &TenantContext, n: usize) -> (HashSet<Uuid>, HashSet<Uuid>) {
    let at = Utc.with_ymd_and_hms(2031, 3, 3, 9, 15, 0).unwrap();
    let (mut fact_ids, mut event_ids) = (HashSet::new(), HashSet::new());
    for i in 0..n {
        let fact = Fact::new(
            Uuid::new_v4(),
            tenant.org_id.clone(),
            tenant.user_id.clone(),
            MemoryType::Knowledge,
            format!("Invented export fixture fact {i}"),
            None,
            MemorySource::Manual,
            0.9,
            0.5,
            None,
            None,
            at,
            at,
            json!({}),
        )
        .expect("fact");
        fact_ids.insert(
            stores
                .facts
                .insert_fact(tenant, fact)
                .await
                .expect("insert fact")
                .id,
        );

        let mut event = MemoryEvent::new(
            tenant.org_id.clone(),
            tenant.user_id.clone(),
            None,
            MemoryEventOperation::Add,
            None,
            Some(format!("event {i}")),
            Some("mock".to_string()),
            Some("mock-llm".to_string()),
            json!({ "index": i }),
        );
        event.created_at = at;
        event_ids.insert(
            stores
                .events
                .record_event(tenant, event)
                .await
                .expect("event")
                .id,
        );
    }
    (fact_ids, event_ids)
}

async fn export(engine: &MemoryEngine, tenant: &TenantContext) -> UserMemoryExport {
    engine
        .export_user_data(ExportUserDataInput {
            tenant: tenant.clone(),
            include_events: true,
            include_deleted: false,
        })
        .await
        .expect("export")
}

/// The pre-fix symptom: never more records than the documented per-export limit.
async fn check_counts() {
    for n in SIZES {
        let stores = stores().await;
        let a = tenant("org_t1b_a", "user_shared");
        seed(&stores, &a, n).await;
        let exported = export(&engine(&stores), &a).await;
        assert_eq!(
            exported.facts.len(),
            n.min(EXPORT_FACTS_LIMIT),
            "facts, n = {n}"
        );
        assert_eq!(
            exported.memory_events.len(),
            n.min(EXPORT_EVENTS_LIMIT),
            "events, n = {n}"
        );
    }
}

#[tokio::test]
async fn export_never_exceeds_the_documented_limits() {
    check_counts().await;
}

fn truncated_sections(export: &UserMemoryExport) -> Vec<(String, usize)> {
    export
        .truncated
        .iter()
        .flatten()
        .map(|t| (t.section.clone(), t.limit))
        .collect()
}

/// Every size: exact counts, the truncation signal iff records were left out, unique ids, only the
/// requested tenant's records (two other tenants hold overlapping data with identical timestamps),
/// and a stable selection under exact timestamp ties.
#[tokio::test]
async fn export_is_complete_or_explicitly_truncated_and_tenant_scoped() {
    for n in SIZES {
        let stores = stores().await;
        let a = tenant("org_t1b_a", "user_shared");
        let other_org = tenant("org_t1b_b", "user_shared");
        let other_user = tenant("org_t1b_a", "user_other");
        let (a_facts, a_events) = seed(&stores, &a, n).await;
        seed(&stores, &other_org, 120).await;
        seed(&stores, &other_user, 120).await;
        let engine = engine(&stores);

        let first = export(&engine, &a).await;
        let fact_ids: Vec<Uuid> = first.facts.iter().map(|f| f.id).collect();
        let event_ids: Vec<Uuid> = first.memory_events.iter().map(|e| e.id).collect();

        assert_eq!(fact_ids.len(), n.min(EXPORT_FACTS_LIMIT), "facts, n = {n}");
        assert_eq!(
            event_ids.len(),
            n.min(EXPORT_EVENTS_LIMIT),
            "events, n = {n}"
        );
        assert_eq!(
            fact_ids.iter().collect::<HashSet<_>>().len(),
            fact_ids.len(),
            "duplicate facts"
        );
        assert_eq!(
            event_ids.iter().collect::<HashSet<_>>().len(),
            event_ids.len(),
            "duplicate events"
        );
        assert!(
            fact_ids.iter().all(|id| a_facts.contains(id)),
            "foreign fact, n = {n}"
        );
        assert!(
            event_ids.iter().all(|id| a_events.contains(id)),
            "foreign event, n = {n}"
        );
        assert!(
            first
                .facts
                .iter()
                .all(|f| f.org_id == a.org_id && f.user_id == a.user_id)
        );
        assert!(
            first
                .memory_events
                .iter()
                .all(|e| e.org_id == a.org_id && e.user_id == a.user_id)
        );

        let expected: Vec<(String, usize)> = if n > EXPORT_FACTS_LIMIT {
            vec![
                ("facts".to_string(), EXPORT_FACTS_LIMIT),
                ("memory_events".to_string(), EXPORT_EVENTS_LIMIT),
            ]
        } else {
            vec![]
        };
        assert_eq!(
            truncated_sections(&first),
            expected,
            "truncation signal, n = {n}"
        );
        if n <= EXPORT_FACTS_LIMIT {
            // Complete: every record exported and the format is unchanged (no `truncated` key).
            assert_eq!(fact_ids.iter().copied().collect::<HashSet<_>>(), a_facts);
            assert_eq!(event_ids.iter().copied().collect::<HashSet<_>>(), a_events);
            let json = serde_json::to_value(&first).expect("serialize");
            assert!(
                json.get("truncated").is_none(),
                "complete export must not carry `truncated`"
            );
        }

        let second = export(&engine, &a).await;
        assert_eq!(
            second.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
            fact_ids,
            "stable fact selection under timestamp ties, n = {n}"
        );
        assert_eq!(
            second
                .memory_events
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            event_ids,
            "stable event selection under timestamp ties, n = {n}"
        );
    }
}

#[tokio::test]
async fn truncation_signal_round_trips_through_serde() {
    let stores = stores().await;
    let a = tenant("org_t1b_a", "user_rt");
    seed(&stores, &a, 101).await;
    let exported = export(&engine(&stores), &a).await;
    let json = serde_json::to_string(&exported).expect("serialize");
    let back: UserMemoryExport = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.truncated, exported.truncated);
    assert_eq!(truncated_sections(&back).len(), 2);
}

fn episode(tenant: &TenantContext, i: usize) -> Episode {
    Episode {
        id: Uuid::new_v4(),
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        session_id: Some("S1".to_string()),
        message_id: Some(format!("M{i}")),
        speaker: None,
        role: MessageRole::User,
        content: format!("Invented message {i}"),
        observed_at: None,
        recorded_at: Utc.with_ymd_and_hms(2031, 3, 3, 9, 15, 0).unwrap(),
        embedded: false,
        metadata: json!({}),
    }
}

/// Episodes: the 10,000 limit is honoured exactly and signalled (10,001 stored); at the limit the
/// export is complete and unmarked.
#[tokio::test]
async fn episode_export_limit_is_exact_and_signalled() {
    for (stored, expect_truncated) in [
        (EXPORT_EPISODES_LIMIT, false),
        (EXPORT_EPISODES_LIMIT + 1, true),
    ] {
        let store = Arc::new(MockEpisodeStore::new());
        let a = tenant("org_t1b_a", "user_episodes");
        let rows: Vec<Episode> = (0..stored).map(|i| episode(&a, i)).collect();
        store
            .insert_episodes(&a, rows)
            .await
            .expect("insert episodes");
        let engine = MemoryEngine::new(
            Arc::new(MockFactStore::new()),
            Arc::new(MockVectorStore::new()),
            Arc::new(MockLlmProvider::new()),
            Arc::new(MockEmbeddingProvider::new(4)),
        )
        .with_episodes(Some(EpisodeComponents {
            store: store.clone(),
            vectors: Arc::new(MockVectorStore::new()),
        }));
        let exported = export(&engine, &a).await;
        let episodes = exported.episodes.as_ref().expect("v2 export");
        assert_eq!(episodes.len(), EXPORT_EPISODES_LIMIT);
        assert_eq!(
            episodes.iter().map(|e| e.id).collect::<HashSet<_>>().len(),
            EXPORT_EPISODES_LIMIT,
            "duplicate episodes"
        );
        let expected = if expect_truncated {
            vec![("episodes".to_string(), EXPORT_EPISODES_LIMIT)]
        } else {
            vec![]
        };
        assert_eq!(truncated_sections(&exported), expected);
    }
}
