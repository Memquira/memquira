//! Opt-in complete paginated exports and import safety, on SQLite and (with
//! MEMCORE_TEST_POSTGRES_URL) PostgreSQL. Invented data only.

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{EventBackend, FactBackend, Settings};
use memcore_core::ports::EntityStore;
use memcore_core::{
    Entity, EntityRelation, EntityRole, EntityType, Episode, EpisodeStore, Fact, FactEntityLink,
    FactStore, MemoryEventStore, MemorySource, MemoryType, MessageRole, TenantContext,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const SECRET: &str = "T1D-PRIVATE-SENTINEL";

struct Stores {
    facts: Arc<dyn FactStore>,
    events: Arc<dyn MemoryEventStore>,
    episodes: Arc<dyn EpisodeStore>,
    entities: Arc<dyn EntityStore>,
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    org: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (auth_name, auth_value) = authorization_header();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org)
        .header(auth_name, auth_value)
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn fact(t: &TenantContext, i: usize) -> Fact {
    let now = Utc::now() - Duration::seconds(i as i64 % 7);
    Fact::new(
        Uuid::new_v4(),
        t.org_id.clone(),
        t.user_id.clone(),
        MemoryType::Preference,
        format!("{SECRET} {} {} fact {i}", t.org_id, t.user_id),
        None,
        MemorySource::UserMessage,
        0.9,
        0.5,
        None,
        None,
        now,
        now,
        json!({}),
    )
    .unwrap()
}

/// `n` facts, each with its add event (content-bearing), for one tenant.
async fn seed_facts_and_events(
    s: &Stores,
    t: &TenantContext,
    n: usize,
) -> (HashSet<String>, HashSet<String>) {
    let (mut facts, mut events) = (HashSet::new(), HashSet::new());
    for i in 0..n {
        let f = fact(t, i);
        s.facts.insert_fact(t, f.clone()).await.unwrap();
        let e = s
            .events
            .record_event(
                t,
                memcore_core::audit::build_add_event(t, &f, &None, &None, json!({})),
            )
            .await
            .unwrap();
        facts.insert(f.id.to_string());
        events.insert(e.id.to_string());
    }
    (facts, events)
}

async fn seed_episodes(s: &Stores, t: &TenantContext, n: usize) -> HashSet<String> {
    let base = Utc::now() - Duration::hours(1);
    let mut ids = HashSet::new();
    let all: Vec<Episode> = (0..n)
        .map(|i| Episode {
            id: Uuid::new_v4(),
            org_id: t.org_id.clone(),
            user_id: t.user_id.clone(),
            session_id: None,
            message_id: None,
            speaker: None,
            role: MessageRole::User,
            content: format!("{SECRET} episode {i}"),
            observed_at: None,
            // Many equal timestamps on purpose: the keyset tie-break on id must hold.
            recorded_at: base + Duration::milliseconds((i / 3) as i64),
            embedded: true,
            metadata: json!({}),
        })
        .collect();
    for chunk in all.chunks(1000) {
        for e in s.episodes.insert_episodes(t, chunk.to_vec()).await.unwrap() {
            ids.insert(e.id.to_string());
        }
    }
    ids
}

async fn seed_entities(s: &Stores, t: &TenantContext, fact_id: Uuid) -> HashSet<String> {
    let now = Utc::now();
    let mut ids = Vec::new();
    for name in ["Ana", "Pixel", "Tallinn"] {
        let e = s
            .entities
            .upsert_entity(
                t,
                Entity {
                    id: Uuid::new_v4(),
                    org_id: t.org_id.clone(),
                    user_id: t.user_id.clone(),
                    canonical_name: name.to_string(),
                    entity_type: EntityType::Person,
                    aliases: vec![],
                    summary: None,
                    is_speaker: false,
                    name_embedding: None,
                    name_embedding_model: None,
                    name_embedding_dimensions: None,
                    created_at: now,
                    updated_at: now,
                },
            )
            .await
            .unwrap();
        ids.push(e.id);
    }
    for id in &ids[..2] {
        s.entities
            .link_fact(
                t,
                FactEntityLink {
                    org_id: t.org_id.clone(),
                    user_id: t.user_id.clone(),
                    fact_id,
                    entity_id: *id,
                    role: EntityRole::Subject,
                    mention: "x".into(),
                    created_at: now,
                },
            )
            .await
            .unwrap();
    }
    s.entities
        .insert_relation(
            t,
            EntityRelation {
                id: Uuid::new_v4(),
                org_id: t.org_id.clone(),
                user_id: t.user_id.clone(),
                source_entity_id: ids[0],
                relation: "owns".into(),
                target_entity_id: ids[1],
                valid_at: None,
                invalid_at: None,
                source_fact_id: Some(fact_id),
                created_at: now,
            },
        )
        .await
        .unwrap();
    ids.iter().map(Uuid::to_string).collect()
}

fn ids(export: &Value, section: &str) -> HashSet<String> {
    export[section]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x["id"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

async fn export(app: &axum::Router, org: &str, user: &str, query: &str) -> (StatusCode, Value) {
    let (status, body) = call(
        app,
        "GET",
        &format!("/api/v1/users/{user}/export{query}"),
        org,
        None,
    )
    .await;
    (status, body["export"].clone())
}

async fn scenario(app: axum::Router, s: Stores, label: &str) {
    let run = Uuid::new_v4().simple().to_string();
    let org = format!("org_a_{run}");
    let other_org = format!("org_b_{run}");

    // ---- caps vs complete, facts and events at the page / cap boundaries ---------------------
    for n in [0usize, 1, 99, 100, 101] {
        let user = format!("u{n}");
        let t = TenantContext::new(org.clone(), user.clone()).unwrap();
        let (fact_ids, event_ids) = seed_facts_and_events(&s, &t, n).await;

        let (status, default) = export(&app, &org, &user, "").await;
        assert_eq!(status, StatusCode::OK, "{label} default n={n}");
        assert_eq!(
            default["facts"].as_array().unwrap().len(),
            n.min(100),
            "{label} default cap n={n}"
        );
        assert_eq!(
            default["memory_events"].as_array().unwrap().len(),
            n.min(100)
        );
        let truncated: HashSet<String> = default["truncated"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| x["section"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let expect: HashSet<String> = if n > 100 {
            ["facts", "memory_events"].map(String::from).into()
        } else {
            HashSet::new()
        };
        assert_eq!(truncated, expect, "{label} truncation markers n={n}");
        assert!(
            default.get("completeness").is_none_or(Value::is_null),
            "the default export never claims completeness"
        );

        for page in ["", "&page_size=7", "&page_size=100", "&page_size=1000"] {
            let (status, full) = export(&app, &org, &user, &format!("?complete=true{page}")).await;
            assert_eq!(status, StatusCode::OK, "{label} complete n={n} {page}");
            assert!(full.get("truncated").is_none_or(Value::is_null));
            assert_eq!(
                ids(&full, "facts"),
                fact_ids,
                "{label} complete facts n={n} {page}"
            );
            assert_eq!(
                ids(&full, "memory_events"),
                event_ids,
                "{label} complete events n={n} {page}"
            );
            let c = &full["completeness"];
            assert_eq!(c["mode"], "complete");
            assert_eq!(c["verified_reads"], 2);
            assert_eq!(c["counts"]["facts"], n);
            assert_eq!(c["counts"]["memory_events"], n);
            assert_eq!(full["episodes"].as_array().unwrap().len(), 0);
        }
    }

    // ---- episodes across the 10,000 default cap; entities with links / relations --------------
    let t = TenantContext::new(org.clone(), "big").unwrap();
    let episode_ids = seed_episodes(&s, &t, 10_001).await;
    let (fact_ids, _) = seed_facts_and_events(&s, &t, 3).await;
    let entity_ids = seed_entities(&s, &t, fact_ids.iter().next().unwrap().parse().unwrap()).await;
    let (_, default) = export(&app, &org, "big", "").await;
    assert_eq!(
        default["episodes"].as_array().unwrap().len(),
        10_000,
        "{label} episode cap"
    );
    assert!(
        default["truncated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["section"] == "episodes")
    );
    let (status, full) = export(&app, &org, "big", "?complete=true&page_size=1000").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ids(&full, "episodes"),
        episode_ids,
        "{label} all 10,001 episodes, none missing or duplicated"
    );
    assert_eq!(full["completeness"]["counts"]["episodes"], 10_001);
    let exported_entities: HashSet<String> = full["entities"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(exported_entities, entity_ids);
    assert_eq!(full["completeness"]["counts"]["entity_links"], 2);
    assert_eq!(full["completeness"]["counts"]["entity_relations"], 1);
    // Deterministic: two complete exports are identical apart from the timestamp.
    let (_, again) = export(&app, &org, "big", "?complete=true&page_size=333").await;
    let strip = |mut v: Value| {
        v.as_object_mut().unwrap().remove("exported_at");
        v["completeness"]["page_size"] = json!(0);
        v
    };
    assert_eq!(
        strip(again),
        strip(full.clone()),
        "{label} stable, deterministic pagination"
    );

    // ---- tenant isolation: same user id in another org ----------------------------------------
    let other = TenantContext::new(other_org.clone(), "u101").unwrap();
    let (other_facts, _) = seed_facts_and_events(&s, &other, 5).await;
    let (_, mine) = export(&app, &org, "u101", "?complete=true").await;
    assert!(
        ids(&mine, "facts").is_disjoint(&other_facts),
        "{label} no other-org records"
    );
    let (_, theirs) = export(&app, &other_org, "u101", "?complete=true").await;
    assert_eq!(ids(&theirs, "facts"), other_facts);

    // ---- request validation --------------------------------------------------------------------
    for q in [
        "?complete=true&page_size=0",
        "?complete=true&page_size=1001",
        "?page_size=10",
        "?complete=true&include_events=false",
        "?complete=true&page_size=abc",
    ] {
        let (status, _) = export(&app, &org, "u1", q).await;
        assert!(
            status.is_client_error(),
            "{label} {q} must be refused, got {status}"
        );
    }

    // ---- import safety (archives imported back into their own tenant) --------------------------
    let (_, source) = export(&app, &org, "u101", "?complete=true").await;
    let body_for = |archive: &Value, mode: &str, dry_run: bool| json!({"export": archive, "mode": mode, "restore_events": true, "dry_run": dry_run});
    let before = export(&app, &org, "u101", "?complete=true").await.1;
    // Tampered: a missing page (one fact removed), a duplicated record, complete + truncated.
    let mut missing = source.clone();
    missing["facts"].as_array_mut().unwrap().pop();
    let mut duplicated = source.clone();
    let first = duplicated["facts"][0].clone();
    duplicated["facts"].as_array_mut().unwrap().push(first);
    duplicated["completeness"]["counts"]["facts"] = json!(102);
    let mut contradictory = source.clone();
    contradictory["truncated"] = json!([{"section": "facts", "limit": 100}]);
    for (name, archive) in [
        ("missing page", missing),
        ("duplicate", duplicated),
        ("complete+truncated", contradictory),
    ] {
        for mode in ["replace", "append"] {
            let (status, body) = call(
                &app,
                "POST",
                "/api/v1/users/u101/import",
                &org,
                Some(body_for(&archive, mode, false)),
            )
            .await;
            assert!(
                status.is_client_error(),
                "{label} {name} {mode}: {status} {body}"
            );
            let (_, dry) = call(
                &app,
                "POST",
                "/api/v1/users/u101/import",
                &org,
                Some(body_for(&archive, mode, true)),
            )
            .await;
            assert!(
                dry.to_string().contains("COMPLETE_EXPORT_MANIFEST_INVALID"),
                "{label} {name}: {dry}"
            );
            assert_eq!(dry["summary"]["manifest_consistent"], false);
        }
    }
    let after = export(&app, &org, "u101", "?complete=true").await.1;
    assert_eq!(
        ids(&after, "facts"),
        ids(&before, "facts"),
        "{label} rejected imports wrote nothing"
    );
    assert_eq!(ids(&after, "memory_events"), ids(&before, "memory_events"));
    // Legacy (unmarked, capped) export: importable, never declared complete.
    let (_, legacy) = export(&app, &org, "u1", "").await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/users/u1/import",
        &org,
        Some(body_for(&legacy, "append", true)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["summary"]["manifest_consistent"], false);
    // Marked truncated export: replace still refused before any write.
    let (_, capped) = export(&app, &org, "u101", "").await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/users/u101/import",
        &org,
        Some(body_for(&capped, "replace", false)),
    )
    .await;
    assert!(status.is_client_error());
    let (_, dry) = call(
        &app,
        "POST",
        "/api/v1/users/u101/import",
        &org,
        Some(body_for(&capped, "replace", true)),
    )
    .await;
    assert!(
        dry.to_string()
            .contains("TRUNCATED_EXPORT_REPLACE_REJECTED"),
        "{dry}"
    );

    // Round trip (restore): complete export -> forget-user -> replace import -> complete export.
    let (status, _) = call(&app, "DELETE", "/api/v1/users/u101", &org, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/users/u101/import",
        &org,
        Some(body_for(&source, "replace", false)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{label} round-trip import: {body}");
    assert_eq!(body["summary"]["manifest_consistent"], true);
    assert_eq!(body["summary"]["imported_facts"], 101);
    let (_, restored) = export(&app, &org, "u101", "?complete=true").await;
    let contents = |v: &Value, s: &str, k: &str| -> Vec<String> {
        let mut out: Vec<String> = v[s]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x[k].as_str().unwrap_or("").to_string())
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        contents(&restored, "facts", "content"),
        contents(&source, "facts", "content"),
        "{label} facts round-trip"
    );
    assert_eq!(restored["completeness"]["counts"]["facts"], 101);
    let restored_events: Vec<String> = contents(&restored, "memory_events", "new_content")
        .into_iter()
        .filter(|c| !c.is_empty())
        .collect();
    let source_events: Vec<String> = contents(&source, "memory_events", "new_content")
        .into_iter()
        .filter(|c| !c.is_empty())
        .collect();
    assert_eq!(
        restored_events, source_events,
        "{label} content events round-trip"
    );

    // ---- After forget-user, no content resurfaces in a complete export -------------------
    let (status, _) = call(&app, "DELETE", "/api/v1/users/u101", &org, None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, gone) = export(&app, &org, "u101", "?complete=true").await;
    assert!(
        !gone.to_string().contains(SECRET),
        "{label} forgotten content resurfaced in a complete export"
    );
    assert_eq!(gone["completeness"]["counts"]["facts"], 0);
    // The other org's same-id user is untouched.
    let (_, theirs) = export(&app, &other_org, "u101", "?complete=true").await;
    assert_eq!(ids(&theirs, "facts"), other_facts);
}

fn base_settings() -> Settings {
    let mut s = Settings::default();
    s.episodes_enabled = true;
    s.entities_enabled = true;
    s
}

#[tokio::test]
async fn complete_export_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("e.db").to_string_lossy().replace('\\', "/")
    );
    let mut settings = base_settings();
    settings.fact_backend = FactBackend::Sqlite;
    settings.event_backend = EventBackend::Sqlite;
    settings.database_url = url.clone();
    let app = create_app(AppState::initialize(settings).await.expect("app"));
    let pool = memcore_storage::connect_sqlite_pool(&url).await.unwrap();
    let stores = Stores {
        facts: Arc::new(memcore_storage::SqliteFactStore::new(pool.clone())),
        events: Arc::new(memcore_storage::SqliteMemoryEventStore::new(pool.clone())),
        episodes: Arc::new(memcore_storage::SqliteEpisodeStore::new(pool.clone())),
        entities: Arc::new(memcore_storage::SqliteEntityStore::new(pool)),
    };
    scenario(app, stores, "sqlite").await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn complete_export_on_postgres() {
    let Ok(url) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    let mut settings = base_settings();
    settings.fact_backend = FactBackend::Postgres;
    settings.event_backend = EventBackend::Postgres;
    settings.postgres_url = Some(url.clone());
    let app = create_app(AppState::initialize(settings).await.expect("app"));
    let pool = memcore_storage::PostgresFactStore::connect(&url)
        .await
        .unwrap()
        .pool();
    let stores = Stores {
        facts: Arc::new(memcore_storage::PostgresFactStore::new(pool.clone())),
        events: Arc::new(memcore_storage::PostgresMemoryEventStore::new(pool.clone())),
        episodes: Arc::new(memcore_storage::PostgresEpisodeStore::new(pool.clone())),
        entities: Arc::new(memcore_storage::PostgresEntityStore::new(pool)),
    };
    scenario(app, stores, "postgres").await;
}

/// The memory bound: a complete export larger than MEMCORE_EXPORT_MAX_RECORDS is refused, never
/// returned partially.
#[tokio::test]
async fn complete_export_refuses_beyond_the_record_bound() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("b.db").to_string_lossy().replace('\\', "/")
    );
    let mut settings = base_settings();
    settings.fact_backend = FactBackend::Sqlite;
    settings.event_backend = EventBackend::Sqlite;
    settings.database_url = url.clone();
    settings.export_max_records = 50;
    let app = create_app(AppState::initialize(settings).await.expect("app"));
    let pool = memcore_storage::connect_sqlite_pool(&url).await.unwrap();
    let stores = Stores {
        facts: Arc::new(memcore_storage::SqliteFactStore::new(pool.clone())),
        events: Arc::new(memcore_storage::SqliteMemoryEventStore::new(pool.clone())),
        episodes: Arc::new(memcore_storage::SqliteEpisodeStore::new(pool.clone())),
        entities: Arc::new(memcore_storage::SqliteEntityStore::new(pool)),
    };
    let t = TenantContext::new("org_bound", "u").unwrap();
    seed_facts_and_events(&stores, &t, 30).await; // 60 records > 50
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/users/u/export?complete=true&page_size=10",
        "org_bound",
        None,
    )
    .await;
    assert!(status.is_client_error(), "{status}");
    assert!(body.to_string().contains("EXPORT_TOO_LARGE"), "{body}");
    assert!(body.get("export").is_none(), "no partial archive");
    // The default (capped) export is unaffected by the complete-export bound.
    let (status, _) = call(&app, "GET", "/api/v1/users/u/export", "org_bound", None).await;
    assert_eq!(status, StatusCode::OK);
}
