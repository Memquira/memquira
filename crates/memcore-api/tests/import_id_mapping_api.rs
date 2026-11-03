//! Protocol amendment v1.1 on in-process SQLite,
//! as clients see it. The mock fact store reports a global-id collision differently from SQL (S3I
//! design §6), so the API-visible outcomes are taken here: IS13 (the IS1-IS3, IS7 and IS8 HTTP
//! traces across worlds), the SQL error class of the IS19 legacy collision and IS21(8) (a derived
//! id round-trips through SQLite storage). Every request carries the same `X-Request-ID`, so
//! bodies compare byte for byte.
//!
//! Worlds (one SQLite file each): W0 the source ids are absent; W1 TB holds them live; W2 TB holds
//! them soft-deleted. TA (the importer) starts empty and sends the same export in every world.
//! Runs on SQLite always. Invented data only.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{EventBackend, FactBackend, Settings};
use memcore_core::ports::FactStore;
use memcore_core::{
    DUPLICATE_FACT_ID, Fact, MemorySource, MemoryType, TenantContext, derived_import_fact_id,
};
use memcore_storage::SqliteFactStore;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const REQUEST_ID: &str = "ar023-s3i-fixed-request-id";
const ORG: &str = "org_s3i";
const TA: &str = "user_ta";
const TB: &str = "user_tb";

fn ta() -> TenantContext {
    TenantContext::new(ORG, TA).unwrap()
}
fn tb() -> TenantContext {
    TenantContext::new(ORG, TB).unwrap()
}
fn tc() -> TenantContext {
    TenantContext::new("org_s3i_other", TA).unwrap()
}

struct World {
    _dir: tempfile::TempDir,
    url: String,
    app: axum::Router,
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memcore.db");
        std::fs::File::create(&path).expect("db file");
        let url = format!("sqlite:{}", path.to_string_lossy().replace('\\', "/"));
        let state = AppState::initialize(Settings {
            fact_backend: FactBackend::Sqlite,
            event_backend: EventBackend::Sqlite,
            database_url: url.clone(),
            ..Settings::default()
        })
        .await
        .expect("sqlite app state");
        Self {
            _dir: dir,
            url,
            app: create_app(state),
        }
    }

    async fn store(&self) -> SqliteFactStore {
        SqliteFactStore::connect(&self.url)
            .await
            .expect("sqlite store")
    }

    /// A pre-existing row planted directly in SQL (another tenant's data, or a legacy import).
    async fn plant(&self, tenant: &TenantContext, id: Uuid, content: &str, deleted: bool) {
        let store = self.store().await;
        let at = Utc.with_ymd_and_hms(2026, 2, 1, 8, 0, 0).unwrap();
        let fact = Fact::new(
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
            json!({}),
        )
        .unwrap();
        store.insert_fact(tenant, fact).await.expect("plant");
        if deleted {
            store
                .soft_delete_fact(tenant, id)
                .await
                .expect("soft delete");
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        org: &str,
        body: Option<String>,
    ) -> (StatusCode, Vec<u8>) {
        let (name, value) = authorization_header();
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("X-Organization-ID", org)
            .header("X-Request-ID", REQUEST_ID)
            .header(name, value);
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body)),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (
            status,
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
    }

    async fn seed(&self, user: &str, content: &str) {
        let body = json!({"user_id": user, "messages": [{"role": "user", "content": content}], "metadata": {}});
        let (status, _) = self
            .send("POST", "/api/v1/memories", ORG, Some(body.to_string()))
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    async fn export(&self, user: &str) -> Value {
        let (status, bytes) = self
            .send("GET", &format!("/api/v1/users/{user}/export"), ORG, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_slice::<Value>(&bytes).unwrap()["export"].clone()
    }

    async fn import(&self, export: &Value, mode: &str, dry_run: bool) -> (StatusCode, Vec<u8>) {
        let body = json!({"export": export, "mode": mode, "dry_run": dry_run}).to_string();
        self.send(
            "POST",
            &format!("/api/v1/users/{TA}/import"),
            ORG,
            Some(body),
        )
        .await
    }

    /// TA's observable state: the list and the export's facts (the export time is not data).
    async fn observe_ta(&self) -> Value {
        let (status, list) = self
            .send(
                "GET",
                &format!("/api/v1/users/{TA}/memories?limit=100"),
                ORG,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let mut export = self.export(TA).await;
        export["exported_at"] = json!("T");
        json!({"list": serde_json::from_slice::<Value>(&list).unwrap(), "facts": export["facts"]})
    }

    /// Every fact row of a tenant, deleted included, straight from SQL (IS15).
    async fn rows(&self, tenant: &TenantContext) -> Vec<(Uuid, String, bool)> {
        let mut rows: Vec<_> = self
            .store()
            .await
            .list_fact_states(tenant, None, 1000)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.fact.id, s.fact.content, s.deleted))
            .collect();
        rows.sort();
        rows
    }
}

