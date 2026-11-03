use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
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

#[tokio::test]
async fn openapi_json_returns_200() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn openapi_json_is_valid_json_with_expected_paths() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("openapi.json should be valid JSON");

    assert_eq!(json["info"]["title"], "memcore API");
    assert!(json["paths"]["/api/v1/memories"].is_object());
    assert!(json["paths"]["/api/v1/api-keys"].is_object());
    assert!(json["paths"]["/api/v1/users/{user_id}/export"].is_object());
    assert!(json["paths"]["/health"].is_object());

    let global_security = json.get("security");
    assert!(
        global_security.is_none()
            || global_security
                .unwrap()
                .as_array()
                .is_some_and(|v| v.is_empty()),
        "OpenAPI should not require auth globally"
    );

    let api_key_list_schema = &json["components"]["schemas"]["ApiKeyItemResponse"];
    assert!(api_key_list_schema["properties"].get("key_hash").is_none());
    assert!(api_key_list_schema["properties"].get("raw_key").is_none());

    let memory_event_schema = &json["components"]["schemas"]["MemoryEventItemResponse"];
    assert!(
        memory_event_schema["properties"]
            .get("input_text")
            .is_none()
    );
}

#[tokio::test]
async fn docs_route_is_public() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/docs")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    assert!(
        response.status().is_success() || response.status().is_redirection(),
        "unexpected docs status: {}",
        response.status()
    );
}

#[tokio::test]
async fn openapi_json_does_not_require_auth() {
    let (status, _) = response_parts(
        test_app(),
        Request::builder()
            .uri("/openapi.json")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}
