//! on the real SQLite stores and the wire format: the export honours its documented limits
//! exactly, selects the newest records deterministically under exact timestamp ties, stays inside
//! the requested tenant, and carries `truncated` on the wire only when records were left out (and
//! the import path accepts it).

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use memcore_api::dto::export::{
    ExportUserResponse, UserMemoryExportResponse, user_memory_export_from_response,
};
use memcore_core::{
    EXPORT_EVENTS_LIMIT, EXPORT_FACTS_LIMIT, ExportUserDataInput, Fact, FactStore, MemoryEngine,
    MemoryEvent, MemoryEventOperation, MemoryEventStore, MemorySource, MemoryType, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockVectorStore, SqliteFactStore, SqliteMemoryEventStore};
use serde_json::json;
use uuid::Uuid;

const SIZES: [usize; 6] = [0, 1, 99, 100, 101, 250];

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).expect("tenant")
}

async fn seed(
    facts: &SqliteFactStore,
    events: &SqliteMemoryEventStore,
    tenant: &TenantContext,
    n: usize,
) -> (Vec<Uuid>, HashSet<Uuid>) {
    let at = Utc.with_ymd_and_hms(2031, 3, 3, 9, 15, 0).unwrap();
    let (mut fact_ids, mut event_ids) = (Vec::new(), HashSet::new());
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
        fact_ids.push(facts.insert_fact(tenant, fact).await.expect("insert").id);
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
        event_ids.insert(events.record_event(tenant, event).await.expect("event").id);
    }
    (fact_ids, event_ids)
}

#[tokio::test]
async fn sqlite_export_is_exact_deterministic_scoped_and_signalled_on_the_wire() {
    for n in SIZES {
        let facts = Arc::new(
            SqliteFactStore::connect("sqlite::memory:")
                .await
                .expect("facts"),
        );
        let events = Arc::new(
            SqliteMemoryEventStore::connect("sqlite::memory:")
                .await
                .expect("events"),
        );
        let a = tenant("org_t1b_a", "user_shared");
        let (a_facts, a_events) = seed(&facts, &events, &a, n).await;
        seed(&facts, &events, &tenant("org_t1b_b", "user_shared"), 120).await;
        seed(&facts, &events, &tenant("org_t1b_a", "user_other"), 120).await;
        let engine = MemoryEngine::new(
            facts.clone(),
            Arc::new(MockVectorStore::new()),
            Arc::new(MockLlmProvider::new()),
            Arc::new(MockEmbeddingProvider::new(4)),
        )
        .with_event_store(events.clone());

        let export = engine
            .export_user_data(ExportUserDataInput {
                tenant: a.clone(),
                include_events: true,
                include_deleted: false,
            })
            .await
            .expect("export");

        // Exact counts; nothing foreign; no duplicates.
        let fact_ids: Vec<Uuid> = export.facts.iter().map(|f| f.id).collect();
        let event_ids: Vec<Uuid> = export.memory_events.iter().map(|e| e.id).collect();
        assert_eq!(fact_ids.len(), n.min(EXPORT_FACTS_LIMIT), "facts, n = {n}");
        assert_eq!(
            event_ids.len(),
            n.min(EXPORT_EVENTS_LIMIT),
            "events, n = {n}"
        );
        assert_eq!(
            fact_ids.iter().collect::<HashSet<_>>().len(),
            fact_ids.len()
        );
        assert_eq!(
            event_ids.iter().collect::<HashSet<_>>().len(),
            event_ids.len()
        );
        assert!(
            event_ids.iter().all(|id| a_events.contains(id)),
            "foreign event, n = {n}"
        );

        // Deterministic selection under exact timestamp ties: the store order is
        // `updated_at DESC, id DESC`, so the export holds exactly the first `limit` ids by id DESC.
        let mut by_store_order: Vec<String> = a_facts.iter().map(Uuid::to_string).collect();
        by_store_order.sort_unstable_by(|x, y| y.cmp(x));
        let expected: Vec<String> = by_store_order
            .into_iter()
            .take(EXPORT_FACTS_LIMIT)
            .collect();
        let exported: Vec<String> = fact_ids.iter().map(Uuid::to_string).collect();
        assert_eq!(exported, expected, "newest-first selection, n = {n}");

        // Wire format: `truncated` only on incomplete exports; the import path accepts it.
        let wire = serde_json::to_value(ExportUserResponse::from(export)).expect("serialize");
        let truncated = wire["export"].get("truncated");
        if n > EXPORT_FACTS_LIMIT {
            assert_eq!(
                truncated.cloned(),
                Some(json!([
                    { "section": "facts", "limit": EXPORT_FACTS_LIMIT },
                    { "section": "memory_events", "limit": EXPORT_EVENTS_LIMIT }
                ])),
                "n = {n}"
            );
        } else {
            assert!(
                truncated.is_none(),
                "complete export must not carry `truncated`, n = {n}"
            );
        }
        let request: UserMemoryExportResponse =
            serde_json::from_value(wire["export"].clone()).expect("import request parses");
        let back = user_memory_export_from_response(request).expect("import conversion");
        assert_eq!(back.truncated.is_some(), n > EXPORT_FACTS_LIMIT);
    }
}
