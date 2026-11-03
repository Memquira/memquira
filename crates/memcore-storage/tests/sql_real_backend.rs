//! SQL storage primitives on real backends:
//! the SQL primitives O8 relies on, on the REAL PostgreSQL store and on SQLite, with the same fixed
//! ids, and the two results compared (any difference is reported, never normalised).
//!
//! PG1 deleted-inclusive read; PG2 tenant-scoped keyset enumeration; PG3 global existence-only
//! checks; PG4 UUIDv8 (C+ derived) ids. Each scenario runs on a fresh PostgreSQL database (created
//! and dropped by the test on the disposable server at `MEMCORE_TEST_POSTGRES_URL`) and a fresh
//! SQLite file. Skipped without `MEMCORE_TEST_POSTGRES_URL`. Invented data only.

#![cfg(feature = "postgres")]

use chrono::{TimeZone, Utc};
use memcore_core::ports::{EpisodeStore, FactStore};
use memcore_core::{
    Episode, Fact, MemorySource, MemoryType, MessageRole, TenantContext, derived_import_fact_id,
};
use memcore_storage::{
    PostgresEpisodeStore, PostgresFactStore, SqliteEpisodeStore, SqliteFactStore,
};
use serde_json::{Value, json};
use sqlx::postgres::PgPool;
use uuid::Uuid;

fn ta() -> TenantContext {
    TenantContext::new("org_t2a", "u1").unwrap()
}
fn tb() -> TenantContext {
    TenantContext::new("org_t2a", "u2").unwrap()
}
fn tc() -> TenantContext {
    TenantContext::new("org_t2b", "u1").unwrap()
}

/// Fixed v4-shaped ids so PostgreSQL and SQLite see identical keys.
fn fid(n: u128) -> Uuid {
    Uuid::from_u128(0x7a00_0000_0000_4000_8000_0000_0000_0000 | n)
}

fn fact(tenant: &TenantContext, id: Uuid, content: &str) -> Fact {
    let at = Utc.with_ymd_and_hms(2026, 4, 1, 10, 0, 0).unwrap();
    Fact::new(
        id,
        tenant.org_id.clone(),
        tenant.user_id.clone(),
        MemoryType::Preference,
        content,
        None,
        MemorySource::ApiImport,
        0.9,
        0.6,
        None,
        None,
        at,
        at,
        json!({"owner": tenant.user_id}),
    )
    .unwrap()
}

fn episode(tenant: &TenantContext, id: Uuid, content: &str, message: &str) -> Episode {
    Episode {
        id,
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        session_id: Some("S1".to_string()),
        message_id: Some(message.to_string()),
        speaker: None,
        role: MessageRole::User,
        content: content.to_string(),
        observed_at: None,
        recorded_at: Utc.with_ymd_and_hms(2026, 4, 1, 10, 0, 0).unwrap(),
        embedded: false,
        metadata: json!({}),
    }
}

struct Pg {
    admin: PgPool,
    name: String,
    facts: PostgresFactStore,
    episodes: PostgresEpisodeStore,
}

async fn fresh_pg(test: &str) -> Option<Pg> {
    let url = match std::env::var("MEMCORE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!("skipping postgres test `{test}`: MEMCORE_TEST_POSTGRES_URL not set");
            return None;
        }
    };
    let admin = PgPool::connect(&url).await.expect("admin connection");
    let name = format!("ar023t2_g1_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .expect("create database");
    let base = url
        .rsplit_once('/')
        .map(|(base, _)| base)
        .expect("url path");
    // The project's normal path: `connect` runs the PostgreSQL migrations.
    let facts = PostgresFactStore::connect(&format!("{base}/{name}"))
        .await
        .expect("postgres fact store");
    let episodes = PostgresEpisodeStore::new(facts.pool());
    Some(Pg {
        admin,
        name,
        facts,
        episodes,
    })
}

