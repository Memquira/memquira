//! In-process Axum test application for E2E API flows.

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use memcore_api::{AppState, create_app};
use memcore_config::Settings;
use serde::Serialize;
use serde_json::Value;
use tower::ServiceExt;

use super::fixtures::{DEFAULT_ORG, DEV_API_KEY};

#[derive(Clone)]
pub struct TestApp {
    pub router: axum::Router,
    pub api_key: String,
    pub org_id: String,
    pub settings: Settings,
}

#[derive(Debug)]
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub raw: String,
}

impl TestApp {
    /// Fast mock-backed app (no SQLite/Postgres/Qdrant/Redis/OpenAI).
    /// Rate limiting stays at Settings defaults unless overridden; multi-step
    /// flows use distinct orgs or stay under the default RPM budget.
    pub fn new() -> Self {
        Self::with_settings(Settings {
            // Keep E2E deterministic: disable process rate limit unless a test enables it.
            rate_limit_enabled: false,
            ..Settings::default()
        })
    }

    pub fn with_auth() -> Self {
        Self::new()
    }

    pub fn with_rate_limit_enabled(rpm: u32) -> Self {
        Self::with_settings(Settings {
            rate_limit_enabled: true,
            rate_limit_requests_per_minute: rpm,
            ..Settings::default()
        })
    }

    pub fn with_rate_limit_disabled() -> Self {
        Self::with_settings(Settings {
            rate_limit_enabled: false,
            ..Settings::default()
        })
    }

    pub fn with_settings(settings: Settings) -> Self {
        let api_key = settings.dev_api_key.clone();
        let router = create_app(AppState::new(settings.clone()));
        Self {
            router,
            api_key,
            org_id: DEFAULT_ORG.to_string(),
            settings,
        }
    }

    /// SQLite in-memory persistence when a test needs real storage wiring.
    pub async fn with_sqlite_memory() -> Self {
        let settings = Settings::sqlite_memory();
        let api_key = settings.dev_api_key.clone();
        let state = AppState::initialize(settings.clone())
            .await
            .expect("sqlite memory app should initialize");
        Self {
            router: create_app(state),
            api_key,
            org_id: DEFAULT_ORG.to_string(),
            settings,
        }
    }

    pub fn with_org(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = org_id.into();
        self
    }

    pub async fn get(&self, path: &str) -> TestResponse {
        self.request("GET", path, None, false, None).await
    }

    pub async fn delete(&self, path: &str) -> TestResponse {
        self.request("DELETE", path, None, false, None).await
    }

    pub async fn post_json_raw(&self, path: &str, body: &str) -> TestResponse {
        self.request("POST", path, Some(body), false, Some("application/json"))
            .await
    }

    pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> TestResponse {
        let raw = serde_json::to_string(body).expect("serialize body");
        self.post_json_raw(path, &raw).await
    }

    pub async fn authed_get(&self, path: &str) -> TestResponse {
        self.request("GET", path, None, true, None).await
    }

    pub async fn authed_delete(&self, path: &str) -> TestResponse {
        self.request("DELETE", path, None, true, None).await
    }

    pub async fn authed_post_json_raw(&self, path: &str, body: &str) -> TestResponse {
        self.request("POST", path, Some(body), true, Some("application/json"))
            .await
    }

    pub async fn authed_post_json<T: Serialize>(&self, path: &str, body: &T) -> TestResponse {
        let raw = serde_json::to_string(body).expect("serialize body");
        self.authed_post_json_raw(path, &raw).await
    }

    pub async fn authed_post_raw(
        &self,
        path: &str,
        body: &str,
        content_type: Option<&str>,
    ) -> TestResponse {
        self.request("POST", path, Some(body), true, content_type)
            .await
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        authed: bool,
        content_type: Option<&str>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);

        if authed {
            builder = builder
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("X-Organization-ID", &self.org_id);
        }

        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }

        let body_bytes = body.unwrap_or("").to_string();
        if !body_bytes.is_empty() {
            builder = builder.header("content-length", body_bytes.len().to_string());
        }

        let request = builder
            .body(Body::from(body_bytes))
            .expect("request should build");

        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router should respond");

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

        TestResponse {
            status,
            headers,
            body,
            raw,
        }
    }

    /// Authenticated request with an explicit org id (tenant isolation tests).
    pub async fn authed_as(
        &self,
        method: &str,
        path: &str,
        org_id: &str,
        body: Option<&str>,
    ) -> TestResponse {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("X-Organization-ID", org_id);

        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }

        let body_bytes = body.unwrap_or("").to_string();
        if !body_bytes.is_empty() {
            builder = builder.header("content-length", body_bytes.len().to_string());
        }

        let request = builder
            .body(Body::from(body_bytes))
            .expect("request should build");

        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router should respond");

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

        TestResponse {
            status,
            headers,
            body,
            raw,
        }
    }

    pub async fn request_with_bearer(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        org_id: Option<&str>,
        body: Option<&str>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(org_id) = org_id {
            builder = builder.header("X-Organization-ID", org_id);
        }
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let body_bytes = body.unwrap_or("").to_string();
        if !body_bytes.is_empty() {
            builder = builder.header("content-length", body_bytes.len().to_string());
        }
        let request = builder
            .body(Body::from(body_bytes))
            .expect("request should build");

        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router should respond");

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

        TestResponse {
            status,
            headers,
            body,
            raw,
        }
    }
}

impl Default for TestApp {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_dev_key() -> &'static str {
    DEV_API_KEY
}
