mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app, create_mock_memory_engine};
use memcore_config::Settings;
use memcore_core::{ContextCacheConfig, EMPTY_CONTEXT_MESSAGE, InMemoryContextCache};
use std::sync::Arc;
use tower::ServiceExt;

use common::authorization_header;

const ORG_ID: &str = "org_123";
const USER_ID: &str = "user_123";
const MEMORY_CONTENT: &str = "I am learning Rust and building a memory engine.";

fn test_app_with_cache() -> axum::Router {
    let settings = Settings::default();
    let engine = Arc::new(
        create_mock_memory_engine(&settings)
            .expect("mock engine")
            .with_context_cache(
                Arc::new(InMemoryContextCache::new(100)),
                ContextCacheConfig {
                    enabled: true,
                    ttl_seconds: 300,
                    max_entries: 100,
                    ..Default::default()
                },
            ),
    );
    create_app(AppState::with_memory_engine(settings, engine))
}

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

fn post_request(uri: &str, body: &str, org_id: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(org_id) = org_id {
        builder = builder.header("X-Organization-ID", org_id);
    }

    let (name, value) = authorization_header();
    builder = builder.header(name, value);

    builder
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

async fn seed_memory(app: &axum::Router) {
    let add_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [
            {{
              "role": "user",
              "content": "{MEMORY_CONTENT}"
            }}
          ],
          "metadata": {{}}
        }}"#
    );

    let (status, _) = response_parts(
        app.clone(),
        post_request("/api/v1/memories", &add_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn build_context_succeeds_after_adding_memory() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert!(json["context"].as_str().unwrap().contains(MEMORY_CONTENT));
}

#[tokio::test]
async fn build_context_response_includes_formatted_context_string() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let context = json["context"]
        .as_str()
        .expect("context should be a string");
    assert!(context.starts_with("Relevant long-term memories:"));
    assert!(context.contains(&format!("- {MEMORY_CONTENT}")));
}

#[tokio::test]
async fn build_context_response_includes_memories_array() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let memories = json["memories"]
        .as_array()
        .expect("memories should be an array");
    assert!(!memories.is_empty());
    assert_eq!(memories[0]["content"], MEMORY_CONTENT);
    assert!(memories[0]["fact_id"].is_string());
    assert!(memories[0]["score"].is_number());
}

#[tokio::test]
async fn missing_organization_header_returns_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Rust"
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, None),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn empty_user_id_returns_validation_error() {
    let context_body = r#"{
      "user_id": "",
      "query": "Rust"
    }"#;

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "user_id cannot be empty");
}

#[tokio::test]
async fn empty_query_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": ""
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "query cannot be empty");
}

#[tokio::test]
async fn invalid_memory_type_filter_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Rust",
          "filters": {{
            "memory_type": ["InvalidType"]
          }}
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "invalid memory type: InvalidType");
}

#[tokio::test]
async fn max_memories_defaults_to_ten() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (status, _) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn max_memories_above_max_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Rust",
          "max_memories": 25
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["message"], "max_memories cannot exceed 20");
}

#[tokio::test]
async fn no_memories_found_returns_empty_context_message() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "unrelated topic with no stored memories"
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["context"], EMPTY_CONTEXT_MESSAGE);
    assert_eq!(json["memories"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn context_lists_higher_ranked_memory_before_lower_ranked() {
    let app = test_app();
    let first_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "First sqlite integration memory alpha bravo charlie delta" }}],
          "metadata": {{}}
        }}"#
    );
    let second_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Second distinct sqlite integration memory foxtrot golf hotel india" }}],
          "metadata": {{}}
        }}"#
    );

    assert_eq!(
        response_parts(
            app.clone(),
            post_request("/api/v1/memories", &first_body, Some(ORG_ID)),
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        response_parts(
            app.clone(),
            post_request("/api/v1/memories", &second_body, Some(ORG_ID)),
        )
        .await
        .0,
        StatusCode::OK
    );

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "integration memory",
          "max_memories": 10
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let context = json["context"].as_str().expect("context");
    let memories = json["memories"].as_array().expect("memories");
    assert!(memories.len() >= 2);

    let first_content = memories[0]["content"].as_str().unwrap();
    let first_pos = context.find(first_content).expect("first in context");
    let second_content = memories[1]["content"].as_str().unwrap();
    let second_pos = context.find(second_content).expect("second in context");
    assert!(first_pos < second_pos);
}