async fn drop_pg(pg: Pg) {
    pg.facts.pool().close().await;
    sqlx::query(&format!("DROP DATABASE IF EXISTS {}", pg.name))
        .execute(&pg.admin)
        .await
        .expect("drop database");
}

struct Sl {
    _dir: tempfile::TempDir,
    facts: SqliteFactStore,
    episodes: SqliteEpisodeStore,
}

async fn fresh_sqlite() -> Sl {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t2.db");
    std::fs::File::create(&path).expect("db file");
    let url = format!("sqlite:{}", path.to_string_lossy().replace('\\', "/"));
    let facts = SqliteFactStore::connect(&url).await.expect("sqlite facts");
    let episodes = SqliteEpisodeStore::connect(&url)
        .await
        .expect("sqlite episodes");
    Sl {
        _dir: dir,
        facts,
        episodes,
    }
}

/// PG1 + PG3 + PG4 on one backend; returns every observation as JSON.
async fn primitives(facts: &dyn FactStore, episodes: &dyn EpisodeStore) -> Value {
    let (a, b, c) = (ta(), tb(), tc());
    let (f1, f2, b1, b2, c1) = (fid(1), fid(2), fid(11), fid(12), fid(21));
    facts
        .insert_fact(&a, fact(&a, f1, "TA live fact one"))
        .await
        .unwrap();
    facts
        .insert_fact(&a, fact(&a, f2, "TA fact two, soft-deleted"))
        .await
        .unwrap();
    facts.soft_delete_fact(&a, f2).await.unwrap();
    facts
        .insert_fact(&b, fact(&b, b1, "TB private live fact"))
        .await
        .unwrap();
    facts
        .insert_fact(&b, fact(&b, b2, "TB private deleted fact"))
        .await
        .unwrap();
    facts.soft_delete_fact(&b, b2).await.unwrap();
    facts
        .insert_fact(&c, fact(&c, c1, "TC private fact, same user id"))
        .await
        .unwrap();

    let state = |s: Option<memcore_core::ports::StoredFactState>| {
        s.map(|s| json!({"id": s.fact.id, "deleted": s.deleted, "content": s.fact.content, "org": s.fact.org_id, "user": s.fact.user_id, "metadata": s.fact.metadata}))
    };
    let pg1 = json!({
        "live": state(facts.get_fact_including_deleted(&a, f1).await.unwrap()),
        "soft_deleted": state(facts.get_fact_including_deleted(&a, f2).await.unwrap()),
        "get_fact_deleted": facts.get_fact(&a, f2).await.unwrap().map(|f| f.id),
        "get_fact_live": facts.get_fact(&a, f1).await.unwrap().map(|f| f.id),
        "unknown": state(facts.get_fact_including_deleted(&a, fid(99)).await.unwrap()),
        "same_org_other_user_live": state(facts.get_fact_including_deleted(&a, b1).await.unwrap()),
        "same_org_other_user_deleted": state(facts.get_fact_including_deleted(&a, b2).await.unwrap()),
        "same_user_other_org": state(facts.get_fact_including_deleted(&a, c1).await.unwrap()),
    });

    // PG3: existence only (episodes: one kept, two hard-deleted by the user's deletion).
    let (ea, eb1, eb2) = (fid(31), fid(32), fid(33));
    episodes
        .insert_episodes(
            &a,
            vec![episode(&a, ea, "TA episode kept for existence", "m1")],
        )
        .await
        .unwrap();
    episodes
        .insert_episodes(
            &b,
            vec![
                episode(&b, eb1, "TB episode one to hard delete", "m1"),
                episode(&b, eb2, "TB episode two to hard delete", "m2"),
            ],
        )
        .await
        .unwrap();
    let hard_deleted = episodes.delete_user_episodes(&b).await.unwrap();
    let pg3 = json!({
        "fact_other_tenant_live": facts.fact_id_exists_in_any_tenant(b1).await.unwrap(),
        "fact_other_tenant_deleted": facts.fact_id_exists_in_any_tenant(b2).await.unwrap(),
        "fact_unknown": facts.fact_id_exists_in_any_tenant(fid(98)).await.unwrap(),
        "episode_existing": episodes.episode_id_exists_in_any_tenant(ea).await.unwrap(),
        "episode_hard_deleted": episodes.episode_id_exists_in_any_tenant(eb1).await.unwrap(),
        "episode_unknown": episodes.episode_id_exists_in_any_tenant(fid(97)).await.unwrap(),
        "episodes_hard_deleted_count": hard_deleted,
    });

    // PG4: a C+ derived (UUIDv8) id through the store.
    let v8 = derived_import_fact_id(&a, fid(41));
    assert_eq!(v8.get_version_num(), 8);
    facts
        .insert_fact(&a, fact(&a, v8, "TA fact under a derived v8 id"))
        .await
        .unwrap();
    let got = facts
        .get_fact(&a, v8)
        .await
        .unwrap()
        .expect("v8 row readable");
    let json_round_trip: Fact =
        serde_json::from_value(serde_json::to_value(&got).unwrap()).unwrap();
    facts.soft_delete_fact(&a, v8).await.unwrap();
    let listed = facts
        .list_fact_states(&a, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.fact.id == v8)
        .map(|s| s.deleted);
    let pg4 = json!({
        "id_text": v8.to_string(),
        "stored_id_text": got.id.to_string(),
        "json_round_trip_equal": json_round_trip == got,
        "after_soft_delete_get": facts.get_fact(&a, v8).await.unwrap().map(|f| f.id),
        "after_soft_delete_state_deleted": listed,
        "exists_any_tenant": facts.fact_id_exists_in_any_tenant(v8).await.unwrap(),
    });
    json!({"pg1": pg1, "pg3": pg3, "pg4": pg4})
}

