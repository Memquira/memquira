mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{EventBackend, FactBackend, Settings};
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const ORG_A: &str = "org_a";
const ORG_B: &str = "org_b";
const USER_A: &str = "user_a";
const USER_B: &str = "user_b";
const MEMORY_CONTENT: &str = "Audit test memory content";

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
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

fn get_request(uri: &str, org_id: Option<&str>, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    if with_auth {
        let (name, value) = authorization_header();
        builder = builder.header(name, value);
    }

    builder.body(Body::empty()).expect("request should build")
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

async fn seed_memory_for_user(
    app: &axum::Router,
    org_id: &str,
    user_id: &str,
    content: &str,
) -> Uuid {
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
    json["memories"][0]["id"]
        .as_str()
        .expect("memory id should be present")
        .parse()
        .expect("memory id should be a valid uuid")
}

fn memory_events_uri(user_id: &str, query: &str) -> String {
    if query.is_empty() {
        format!("/api/v1/users/{user_id}/memory-events")
    } else {
        format!("/api/v1/users/{user_id}/memory-events?{query}")
    }
}

#[tokio::test]
async fn listing_events_after_adding_memory() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn response_includes_events_array() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (_, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_A), true),
    )
    .await;

    let events = json["events"]
        .as_array()
        .expect("events should be an array");
    assert!(!events.is_empty());
    assert!(events[0]["id"].is_string());
    assert_eq!(events[0]["operation"], "Add");
    assert!(events[0]["metadata"].is_object());
    assert!(events[0]["created_at"].is_string());
    assert!(json["next_cursor"].is_null());
}

#[tokio::test]
async fn response_does_not_expose_input_text() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (_, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_A), true),
    )
    .await;

    let events = json["events"]
        .as_array()
        .expect("events should be an array");
    for event in events {
        assert!(
            event.get("input_text").is_none(),
            "input_text must not appear in public API response"
        );
    }
}

#[tokio::test]
async fn filtering_by_operation_works() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app.clone(),
        get_request(
            &memory_events_uri(USER_A, "operation=Add"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
    assert_eq!(json["events"][0]["operation"], "Add");

    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, "operation=Delete"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn filtering_by_fact_id_works() {
    let app = test_app();
    let fact_id = seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app.clone(),
        get_request(
            &memory_events_uri(USER_A, &format!("fact_id={fact_id}")),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["events"].as_array().unwrap().len(), 1);
    assert_eq!(json["events"][0]["fact_id"], fact_id.to_string());

    let other_id = Uuid::new_v4();
    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, &format!("fact_id={other_id}")),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_operation_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &memory_events_uri(USER_A, "operation=NotValid"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid operation");
}

#[tokio::test]
async fn invalid_fact_id_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &memory_events_uri(USER_A, "fact_id=not-a-uuid"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid fact_id");
}

#[tokio::test]
async fn limit_defaults_correctly() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, _) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn limit_above_max_returns_validation_error() {
    let (status, json) = response_parts(
        test_app(),
        get_request(&memory_events_uri(USER_A, "limit=200"), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "limit cannot exceed 100");
}

#[tokio::test]
async fn invalid_cursor_returns_validation_error() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, "cursor=opaque-token"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid cursor");
}

#[tokio::test]
async fn route_requires_authorization_header() {
    let (status, json) = response_parts(
        test_app(),
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_A), false),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn route_requires_organization_header() {
    let (status, json) = response_parts(
        test_app(),
        get_request(&memory_events_uri(USER_A, ""), None, true),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn user_a_cannot_list_user_b_events() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_B, ""), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn org_a_cannot_list_org_b_events() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_B), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["events"].as_array().unwrap().is_empty());
}

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

#[tokio::test]
async fn sqlite_backend_returns_persisted_audit_events_via_api() {
    let fixture = SqliteFileFixture::new();
    let state = AppState::initialize(fixture.settings())
        .await
        .expect("sqlite app state should initialize");
    let app = create_app(state);

    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, ""), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
    assert_eq!(json["events"][0]["operation"], "Add");
}

#[tokio::test]
async fn user_memory_events_accepts_created_after() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, "created_after=2020-01-01T00:00:00Z"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn user_memory_events_accepts_created_before() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, "created_before=2099-01-01T00:00:00Z"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn user_memory_events_accepts_both_date_filters() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(
                USER_A,
                "created_after=2020-01-01T00:00:00Z&created_before=2099-01-01T00:00:00Z",
            ),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn user_memory_events_invalid_created_after_returns_validation_error() {
    let app = test_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, "created_after=not-a-timestamp"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "invalid created_after timestamp");
}

#[tokio::test]
async fn user_memory_events_invalid_created_before_returns_validation_error() {
    let app = test_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(USER_A, "created_before=bad"),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid created_before timestamp");
}

#[tokio::test]
async fn user_memory_events_invalid_date_range_returns_validation_error() {
    let app = test_app();
    let (status, json) = response_parts(
        app,
        get_request(
            &memory_events_uri(
                USER_A,
                "created_after=2026-06-01T00:00:00Z&created_before=2026-01-01T00:00:00Z",
            ),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json["error"]["message"],
        "created_after must be earlier than created_before"
    );
}

#[tokio::test]
async fn keyword_search_finds_matching_event_content() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, "Audit test memory content with Rust").await;

    let (status, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, "q=rust"), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
    for event in json["events"].as_array().unwrap() {
        assert!(event.get("input_text").is_none());
    }
}

#[tokio::test]
async fn empty_q_behaves_like_no_search() {
    let app = test_app();
    seed_memory_for_user(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let (status, json) = response_parts(
        app,
        get_request(&memory_events_uri(USER_A, "q=%20"), Some(ORG_A), true),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!json["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn long_q_returns_validation_error() {
    let long = "a".repeat(201);
    let (status, json) = response_parts(
        test_app(),
        get_request(
            &memory_events_uri(USER_A, &format!("q={long}")),
            Some(ORG_A),
            true,
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "q must be 200 characters or less");
}