#[tokio::test]
async fn build_context_works_without_budget_fields() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["context"].as_str().unwrap().contains(MEMORY_CONTENT));
    assert_eq!(json["budget"]["max_tokens"], 2000);
    assert_eq!(json["budget"]["reserved_tokens"], 300);
}

#[tokio::test]
async fn build_context_accepts_max_tokens_and_reserved_tokens() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}",
          "max_tokens": 1200,
          "reserved_tokens": 200
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["budget"]["max_tokens"], 1200);
    assert_eq!(json["budget"]["reserved_tokens"], 200);
    assert_eq!(json["budget"]["available_tokens"], 1000);
}

#[tokio::test]
async fn invalid_budget_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "test query",
          "max_tokens": 500,
          "reserved_tokens": 500
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn build_context_response_includes_budget_metadata() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let budget = &json["budget"];
    assert!(budget["used_tokens"].is_number());
    assert!(budget["included_memories"].is_number());
    assert!(budget["skipped_memories"].is_number());
    assert!(budget["available_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn context_output_respects_token_budget() {
    let app = test_app();
    let long_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "{}" }}],
          "metadata": {{}}
        }}"#,
        "x".repeat(2000)
    );
    let short_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "budget tiny memory alpha bravo" }}],
          "metadata": {{}}
        }}"#
    );

    assert_eq!(
        response_parts(
            app.clone(),
            post_request("/api/v1/memories", &long_body, Some(ORG_ID)),
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        response_parts(
            app.clone(),
            post_request("/api/v1/memories", &short_body, Some(ORG_ID)),
        )
        .await
        .0,
        StatusCode::OK
    );

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "budget tiny memory",
          "max_tokens": 120,
          "reserved_tokens": 20
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(json["budget"]["included_memories"], 1);
    assert_eq!(json["budget"]["skipped_memories"], 1);
    assert_eq!(json["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn format_markdown_with_sections_groups_output() {
    let app = test_app();
    let profile_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Profile context format user is a full-stack developer" }}],
          "metadata": {{}}
        }}"#
    );
    let project_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Project context format user is building memcore engine" }}],
          "metadata": {{}}
        }}"#
    );

    for body in [profile_body, project_body] {
        assert_eq!(
            response_parts(
                app.clone(),
                post_request("/api/v1/memories", &body, Some(ORG_ID)),
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "context format",
          "format": "markdown",
          "section_by_memory_type": true
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let context = json["context"].as_str().unwrap();
    assert!(context.contains("## "));
    assert!(!json["memories"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn format_plain_text_works() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}",
          "format": "plain_text"
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json["context"].as_str().unwrap().contains(MEMORY_CONTENT));
}

#[tokio::test]
async fn format_json_works() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}",
          "format": "json"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let context: serde_json::Value =
        serde_json::from_str(json["context"].as_str().unwrap()).expect("json context");
    assert!(context["memories"].is_array());
}

