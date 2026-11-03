use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use tower::ServiceExt;

fn test_app() -> axum::Router {
    create_app(AppState::new(Settings::default()))
}

async fn get_json(uri: &str) -> (StatusCode, serde_json::Value) {
    let response = test_app()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("response should be valid json");
    (status, json)
}

#[tokio::test]
async fn version_endpoint_is_public() {
    let (status, json) = get_json("/api/v1/version").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "success");
    assert_eq!(
        json["version"]["package_version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test]
async fn version_endpoint_returns_build_metadata_fields() {
    let (status, json) = get_json("/api/v1/version").await;
    assert_eq!(status, StatusCode::OK);

    let version = &json["version"];
    assert!(version["package_version"].is_string());
    assert!(version["git_sha"].is_string());
    assert!(version["build_timestamp"].is_string());
    assert!(version["rustc_version"].is_string());
    assert!(version["profile"].is_string());

    // Without release env injection, git/timestamp default to unknown.
    assert_eq!(version["git_sha"], "unknown");
    assert_eq!(version["build_timestamp"], "unknown");
}

#[tokio::test]
async fn version_endpoint_does_not_expose_secrets_or_env_dump() {
    let (status, json) = get_json("/api/v1/version").await;
    assert_eq!(status, StatusCode::OK);

    let body = json.to_string().to_lowercase();
    for forbidden in [
        "openai_api_key",
        "database_url",
        "postgres_url",
        "redis_url",
        "api_key_pepper",
        "authorization",
        "bearer ",
        "memcore_dev_api_key",
        "password",
    ] {
        assert!(
            !body.contains(forbidden),
            "version response must not contain {forbidden}"
        );
    }

    // Only the documented shape — no env map / settings dump.
    let version = json["version"].as_object().expect("version object");
    let mut keys: Vec<&str> = version.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "build_timestamp",
            "git_sha",
            "package_version",
            "profile",
            "rustc_version",
        ]
    );
}
