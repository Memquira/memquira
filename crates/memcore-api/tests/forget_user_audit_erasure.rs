//! `DELETE /api/v1/users/{id}` also erases the user's content-bearing audit
//! events (exact org + user), keeps only content-free tombstones, never touches other tenants, and
//! the user's export no longer contains the erased text. SQLite always; PostgreSQL with
//! MEMCORE_TEST_POSTGRES_URL. Invented data only.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{EventBackend, FactBackend, Settings};
use memcore_core::{
    MemoryEvent, MemoryEventOperation, MemoryEventQuery, MemoryEventStore, TenantContext,
};
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const SECRET: &str = "PRIVATE-AUDIT-SENTINEL";

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    org: &str,
    body: Option<String>,
) -> (StatusCode, String) {
    let (auth_name, auth_value) = authorization_header();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org)
        .header(auth_name, auth_value)
        .body(body.map(Body::from).unwrap_or_else(Body::empty))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn content_of(org: &str, user: &str, i: usize) -> String {
    format!("{SECRET} {org} {user} likes invented hobby number {i}")
}

struct Counts {
    content_events: usize,
    all_events: usize,
    text_present: bool,
}

async fn counts(events: &Arc<dyn MemoryEventStore>, org: &str, user: &str) -> Counts {
    let tenant = TenantContext::new(org, user).unwrap();
    let all = events
        .list_events(MemoryEventQuery::new(tenant.clone(), 1000))
        .await
        .unwrap();
    let text_present = all.iter().any(|e| {
        [
            &e.input_text,
            &e.previous_content,
            &e.new_content,
            &e.provider_name,
            &e.model_name,
        ]
        .iter()
        .any(|c| c.as_deref().is_some_and(|c| c.contains(SECRET)))
            || e.metadata.to_string().contains(SECRET)
    });
    Counts {
        content_events: events.count_user_audit_content(&tenant).await.unwrap(),
        all_events: all.len(),
        text_present,
    }
}

/// Pre-freeze check: `forget_user` events that LOOK like tombstones but carry text in a
/// free-form field (metadata, provider / model name) or a fact id, as an imported archive could.
/// They must be erased. One canonical content-free tombstone (maintenance marker) is also seeded.
async fn seed_forged_tombstones(events: &Arc<dyn MemoryEventStore>, org: &str, user: &str) {
    let tenant = TenantContext::new(org, user).unwrap();
    let forged = |fact_id, provider: Option<&str>, model: Option<&str>, metadata| {
        MemoryEvent::new(
            org,
            user,
            fact_id,
            MemoryEventOperation::ForgetUser,
            None,
            None,
            provider.map(str::to_string),
            model.map(str::to_string),
            metadata,
        )
    };
    let secret = format!("{SECRET} {org} {user} tombstone");
    for event in [
        forged(
            None,
            None,
            None,
            serde_json::json!({"deleted": true, "note": secret}),
        ),
        forged(None, None, None, serde_json::json!({"fact": secret})),
        forged(
            None,
            Some(&secret),
            None,
            serde_json::json!({"deleted": true}),
        ),
        forged(
            None,
            None,
            Some(&secret),
            serde_json::json!({"deleted": true}),
        ),
        forged(
            Some(Uuid::new_v4()),
            None,
            None,
            serde_json::json!({"deleted": true}),
        ),
        forged(
            None,
            Some("maintenance"),
            None,
            serde_json::json!({"deleted": true}),
        ),
    ] {
        events.record_event(&tenant, event).await.unwrap();
    }
}