#[tokio::test]
async fn g1_pg1_pg3_pg4_primitives_on_postgres_match_sqlite() {
    let Some(pg) = fresh_pg("g1_primitives").await else {
        return;
    };
    let sl = fresh_sqlite().await;
    let on_pg = primitives(&pg.facts, &pg.episodes).await;
    let on_sl = primitives(&sl.facts, &sl.episodes).await;
    eprintln!("G1 primitives (PostgreSQL): {on_pg:#}");
    // PG1
    assert_eq!(on_pg["pg1"]["live"]["deleted"], false);
    assert_eq!(on_pg["pg1"]["soft_deleted"]["deleted"], true);
    assert!(
        on_pg["pg1"]["get_fact_deleted"].is_null(),
        "get_fact excludes deleted rows"
    );
    assert!(!on_pg["pg1"]["get_fact_live"].is_null());
    for key in [
        "unknown",
        "same_org_other_user_live",
        "same_org_other_user_deleted",
        "same_user_other_org",
    ] {
        assert!(
            on_pg["pg1"][key].is_null(),
            "PG1 {key}: another tenant's row reads as absent"
        );
    }
    assert!(
        !on_pg.to_string().contains("TB private"),
        "no TB content in any TA result"
    );
    assert!(
        !on_pg.to_string().contains("TC private"),
        "no TC content in any TA result"
    );
    // PG3
    assert_eq!(on_pg["pg3"]["fact_other_tenant_live"], true);
    assert_eq!(on_pg["pg3"]["fact_other_tenant_deleted"], true);
    assert_eq!(on_pg["pg3"]["fact_unknown"], false);
    assert_eq!(on_pg["pg3"]["episode_existing"], true);
    assert_eq!(on_pg["pg3"]["episode_hard_deleted"], false);
    assert_eq!(on_pg["pg3"]["episode_unknown"], false);
    // PG4
    assert_eq!(on_pg["pg4"]["id_text"], on_pg["pg4"]["stored_id_text"]);
    assert_eq!(on_pg["pg4"]["json_round_trip_equal"], true);
    assert!(on_pg["pg4"]["after_soft_delete_get"].is_null());
    assert_eq!(on_pg["pg4"]["after_soft_delete_state_deleted"], true);
    assert_eq!(on_pg["pg4"]["exists_any_tenant"], true);
    // PAR: SQLite gives the same observations.
    assert_eq!(
        on_sl, on_pg,
        "PostgreSQL / SQLite difference (reported, not normalised)"
    );
    drop_pg(pg).await;
}

