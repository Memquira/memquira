//! The read-only entity API (`GET /api/v1/users/{user_id}/entities[/{id}]`) and
//! the entity fields of add, export and import responses.
//!
//! With `MEMCORE_ENTITIES_ENABLED` off (the default) the endpoints answer 400 and every existing
//! response keeps its shape. On, the mock model annotates each stored fact's subject; the
//! endpoints need an API key, are scoped to the organization header and path user (P3-04), and
//! paginate.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::authorization_header;

const ORG: &str = "org_entities_api";

async fn call_as(
    app: axum::Router,
    org: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
    authorized: bool,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org);
    if authorized {
        let (name, value) = authorization_header();
        request = request.header(name, value);
    }
    let request = request
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_as(app, ORG, method, uri, body, true).await
}

fn app(entities_enabled: bool) -> axum::Router {
    create_app(AppState::new(Settings {
        entities_enabled,
        rate_limit_enabled: false,
        ..Settings::default()
    }))
}

/// Two messages from two speakers; the mock extraction attributes each fact to its speaker and the
/// mock annotation names that subject.
async fn add(app: &axum::Router, user: &str) -> Value {
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v1/memories",
        Some(json!({
            "user_id": user,
            "session_id": "S1",
            "session_timestamp": "2026-03-04T10:00:00Z",
            "messages": [
                { "role": "user", "name": "Ines", "message_id": "D1:1",
                  "content": "I adopted a beagle called Biscuit last spring." },
                { "role": "user", "name": "Tomasz", "message_id": "D1:2",
                  "content": "I started learning the cello this winter." }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn disabled_entities_answer_400_and_keep_existing_responses() {
    let app = app(false);
    let added = add(&app, "user_a").await;
    assert!(added.get("entities").is_none(), "{added}");
    let (status, _) = call(app.clone(), "GET", "/api/v1/users/user_a/entities", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        app.clone(),
        "GET",
        "/api/v1/users/user_a/entities/00000000-0000-4000-8000-000000000001",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, export) = call(app, "GET", "/api/v1/users/user_a/export", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(export["export"].get("entities").is_none());
    assert_ne!(export["export"]["format_version"], "memcore.user_export.v3");
}

#[tokio::test]
async fn enabled_entities_are_listed_detailed_and_paginated() {
    let app = app(true);
    let added = add(&app, "user_a").await;
    assert_eq!(added["entities"]["annotation_requests"], 1, "{added}");
    assert_eq!(added["entities"]["created"], 2, "{added}");

    let (status, first) = call(
        app.clone(),
        "GET",
        "/api/v1/users/user_a/entities?limit=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["entities"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a second page")
        .to_string();
    let (_, second) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/users/user_a/entities?limit=1&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(second["entities"].as_array().unwrap().len(), 1);
    assert!(second["next_cursor"].is_null());
    let mut names = vec![
        first["entities"][0]["canonical_name"]
            .as_str()
            .unwrap()
            .to_string(),
        second["entities"][0]["canonical_name"]
            .as_str()
            .unwrap()
            .to_string(),
    ];
    names.sort();
    assert_eq!(names, vec!["Ines", "Tomasz"]);
    assert_eq!(first["entities"][0]["is_speaker"], true);
    assert_eq!(first["entities"][0]["entity_type"], "person");
    assert!(first["entities"][0].get("name_embedding").is_none());

    let id = first["entities"][0]["id"].as_str().unwrap();
    let (status, detail) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/users/user_a/entities/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["entity"]["id"], id);
    assert_eq!(detail["facts"].as_array().unwrap().len(), 1);
    assert_eq!(detail["facts"][0]["role"], "subject");
    assert_eq!(detail["facts"][0]["active"], true);

    let (status, _) = call(
        app.clone(),
        "GET",
        "/api/v1/users/user_a/entities/00000000-0000-4000-8000-000000000001",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        app.clone(),
        "GET",
        "/api/v1/users/user_a/entities/not-a-uuid",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(app, "GET", "/api/v1/users/user_a/entities?limit=101", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// P3-04 at the API: another user or organization sees nothing, and a key is required.
#[tokio::test]
async fn entities_are_scoped_to_the_organization_and_user() {
    let app = app(true);
    add(&app, "user_a").await;
    let (_, list) = call(app.clone(), "GET", "/api/v1/users/user_a/entities", None).await;
    let id = list["entities"][0]["id"].as_str().unwrap().to_string();

    let (status, other_user) =
        call(app.clone(), "GET", "/api/v1/users/user_b/entities", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(other_user["entities"].as_array().unwrap().is_empty());
    let (status, _) = call(
        app.clone(),
        "GET",
        &format!("/api/v1/users/user_b/entities/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, other_org) = call_as(
        app.clone(),
        "org_other",
        "GET",
        "/api/v1/users/user_a/entities",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(other_org["entities"].as_array().unwrap().is_empty());
    let (status, _) = call_as(
        app.clone(),
        "org_other",
        "GET",
        &format!("/api/v1/users/user_a/entities/{id}"),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call_as(
        app,
        ORG,
        "GET",
        "/api/v1/users/user_a/entities",
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Export is v3 with the entity layer; importing it into another user brings the entities along.
#[tokio::test]
async fn export_carries_entities_and_import_restores_them() {
    let app = app(true);
    add(&app, "user_a").await;
    let (status, export) = call(app.clone(), "GET", "/api/v1/users/user_a/export", None).await;
    assert_eq!(status, StatusCode::OK, "{export}");
    assert_eq!(export["export"]["format_version"], "memcore.user_export.v3");
    let block = &export["export"]["entities"];
    assert_eq!(block["entities"].as_array().unwrap().len(), 2);
    assert_eq!(block["links"].as_array().unwrap().len(), 2);
    // Embeddings travel with their model and dimension.
    let first = &block["entities"][0];
    assert_eq!(first["name_embedding_model"], "mock/mock-embedding");
    assert_eq!(
        first["name_embedding_dimensions"].as_u64().unwrap() as usize,
        first["name_embedding"].as_array().unwrap().len()
    );

    let (status, imported) = call(
        app.clone(),
        "POST",
        "/api/v1/users/user_a/import",
        Some(json!({ "export": export["export"], "mode": "replace" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["summary"]["imported_entities"], 2);
    let (_, list) = call(app, "GET", "/api/v1/users/user_a/entities", None).await;
    assert_eq!(list["entities"].as_array().unwrap().len(), 2);
}

/// Entity data written with entities on is deleted by
/// forget-user after a restart with entities off, and exported in the meantime.
#[tokio::test]
async fn sqlite_forget_user_reaches_entities_after_they_are_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memcore.db");
    std::fs::File::create(&db).unwrap();
    let settings = |entities_enabled| Settings {
        fact_backend: memcore_config::FactBackend::Sqlite,
        event_backend: memcore_config::EventBackend::Sqlite,
        database_url: format!("sqlite:{}", db.to_string_lossy().replace('\\', "/")),
        entities_enabled,
        rate_limit_enabled: false,
        ..Settings::default()
    };
    let on = create_app(AppState::initialize(settings(true)).await.unwrap());
    add(&on, "user_a").await;
    add(&on, "user_b").await;
    drop(on);

    let off = create_app(AppState::initialize(settings(false)).await.unwrap());
    let (status, export) = call(off.clone(), "GET", "/api/v1/users/user_a/export", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(export["export"]["format_version"], "memcore.user_export.v3");
    let (status, _) = call(off.clone(), "DELETE", "/api/v1/users/user_a", None).await;
    assert_eq!(status, StatusCode::OK);

    let pool = sqlx_count(&db).await;
    assert_eq!(pool.0, 0, "user_a entity rows");
    assert_eq!(pool.1, 0, "user_a links");
    assert!(pool.2 > 0, "user_b untouched");
}

/// (user_a entities, user_a links, user_b entities)
async fn sqlx_count(db: &std::path::Path) -> (usize, usize, usize) {
    use memcore_core::ports::EntityStore;
    let store = memcore_storage::SqliteEntityStore::connect(&format!(
        "sqlite:{}",
        db.to_string_lossy().replace('\\', "/")
    ))
    .await
    .unwrap();
    let a = memcore_core::TenantContext::new(ORG, "user_a").unwrap();
    let b = memcore_core::TenantContext::new(ORG, "user_b").unwrap();
    let entities_a = store.list_all_entities(&a).await.unwrap();
    let mut links_a = 0;
    for entity in &entities_a {
        links_a += store.links_for_entity(&a, entity.id).await.unwrap().len();
    }
    (
        entities_a.len(),
        links_a,
        store.list_all_entities(&b).await.unwrap().len(),
    )
}