async fn scenario(settings: Settings, events: Arc<dyn MemoryEventStore>) {
    let run = Uuid::new_v4().simple().to_string();
    let (org_a, org_b) = (format!("org_a_{run}"), format!("org_b_{run}"));
    let tenants = [
        (org_a.clone(), "user_1"),
        (org_a.clone(), "user_2"),
        (org_b.clone(), "user_1"),
    ];
    let app = create_app(AppState::initialize(settings).await.expect("app starts"));

    for (org, user) in &tenants {
        for i in 0..2 {
            let body = serde_json::json!({
                "user_id": user, "metadata": {},
                "messages": [{"role": "user", "content": content_of(org, user, i)}],
            });
            let (status, text) = call(
                &app,
                "POST",
                "/api/v1/memories",
                org,
                Some(body.to_string()),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{text}");
        }
    }
    for (org, user) in &tenants {
        seed_forged_tombstones(&events, org, user).await;
    }
    let before_target = counts(&events, &org_a, "user_1").await;
    assert!(
        before_target.content_events > 0 && before_target.text_present,
        "seeded audit content"
    );
    let mut before_others = Vec::new();
    for (org, user) in &tenants[1..] {
        let c = counts(&events, org, user).await;
        assert!(c.text_present);
        before_others.push((c.content_events, c.all_events));
    }

    // Forget (org_a, user_1).
    let (status, text) = call(&app, "DELETE", "/api/v1/users/user_1", &org_a, None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let after = counts(&events, &org_a, "user_1").await;
    assert_eq!(
        after.content_events, 0,
        "no content-bearing audit events remain"
    );
    assert!(!after.text_present, "no erased text in the audit table");
    let tenant = TenantContext::new(org_a.clone(), "user_1").unwrap();
    let remaining = events
        .list_events(MemoryEventQuery::new(tenant.clone(), 100))
        .await
        .unwrap();
    assert!(
        !remaining.is_empty() && remaining.iter().all(MemoryEvent::is_content_free_tombstone),
        "only content-free tombstones remain: {remaining:?}"
    );
    // The seeded canonical tombstone and the new one survive; every forged one is gone.
    assert_eq!(remaining.len(), 2, "{remaining:?}");

    // Other tenants unchanged (same org other user; same user id other org).
    for ((org, user), before) in tenants[1..].iter().zip(&before_others) {
        let c = counts(&events, org, user).await;
        assert_eq!(
            (c.content_events, c.all_events),
            *before,
            "{org}/{user} changed"
        );
        assert!(c.text_present);
    }

    // The forgotten user's export carries none of the erased text; the others' still do.
    let (status, export) = call(&app, "GET", "/api/v1/users/user_1/export", &org_a, None).await;
    assert_eq!(status, StatusCode::OK, "{export}");
    assert!(!export.contains(SECRET), "export resurfaced erased content");
    let (_, other_export) = call(&app, "GET", "/api/v1/users/user_1/export", &org_b, None).await;
    assert!(other_export.contains(SECRET));
    let (status, listed) = call(
        &app,
        "GET",
        "/api/v1/users/user_1/memory-events",
        &org_a,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!listed.contains(SECRET));

    // A second forget is safe.
    let (status, _) = call(&app, "DELETE", "/api/v1/users/user_1", &org_a, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(counts(&events, &org_a, "user_1").await.content_events, 0);
}

#[tokio::test]
async fn forget_user_erases_audit_content_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("m.db").to_string_lossy().replace('\\', "/")
    );
    let mut settings = Settings::default();
    settings.fact_backend = FactBackend::Sqlite;
    settings.event_backend = EventBackend::Sqlite;
    settings.database_url = url.clone();
    let pool = memcore_storage::connect_sqlite_pool(&url).await.unwrap();
    memcore_storage::check_sqlite_pool_startup(
        &pool,
        memcore_storage::StorageMigrationMode::Auto,
        true,
    )
    .await
    .unwrap();
    let events: Arc<dyn MemoryEventStore> =
        Arc::new(memcore_storage::SqliteMemoryEventStore::new(pool));
    scenario(settings, events).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn forget_user_erases_audit_content_on_postgres() {
    let Ok(url) = std::env::var("MEMCORE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    let mut settings = Settings::default();
    settings.fact_backend = FactBackend::Postgres;
    settings.event_backend = EventBackend::Postgres;
    settings.postgres_url = Some(url.clone());
    let store = memcore_storage::PostgresFactStore::connect(&url)
        .await
        .unwrap();
    let events: Arc<dyn MemoryEventStore> =
        Arc::new(memcore_storage::PostgresMemoryEventStore::new(store.pool()));
    scenario(settings, events).await;
}
