//! Qdrant vector backend API integration tests (skipped without MEMCORE_TEST_QDRANT_URL).

#[path = "../../memcore-storage/tests/support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::{Settings, VectorBackend};
use memcore_core::EMPTY_CONTEXT_MESSAGE;
use tower::ServiceExt;
use uuid::Uuid;

use common::authorization_header;

const ORG_A: &str = "org_qdrant_a";
const USER_A: &str = "user_qdrant_a";
const MEMORY_CONTENT: &str = "Qdrant vector search content for memcore API tests";

fn qdrant_url() -> Option<String> {
    match std::env::var("MEMCORE_TEST_QDRANT_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
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

    let (status, _) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", &add_body, org_id),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn mock_vector_mode_still_works_without_qdrant_server() {
    let state = AppState::new(Settings::default());
    assert_eq!(state.settings.vector_backend, VectorBackend::Mock);
    let app = create_app(state);

    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let search_body = format!(
        r#"{{
          "user_id": "{USER_A}",
          "query": "{MEMORY_CONTENT}",
          "limit": 5
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["results"].is_array());
}

#[tokio::test]
async fn app_starts_with_qdrant_vector_store_when_server_available() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant API test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };

    let collection = format!("memcore_api_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let state = AppState::initialize(Settings::qdrant_with_collection(url, collection))
        .await
        .expect("qdrant app state should initialize");

    assert_eq!(state.settings.vector_backend, VectorBackend::Qdrant);
}

#[tokio::test]
async fn search_returns_qdrant_backed_results_after_add_when_server_available() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant API test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };

    let collection = format!("memcore_api_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let state = AppState::initialize(Settings::qdrant_with_collection(url, collection))
        .await
        .expect("qdrant app state should initialize");
    let app = create_app(state);

    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let search_body = format!(
        r#"{{
          "user_id": "{USER_A}",
          "query": "{MEMORY_CONTENT}",
          "limit": 5
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, ORG_A),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let results = json["results"]
        .as_array()
        .expect("search should return results array");
    assert!(!results.is_empty());
}

#[tokio::test]
async fn build_context_works_with_qdrant_when_server_available() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant API test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };

    let collection = format!("memcore_api_test_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let state = AppState::initialize(Settings::qdrant_with_collection(url, collection))
        .await
        .expect("qdrant app state should initialize");
    let app = create_app(state);

    seed_memory(&app, ORG_A, USER_A, MEMORY_CONTENT).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_A}",
          "query": "{MEMORY_CONTENT}",
          "limit": 5
        }}"#
    );

    let (status, json) =
        response_parts(app, post_request("/api/v1/context", &context_body, ORG_A)).await;

    assert_eq!(status, StatusCode::OK);
    let context = json["context"]
        .as_str()
        .expect("context should be a string");
    assert_ne!(context, EMPTY_CONTEXT_MESSAGE);
}