#[tokio::test]
async fn invalid_format_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "test",
          "format": "yaml"
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn section_by_memory_type_false_preserves_flat_ranking() {
    let app = test_app();
    let first_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Flat format first ranked memory alpha" }}],
          "metadata": {{}}
        }}"#
    );
    let second_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Flat format second ranked memory beta" }}],
          "metadata": {{}}
        }}"#
    );

    for body in [first_body, second_body] {
        assert_eq!(
            response_parts(
                app.clone(),
                post_request("/api/v1/memories", &body, Some(ORG_ID)),
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Flat format ranked",
          "format": "markdown",
          "section_by_memory_type": false
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let context = json["context"].as_str().unwrap();
    assert!(!context.contains("## "));
    let first = context.find("Flat format first").unwrap_or(0);
    let second = context.find("Flat format second").unwrap_or(usize::MAX);
    if first > 0 && second < usize::MAX {
        assert!(first < second);
    }
}

#[tokio::test]
async fn metadata_flags_affect_context_output() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}",
          "format": "markdown",
          "include_scores": true,
          "include_memory_types": true
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    let context = json["context"].as_str().unwrap();
    assert!(context.contains("score=") || context.contains("Conversation"));
}

#[tokio::test]
async fn default_context_request_has_no_compression_field() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert!(json.get("compression").is_none());
}

#[tokio::test]
async fn simple_extractive_compression_returns_metadata() {
    let app = test_app();
    for index in 0..6 {
        let body = format!(
            r#"{{
              "user_id": "{USER_ID}",
              "messages": [{{ "role": "user", "content": "compression api memory item {index} extra words" }}],
              "metadata": {{}}
            }}"#
        );
        assert_eq!(
            response_parts(
                app.clone(),
                post_request("/api/v1/memories", &body, Some(ORG_ID)),
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "compression api memory",
          "max_tokens": 60,
          "reserved_tokens": 10,
          "compression_mode": "simple_extractive",
          "summary_max_tokens": 35,
          "include_summary_section": true
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["compression"]["enabled"], true);
    assert_eq!(json["compression"]["mode"], "simple_extractive");
    assert!(json["compression"]["summarized_memories"].as_u64().unwrap() > 0);
    assert!(
        json["context"]
            .as_str()
            .unwrap()
            .contains("Compressed Memory Summary")
    );
    assert!(
        json["budget"]["used_tokens"].as_u64().unwrap()
            <= json["budget"]["available_tokens"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn invalid_compression_mode_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "test",
          "compression_mode": "invalid_mode"
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn invalid_summary_max_tokens_returns_validation_error() {
    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "test",
          "compression_mode": "simple_extractive",
          "summary_max_tokens": 0
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn compression_mode_disabled_works() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}",
          "compression_mode": "disabled"
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(json.get("compression").is_none());
}

#[tokio::test]
async fn default_context_request_has_no_cache_field() {
    let app = test_app();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;

    assert!(json.get("cache").is_none());
}

#[tokio::test]
async fn repeated_context_request_returns_cache_hit_when_enabled() {
    let app = test_app_with_cache();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, first) = response_parts(
        app.clone(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;
    assert_eq!(first["cache"]["enabled"], true);
    assert_eq!(first["cache"]["hit"], false);
    assert_eq!(first["cache"]["stampede_protection_enabled"], true);
    assert!(
        !first["cache"]["waited_for_inflight"]
            .as_bool()
            .unwrap_or(false)
    );

    let (_, second) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;
    assert_eq!(second["cache"]["hit"], true);
    assert!(
        !second["cache"]["waited_for_inflight"]
            .as_bool()
            .unwrap_or(false)
    );
}

#[tokio::test]
async fn memory_add_invalidates_context_cache() {
    let app = test_app_with_cache();
    seed_memory(&app).await;

    let context_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let _ = response_parts(
        app.clone(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;
    let (_, hit) = response_parts(
        app.clone(),
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;
    assert_eq!(hit["cache"]["hit"], true);

    let add_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Another cache invalidation memory." }}],
          "metadata": {{}}
        }}"#
    );
    assert_eq!(
        response_parts(
            app.clone(),
            post_request("/api/v1/memories", &add_body, Some(ORG_ID)),
        )
        .await
        .0,
        StatusCode::OK
    );

    let (_, miss) = response_parts(
        app,
        post_request("/api/v1/context", &context_body, Some(ORG_ID)),
    )
    .await;
    assert_eq!(miss["cache"]["hit"], false);
}
