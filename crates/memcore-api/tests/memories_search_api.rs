mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

use common::authorization_header;

const ORG_ID: &str = "org_123";
const USER_ID: &str = "user_123";
const MEMORY_CONTENT: &str = "I am learning Rust and building a memory engine.";

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
async fn search_memory_succeeds_after_adding_memory() {
    let app = test_app();
    seed_memory(&app).await;

    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (status, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
}

#[tokio::test]
async fn search_memory_response_includes_results() {
    let app = test_app();
    seed_memory(&app).await;

    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    let results = json["results"]
        .as_array()
        .expect("results should be an array");
    assert!(!results.is_empty());
    assert!(results[0]["fact_id"].is_string());
    assert_eq!(results[0]["content"], MEMORY_CONTENT);
    assert_eq!(results[0]["memory_type"], "Conversation");
    assert!(results[0]["score"].is_number());
    assert!(results[0]["score"].as_f64().unwrap() > 0.0);
    assert!(results[0]["metadata"].is_object());
}

#[tokio::test]
async fn search_results_are_sorted_by_score_descending() {
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

    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "integration memory",
          "limit": 10
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    let results = json["results"].as_array().expect("results");
    assert!(results.len() >= 2);
    for window in results.windows(2) {
        let left = window[0]["score"].as_f64().expect("score");
        let right = window[1]["score"].as_f64().expect("score");
        assert!(left >= right);
    }
}

#[tokio::test]
async fn missing_organization_header_returns_error() {
    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Rust"
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories/search", &search_body, None),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "missing X-Organization-ID header");
}

#[tokio::test]
async fn empty_user_id_returns_validation_error() {
    let search_body = r#"{
      "user_id": "",
      "query": "Rust"
    }"#;

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories/search", search_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "user_id cannot be empty");
}

#[tokio::test]
async fn empty_query_returns_validation_error() {
    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": ""
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "query cannot be empty");
}

#[tokio::test]
async fn invalid_memory_type_filter_returns_validation_error() {
    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Rust",
          "filters": {{
            "memory_type": ["NotARealType"]
          }}
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(
        json["error"]["message"],
        "invalid memory type: NotARealType"
    );
}

#[tokio::test]
async fn limit_defaults_to_ten() {
    let app = test_app();
    seed_memory(&app).await;

    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (status, _) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn limit_above_max_returns_validation_error() {
    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "Rust",
          "limit": 100
        }}"#
    );

    let (status, json) = response_parts(
        test_app(),
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(json["error"]["message"], "limit cannot exceed 50");
}

#[tokio::test]
async fn search_finds_keyword_phrase_match() {
    let app = test_app();
    let unique_phrase = "UNIQUE_KEYWORD_PHRASE_api_rrf_xyz";
    let add_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "messages": [{{ "role": "user", "content": "Memory containing {unique_phrase} for fused search" }}],
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

    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{unique_phrase}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    let results = json["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains(unique_phrase)
    );
}

#[tokio::test]
async fn search_does_not_duplicate_same_fact() {
    let app = test_app();
    seed_memory(&app).await;

    let search_body = format!(
        r#"{{
          "user_id": "{USER_ID}",
          "query": "{MEMORY_CONTENT}"
        }}"#
    );

    let (_, json) = response_parts(
        app,
        post_request("/api/v1/memories/search", &search_body, Some(ORG_ID)),
    )
    .await;

    let results = json["results"].as_array().expect("results");
    assert!(!results.is_empty());
    let fact_ids: Vec<_> = results
        .iter()
        .map(|result| result["fact_id"].as_str().unwrap())
        .collect();
    let unique: std::collections::HashSet<_> = fact_ids.iter().collect();
    assert_eq!(unique.len(), fact_ids.len());
}