/// Startup ensures the payload indexes on the fact and (enabled) episode collections,
/// the lazily opened lifecycle episode collection gets them on first open, and
/// `MEMCORE_QDRANT_ENSURE_INDEXES=false` creates none.
#[tokio::test]
async fn startup_ensures_qdrant_payload_indexes_unless_disabled() {
    use memcore_storage::{EPISODE_PAYLOAD_INDEXES, FACT_PAYLOAD_INDEXES, QdrantVectorStore};

    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant API test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    async fn missing(url: &str, collection: &str, fields: &[&str]) -> Vec<String> {
        // An ensure on a fully indexed collection creates nothing; what it creates was missing.
        QdrantVectorStore::connect_existing(url, collection, 8)
            .await
            .expect("connect_existing")
            .expect("collection should exist")
            .ensure_payload_indexes(fields)
            .await
            .expect("ensure")
            .created
    }

    // Episodes on: both collections are opened at startup and indexed.
    let collection = format!("memcore_api_t3b_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let mut settings = Settings::qdrant_with_collection(url.clone(), collection.clone());
    settings.episodes_enabled = true;
    settings.qdrant_episode_collection = format!("{collection}_episodes");
    AppState::initialize(settings)
        .await
        .expect("qdrant app state should initialize");
    assert!(
        missing(&url, &collection, FACT_PAYLOAD_INDEXES)
            .await
            .is_empty()
    );
    assert!(
        missing(
            &url,
            &format!("{collection}_episodes"),
            EPISODE_PAYLOAD_INDEXES
        )
        .await
        .is_empty()
    );

    // Episodes off, existing earlier episode collection: indexed when the lifecycle opens it.
    let collection = format!("memcore_api_t3b_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let episodes = format!("{collection}_episodes");
    drop(
        // The mock embedding provider's dimensions (8): verifies the size of existing stores.
        QdrantVectorStore::connect(&url, &episodes, 8)
            .await
            .expect("legacy episode collection"),
    );
    let mut settings = Settings::qdrant_with_collection(url.clone(), collection.clone());
    settings.qdrant_episode_collection = episodes.clone();
    let app = create_app(
        AppState::initialize(settings)
            .await
            .expect("qdrant app state should initialize"),
    );
    let (auth_name, auth_value) = authorization_header();
    let forget = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/users/{USER_A}"))
        .header("X-Organization-ID", ORG_A)
        .header(auth_name, auth_value)
        .body(Body::empty())
        .expect("request should build");
    let (status, body) = response_parts(app, forget).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        missing(&url, &episodes, EPISODE_PAYLOAD_INDEXES)
            .await
            .is_empty()
    );

    // Escape hatch: no index is created.
    let collection = format!("memcore_api_t3b_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let mut settings = Settings::qdrant_with_collection(url.clone(), collection.clone());
    settings.qdrant_ensure_indexes = false;
    AppState::initialize(settings)
        .await
        .expect("qdrant app state should initialize");
    assert_eq!(
        missing(&url, &collection, FACT_PAYLOAD_INDEXES).await.len(),
        4
    );
}

/// With episodes off, forget-user reaches an existing episode collection whose
/// embedding identity no longer matches the configured provider (deletion only, this tenant only),
/// while an import that would write vectors still fails closed and a dry run deletes nothing.
#[tokio::test]
async fn forget_user_deletes_from_an_identity_mismatched_episode_store_but_import_fails_closed() {
    use memcore_core::EmbeddingIdentity;
    use memcore_storage::{QdrantVectorStore, VectorRecord, VectorStore};
    use qdrant_client::qdrant::{Condition, CountPointsBuilder, Filter};

    let Some(url) = qdrant_url() else {
        eprintln!("skipping qdrant API test: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collection = format!("memcore_api_t20_{}", Uuid::new_v4().simple());
    let _owned_collection = qdrant_cleanup::TestCollections::owning(&collection);
    let episodes = format!("{collection}_episodes");
    // Built by another model (recorded identity); the mock provider of this app has none.
    let old = EmbeddingIdentity::remote("openai_compatible", "old-model", 8);
    let store = QdrantVectorStore::connect_verified(&url, &episodes, 8, Some(&old), None)
        .await
        .expect("episode store built by another model");
    for user in ["user_a", "user_b"] {
        let tenant = memcore_core::TenantContext::new(ORG_A, user).unwrap();
        for i in 0..2 {
            let id = Uuid::new_v4();
            store
                .upsert_vector(
                    &tenant,
                    VectorRecord {
                        id,
                        fact_id: id,
                        org_id: ORG_A.into(),
                        user_id: user.into(),
                        embedding: vec![0.1 * (i as f32 + 1.0); 8],
                        content: format!("{user} episode {i}"),
                        memory_type: memcore_core::MemoryType::Conversation,
                        metadata: serde_json::json!({"kind": "episode"}),
                    },
                )
                .await
                .unwrap();
        }
    }
    let client = qdrant_client::Qdrant::from_url(&url)
        .skip_compatibility_check()
        .build()
        .unwrap();
    let count = |user: &'static str| {
        let (client, episodes) = (client.clone(), episodes.clone());
        async move {
            client
                .count(
                    CountPointsBuilder::new(&episodes)
                        .filter(Filter::must([Condition::matches(
                            "user_id",
                            user.to_string(),
                        )]))
                        .exact(true),
                )
                .await
                .unwrap()
                .result
                .unwrap()
                .count
        }
    };

    let mut settings = Settings::qdrant_with_collection(url.clone(), collection.clone());
    settings.qdrant_episode_collection = episodes.clone();
    let app = create_app(AppState::initialize(settings).await.expect("app starts"));
    let (auth_name, auth_value) = authorization_header();

    // Dry-run replace import for user_b: validation only, nothing deleted or re-embedded.
    let export = serde_json::json!({
        "org_id": ORG_A, "user_id": "user_b", "exported_at": "2026-10-11T00:00:00Z",
        "format_version": "memcore.user_export.v2", "facts": [], "memory_events": [],
        "episodes": [{
            "id": Uuid::new_v4(), "org_id": ORG_A, "user_id": "user_b", "role": "user",
            "content": "An invented message long enough to be embedded by the import path.",
            "recorded_at": "2026-10-10T00:00:00Z", "embedded": true, "metadata": {}
        }]
    });
    let dry = serde_json::json!({"export": export, "mode": "replace", "dry_run": true}).to_string();
    let (status, body) = response_parts(
        app.clone(),
        post_request("/api/v1/users/user_b/import", &dry, ORG_A),
    )
    .await;
    assert!(status.is_success(), "dry run: {status} {body}");
    assert_eq!(count("user_b").await, 2, "dry run deleted nothing");

    // Import that would write vectors into the mismatched store fails closed.
    let append = serde_json::json!({"export": export, "mode": "append"}).to_string();
    let (status, body) = response_parts(
        app.clone(),
        post_request("/api/v1/users/user_b/import", &append, ORG_A),
    )
    .await;
    assert!(
        !status.is_success(),
        "import must not write into a mismatched store: {body}"
    );
    assert_eq!(
        count("user_b").await,
        2,
        "no vector written by the refused import"
    );

    // Forget-user removes exactly user_a's vectors; user_b is untouched.
    let forget = Request::builder()
        .method("DELETE")
        .uri("/api/v1/users/user_a")
        .header("X-Organization-ID", ORG_A)
        .header(auth_name, auth_value)
        .body(Body::empty())
        .unwrap();
    let (status, body) = response_parts(app, forget).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(count("user_a").await, 0);
    assert_eq!(count("user_b").await, 2);
    let _ = client.delete_collection(&episodes).await;
    let _ = client
        .delete_collection(memcore_storage::identity_collection_name(&episodes))
        .await;
}
