mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{EventBackend, FactBackend, Settings};
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

struct SqliteFileFixture {
    _temp_dir: tempfile::TempDir,
    database_url: String,
}

impl SqliteFileFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("tempdir should be created");
        let db_path = temp_dir.path().join("memcore.db");
        std::fs::File::create(&db_path).expect("sqlite database file should be created");
        let database_url = format!("sqlite:{}", db_path.to_string_lossy().replace('\\', "/"));
        Self {
            _temp_dir: temp_dir,
            database_url,
        }
    }

    fn settings(&self) -> Settings {
        Settings {
            fact_backend: FactBackend::Sqlite,
            event_backend: EventBackend::Sqlite,
            database_url: self.database_url.clone(),
            ..Settings::default()
        }
    }
}

const ORG_A: &str = "org_sqlite_a";
const ORG_B: &str = "org_sqlite_b";
const USER_A: &str = "user_sqlite_a";
const USER_B: &str = "user_sqlite_b";
const MEMORY_A: &str = "First sqlite integration memory alpha bravo charlie delta";
const MEMORY_B: &str = "Second distinct sqlite integration memory foxtrot golf hotel india";

async fn sqlite_app() -> axum::Router {
    let state = AppState::initialize(Settings::sqlite_memory())
        .await
        .expect("sqlite app state should initialize");
    create_app(state)
}

fn post_request(uri: &str, body: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::from(body.to_string()))
        .expect("request should build")
}

fn delete_request(uri: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::empty())
        .expect("request should build")
}

fn get_request(uri: &str, org_id: &str) -> Request<Body> {
    let (auth_name, auth_value) = authorization_header();
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("X-Organization-ID", org_id)
        .header(auth_name, auth_value)
        .body(Body::empty())
        .expect("request should build")
}

async fn response_parts(
    app: axum::Router,
    request: Request<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.expect("router should respond");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    (status, json)
}

async fn seed_memory(app: &axum::Router, org_id: &str, user_id: &str, content: &str) {
    let add_body = format!(
        r#"{{
          "user_id": "{user_id}",
          "messages": [{{ "role": "user", "content": "{content}" }}],
          "metadata": {{}}
        }}"#
    );

    let (status, json) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", &add_body, org_id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "add failed for {content:?}: {json}");
    assert_eq!(
        json["summary"]["added"], 1,
        "expected add for {content:?}: {json}"
    );
}

async fn seed_two_memories(app: &axum::Router, org_id: &str, user_id: &str) {
    seed_memory(app, org_id, user_id, MEMORY_A).await;
    seed_memory(app, org_id, user_id, MEMORY_B).await;
}

#[tokio::test]
async fn app_starts_with_sqlite_fact_store() {
    let state = AppState::initialize(Settings::sqlite_memory())
        .await
        .expect("sqlite initialization should succeed");
    assert!(matches!(
        state.settings.fact_backend,
        memcore_config::FactBackend::Sqlite
    ));
}

#[tokio::test]
async fn post_memories_inserts_sqlite_backed_facts() {
    let app = sqlite_app().await;
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
    assert_eq!(json["memories"][0]["content"], MEMORY_A);
}

#[tokio::test]
async fn list_memories_reads_sqlite_backed_facts() {
    let app = sqlite_app().await;
    seed_two_memories(&app, ORG_A, USER_A).await;

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn delete_single_memory_soft_deletes_sqlite_fact() {
    let app = sqlite_app().await;
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;

    let (status, list_json) = response_parts(
        app.clone(),
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let memory_id = list_json["memories"][0]["id"].as_str().expect("memory id");
    let memory_id = Uuid::parse_str(memory_id).expect("valid uuid");

    let (status, delete_json) = response_parts(
        app.clone(),
        delete_request(
            &format!("/api/v1/users/{USER_A}/memories/{memory_id}"),
            ORG_A,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(delete_json["deleted"], true);

    let (status, list_json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(list_json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn forget_user_deletes_sqlite_facts_for_user() {
    let app = sqlite_app().await;
    seed_two_memories(&app, ORG_A, USER_A).await;

    let (status, _) = response_parts(
        app.clone(),
        delete_request(&format!("/api/v1/users/{USER_A}"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn sqlite_tenant_isolation_between_users() {
    let app = sqlite_app().await;
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;
    seed_memory(&app, ORG_A, USER_B, MEMORY_B).await;

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_B}/memories"), ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
    assert_eq!(json["memories"][0]["content"], MEMORY_B);
}

#[tokio::test]
async fn sqlite_tenant_isolation_between_orgs() {
    let app = sqlite_app().await;
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;
    seed_memory(&app, ORG_B, USER_A, MEMORY_B).await;

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_A}/memories"), ORG_B),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
    assert_eq!(json["memories"][0]["content"], MEMORY_B);
}

#[tokio::test]
async fn forgetting_sqlite_user_does_not_delete_other_user_in_same_org() {
    let app = sqlite_app().await;
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;
    seed_memory(&app, ORG_A, USER_B, MEMORY_B).await;

    let (status, _) = response_parts(
        app.clone(),
        delete_request(&format!("/api/v1/users/{USER_A}"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = response_parts(
        app,
        get_request(&format!("/api/v1/users/{USER_B}/memories"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn add_memory_records_audit_event_in_sqlite() {
    use memcore_core::{MemoryEventOperation, TenantContext};
    use memcore_storage::traits::MemoryEventStore;
    use memcore_storage::{MemoryEventQuery, SqliteMemoryEventStore};

    let fixture = SqliteFileFixture::new();
    let state = AppState::initialize(fixture.settings())
        .await
        .expect("sqlite app state should initialize");
    let app = create_app(state);
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;

    let event_store = SqliteMemoryEventStore::connect(&fixture.database_url)
        .await
        .expect("sqlite event store should connect");
    let tenant = TenantContext::new(ORG_A, USER_A).expect("tenant should be valid");
    let mut query = MemoryEventQuery::new(tenant, 10);
    query.operation = Some(MemoryEventOperation::Add);

    let events = event_store
        .list_events(query)
        .await
        .expect("list events should succeed");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operation, MemoryEventOperation::Add);
    assert_eq!(events[0].new_content.as_deref(), Some(MEMORY_A));
    assert!(events[0].input_text.is_none());
}

#[tokio::test]
async fn forget_user_records_audit_event_in_sqlite() {
    use memcore_core::{MemoryEventOperation, TenantContext};
    use memcore_storage::traits::MemoryEventStore;
    use memcore_storage::{MemoryEventQuery, SqliteMemoryEventStore};

    let fixture = SqliteFileFixture::new();
    let state = AppState::initialize(fixture.settings())
        .await
        .expect("sqlite app state should initialize");
    let app = create_app(state);
    seed_memory(&app, ORG_A, USER_A, MEMORY_A).await;

    let (status, _) = response_parts(
        app,
        delete_request(&format!("/api/v1/users/{USER_A}"), ORG_A),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event_store = SqliteMemoryEventStore::connect(&fixture.database_url)
        .await
        .expect("sqlite event store should connect");
    let tenant = TenantContext::new(ORG_A, USER_A).expect("tenant should be valid");
    let events = event_store
        .list_events(MemoryEventQuery::new(tenant, 10))
        .await
        .expect("list events should succeed");

    assert!(
        events
            .iter()
            .any(|event| event.operation == MemoryEventOperation::ForgetUser)
    );
}