/// The export X of TA, produced by a source deployment (its ids are memcore-minted v4 ids).
async fn source_export() -> Value {
    let source = World::new().await;
    source
        .seed(
            TA,
            "Avery leads the Tuesday trail running club in the hills.",
        )
        .await;
    source
        .seed(TA, "Avery prefers oat milk in every coffee order.")
        .await;
    let export = source.export(TA).await;
    assert_eq!(export["facts"].as_array().unwrap().len(), 2);
    export
}

fn source_ids(export: &Value) -> Vec<Uuid> {
    export["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| Uuid::parse_str(f["id"].as_str().unwrap()).unwrap())
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Foreign {
    Absent,
    Live,
    Deleted,
}

async fn world(foreign: Foreign, ids: &[Uuid]) -> World {
    let w = World::new().await;
    w.plant(&tb(), Uuid::new_v4(), "TB unrelated canary fact.", false)
        .await;
    w.plant(
        &tc(),
        Uuid::new_v4(),
        "TC canary fact, same user id.",
        false,
    )
    .await;
    for (n, id) in ids.iter().enumerate() {
        match foreign {
            Foreign::Absent => {}
            Foreign::Live => {
                w.plant(&tb(), *id, &format!("TB private fact {n}."), false)
                    .await
            }
            Foreign::Deleted => {
                w.plant(&tb(), *id, &format!("TB private fact {n}."), true)
                    .await
            }
        }
    }
    w
}

#[tokio::test]
async fn is13_append_traces_are_identical_across_worlds_on_sqlite() {
    let x = source_export().await;
    let ids = source_ids(&x);
    let mut traces: Vec<(String, Value)> = Vec::new();
    for foreign in [Foreign::Absent, Foreign::Live, Foreign::Deleted] {
        let w = world(foreign, &ids).await;
        let (tb_before, tc_before) = (w.rows(&tb()).await, w.rows(&tc()).await);
        let first = w.import(&x, "append", false).await;
        assert_eq!(
            first.0,
            StatusCode::OK,
            "{foreign:?}: {}",
            String::from_utf8_lossy(&first.1)
        );
        let after = w.observe_ta().await;
        let second = w.import(&x, "append", false).await;
        assert_eq!(second.0, StatusCode::CONFLICT, "IS8");
        assert_eq!(
            serde_json::from_slice::<Value>(&second.1).unwrap()["error"]["code"],
            "IMPORT_ALREADY_APPLIED"
        );
        assert_eq!(w.observe_ta().await, after, "IS8: nothing written");
        // IS21(8): the derived ids are what SQLite stores and returns.
        let store = w.store().await;
        for id in &ids {
            let derived = derived_import_fact_id(&ta(), *id);
            assert_eq!(derived.get_version_num(), 8);
            assert!(
                store.get_fact(&ta(), derived).await.unwrap().is_some(),
                "M(TA, f) stored"
            );
            assert!(
                store.get_fact(&ta(), *id).await.unwrap().is_none(),
                "source id not stored for TA"
            );
        }
        assert_eq!(w.rows(&tb()).await, tb_before, "IS15: TB unchanged");
        assert_eq!(w.rows(&tc()).await, tc_before, "IS15: TC unchanged");
        traces.push((
            format!("{foreign:?}"),
            json!([
                first.0.as_u16(),
                String::from_utf8(first.1).unwrap(),
                after,
                second.0.as_u16(),
                String::from_utf8(second.1).unwrap()
            ]),
        ));
    }
    for (label, trace) in &traces[1..] {
        assert_eq!(trace, &traces[0].1, "{label} differs from W0");
    }
}

#[tokio::test]
async fn is13_is7_duplicate_ids_are_identical_400s_before_any_write_on_sqlite() {
    let mut x = source_export().await;
    let ids = source_ids(&x);
    let mut twin = x["facts"][0].clone();
    twin["content"] = json!("A second record reusing the first record's id.");
    x["facts"].as_array_mut().unwrap().push(twin);
    let mut traces: Vec<(String, Value)> = Vec::new();
    for foreign in [Foreign::Absent, Foreign::Live] {
        let w = world(foreign, &ids).await;
        w.seed(TA, "TA prior memory about sailing small boats.")
            .await;
        let before = w.rows(&ta()).await;
        let (tb_before, tc_before) = (w.rows(&tb()).await, w.rows(&tc()).await);
        let mut trace = Vec::new();
        for mode in ["append", "replace"] {
            let (status, bytes) = w.import(&x, mode, false).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{mode}");
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert!(text.contains(DUPLICATE_FACT_ID), "{text}");
            assert!(!text.contains(&ids[0].to_string()), "no id in the body");
            assert_eq!(
                w.rows(&ta()).await,
                before,
                "{mode}: TA intact (replace deleted nothing)"
            );
            let (dry_status, dry) = w.import(&x, mode, true).await;
            assert_eq!(dry_status, StatusCode::OK);
            assert!(
                String::from_utf8_lossy(&dry).contains(DUPLICATE_FACT_ID),
                "dry run lists the code"
            );
            trace.push(json!([
                status.as_u16(),
                text,
                String::from_utf8(dry).unwrap()
            ]));
        }
        assert_eq!(w.rows(&tb()).await, tb_before, "IS15: TB unchanged");
        assert_eq!(w.rows(&tc()).await, tc_before, "IS15: TC unchanged");
        traces.push((format!("{foreign:?}"), json!(trace)));
    }
    assert_eq!(traces[1].1, traces[0].1, "W1 differs from W0");
}

#[tokio::test]
async fn is19_legacy_derived_id_collision_on_sqlite_is_recorded() {
    let x = source_export().await;
    let ids = source_ids(&x);
    let w0 = world(Foreign::Absent, &ids).await;
    let wl = world(Foreign::Absent, &ids).await;
    // TB's legacy row (a pre-fix import could store any id) under M(TA, f) for the second fact.
    let legacy = derived_import_fact_id(&ta(), ids[1]);
    wl.plant(&tb(), legacy, "TB legacy imported row.", false)
        .await;
    let (tb_before, tc_before) = (wl.rows(&tb()).await, wl.rows(&tc()).await);
    let normal = w0.import(&x, "append", false).await;
    let collided = wl.import(&x, "append", false).await;
    assert_eq!(normal.0, StatusCode::OK);
    assert_eq!(wl.rows(&tb()).await, tb_before, "TB's row unchanged");
    assert_eq!(wl.rows(&tc()).await, tc_before, "IS15: TC unchanged");
    assert_ne!(collided.0, StatusCode::OK, "the legacy row still collides");
    assert_ne!(
        collided, normal,
        "signal remains: LEGACY-LIMITATION (observed)"
    );
    let ta_rows = wl.rows(&ta()).await;
    eprintln!(
        "IS19 LEGACY-LIMITATION (observed, SQLite): status={} body={} ta_rows_after={}",
        collided.0.as_u16(),
        String::from_utf8_lossy(&collided.1),
        ta_rows.len()
    );
    let text = String::from_utf8_lossy(&collided.1).to_string();
    assert!(!text.contains(&legacy.to_string()), "no id in the body");
    assert!(
        !text.contains("TB legacy"),
        "no foreign content in the body"
    );
}