/// PG2 on one backend: the full keyset enumeration with page size `limit`.
async fn enumerate(facts: &dyn FactStore, limit: usize) -> Vec<(Uuid, bool)> {
    let mut out = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = facts.list_fact_states(&ta(), after, limit).await.unwrap();
        assert!(page.len() <= limit, "at most `limit` rows per page");
        pages += 1;
        assert!(pages < 10_000, "enumeration must terminate");
        if page.is_empty() {
            break;
        }
        after = page.last().map(|s| s.fact.id);
        let full = page.len() == limit;
        out.extend(page.into_iter().map(|s| (s.fact.id, s.deleted)));
        if !full {
            break;
        }
    }
    out
}

async fn pagination(facts: &dyn FactStore) -> Value {
    let (a, b) = (ta(), tb());
    for n in 0..25u128 {
        facts
            .insert_fact(&a, fact(&a, fid(100 + n), &format!("TA paged fact {n}")))
            .await
            .unwrap();
        if n % 5 == 0 {
            facts.soft_delete_fact(&a, fid(100 + n)).await.unwrap();
        }
    }
    for n in 0..10u128 {
        facts
            .insert_fact(&b, fact(&b, fid(200 + n), &format!("TB paged fact {n}")))
            .await
            .unwrap();
    }
    let mut expected: Vec<(Uuid, bool)> = (0..25u128).map(|n| (fid(100 + n), n % 5 == 0)).collect();
    expected.sort();
    let mut runs = serde_json::Map::new();
    for limit in [1usize, 3, 7, 100] {
        let got = enumerate(facts, limit).await;
        assert_eq!(
            got, expected,
            "limit {limit}: complete, ordered, no duplicate, deleted included"
        );
        runs.insert(limit.to_string(), json!(got.len()));
    }
    // Under the supported non-concurrent contract: insert two, soft-delete one, enumerate again.
    facts
        .insert_fact(&a, fact(&a, fid(150), "TA paged fact late one"))
        .await
        .unwrap();
    facts
        .insert_fact(
            &a,
            fact(&a, fid(99), "TA paged fact late two (sorts first)"),
        )
        .await
        .unwrap();
    facts.soft_delete_fact(&a, fid(101)).await.unwrap();
    expected.push((fid(150), false));
    expected.push((fid(99), false));
    for item in expected.iter_mut() {
        if item.0 == fid(101) {
            item.1 = true;
        }
    }
    expected.sort();
    let after_changes = enumerate(facts, 4).await;
    assert_eq!(after_changes, expected, "fresh enumeration after changes");
    json!({"pages": runs, "after_changes": after_changes.len(), "order": after_changes.iter().map(|(id, d)| format!("{id}:{d}")).collect::<Vec<_>>()})
}

#[tokio::test]
async fn g1_pg2_keyset_enumeration_on_postgres_matches_sqlite() {
    let Some(pg) = fresh_pg("g1_pagination").await else {
        return;
    };
    let sl = fresh_sqlite().await;
    let on_pg = pagination(&pg.facts).await;
    let on_sl = pagination(&sl.facts).await;
    eprintln!(
        "G1 pagination (PostgreSQL): pages {} after_changes {}",
        on_pg["pages"], on_pg["after_changes"]
    );
    assert_eq!(
        on_sl, on_pg,
        "PostgreSQL / SQLite ordering or content difference"
    );
    drop_pg(pg).await;
}
