use std::time::Duration;

use memcore_common::{MemcoreError, MemcoreResult};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::{
    ApiErrorResponse, ChatCompletionsRequest, ChatCompletionsResponse, ResponsesCreateRequest,
    ResponsesCreateResponse,
};

const DEFAULT_TIMEOUT_SECS: u64 = 90;

/// HTTP client timeout: the larger of the 90 s default and the provider timeout. Previously
/// the HTTP client always stopped at 90 s, so a longer `MEMCORE_PROVIDER_TIMEOUT_SECONDS` (slow
/// CPU inference) was silently cut short; provider timeouts up to 90 s behave exactly as before.
pub fn http_timeout(provider_timeout: Option<Duration>) -> Duration {
    let default = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    provider_timeout.map_or(default, |timeout| timeout.max(default))
}

/// The Responses API accepts `input` only as a string or an array of input items. Message lists are
/// already arrays; the structured JSON payloads (extraction, classification, consolidation, entity
/// prompts) are objects, which OpenAI rejects with HTTP 400 ("Invalid type for 'input'"). They are
/// sent as their JSON text, exactly what the chat-completions transport sends as the user message
/// (`input.to_string()`), so both transports give the model the same content.
pub(crate) fn responses_input(input: Value) -> Value {
    match input {
        Value::Array(_) | Value::String(_) => input,
        other => Value::String(other.to_string()),
    }
}

#[derive(Clone)]
pub struct OpenAiClient {
    http: Client,
    /// `None`: a server that needs no authentication (no `Authorization` header is sent).
    api_key: Option<String>,
    base_url: String,
}

impl std::fmt::Debug for OpenAiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiClient")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl OpenAiClient {
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> MemcoreResult<Self> {
        let api_key = api_key.into().trim().to_string();
        if api_key.is_empty() {
            return Err(MemcoreError::ValidationError(
                "OPENAI_API_KEY cannot be empty".to_string(),
            ));
        }
        Self::with_optional_key(Some(api_key), base_url)
    }

    /// A client whose key may be absent (key-free loopback / unauthenticated servers;
    /// the settings validation decides when that is allowed).
    pub fn with_optional_key(
        api_key: Option<String>,
        base_url: impl Into<String>,
    ) -> MemcoreResult<Self> {
        Self::with_optional_key_and_timeout(api_key, base_url, None)
    }

    /// As [`Self::with_optional_key`], with the deployment's provider timeout
    /// (`MEMCORE_PROVIDER_TIMEOUT_SECONDS`) applied to the HTTP client too: see [`http_timeout`].
    pub fn with_optional_key_and_timeout(
        api_key: Option<String>,
        base_url: impl Into<String>,
        provider_timeout: Option<Duration>,
    ) -> MemcoreResult<Self> {
        let api_key = api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty());
        let base_url = normalize_base_url(base_url.into());
        let http = Client::builder()
            .timeout(http_timeout(provider_timeout))
            .build()
            .map_err(|err| {
                MemcoreError::Internal(format!("failed to build OpenAI HTTP client: {err}"))
            })?;

        Ok(Self {
            http,
            api_key,
            base_url,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn create_response(
        &self,
        request: &ResponsesCreateRequest,
    ) -> MemcoreResult<ResponsesCreateResponse> {
        self.post_json("/responses", request).await
    }

    pub async fn create_chat_completion(
        &self,
        request: &ChatCompletionsRequest,
    ) -> MemcoreResult<ChatCompletionsResponse> {
        self.post_json("/chat/completions", request).await
    }

    pub async fn create_embeddings<T: Serialize, R: DeserializeOwned>(
        &self,
        request: &T,
    ) -> MemcoreResult<R> {
        self.post_json("/embeddings", request).await
    }

    /// OpenAI-compatible hosts (Gemini, Groq, etc.) typically expose chat completions,
    /// not the OpenAI `/responses` API used by direct `api.openai.com`.
    pub fn prefers_chat_completions(base_url: &str) -> bool {
        let normalized = base_url.trim().to_ascii_lowercase();
        !normalized.contains("api.openai.com")
    }

    /// Z.ai GLM defaults to thinking mode which fills `reasoning_content` and can leave
    /// `message.content` empty under small `max_tokens` budgets. Disable thinking for
    /// structured fact extraction / chat JSON paths.
    pub fn prefers_disabled_thinking(base_url: &str) -> bool {
        let normalized = base_url.trim().to_ascii_lowercase();
        normalized.contains("api.z.ai")
    }

    pub(crate) fn responses_request_body(
        &self,
        model: &str,
        instructions: &str,
        input: Value,
        schema_name: &str,
        schema: Value,
    ) -> ResponsesCreateRequest {
        ResponsesCreateRequest {
            model: model.to_string(),
            input: responses_input(input),
            instructions: Some(instructions.to_string()),
            text: Some(super::types::TextFormatConfig {
                format: super::types::TextFormat::JsonSchema {
                    name: schema_name.to_string(),
                    strict: true,
                    schema,
                },
            }),
        }
    }

    pub(crate) fn responses_text_request_body(
        &self,
        model: &str,
        instructions: &str,
        input: Value,
    ) -> ResponsesCreateRequest {
        ResponsesCreateRequest {
            model: model.to_string(),
            input: responses_input(input),
            instructions: Some(instructions.to_string()),
            text: Some(super::types::TextFormatConfig {
                format: super::types::TextFormat::Text,
            }),
        }
    }

    async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> MemcoreResult<R> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.http.post(&url).json(body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|err| self.redact(map_transport_error(err)))?;

        let status = response.status();
        let retry_after_ms = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after_ms);
        let bytes = response
            .bytes()
            .await
            .map_err(|err| self.redact(map_transport_error(err)))?;

        if status.is_success() {
            // Record what the provider reported about usage (never invented).
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                crate::usage::capture_reported_usage(crate::usage::parse_reported_usage(
                    &value,
                    path != "/embeddings",
                ));
            }
            return serde_json::from_slice(&bytes).map_err(|err| {
                MemcoreError::ProviderError(format!("invalid JSON from OpenAI: {err}"))
            });
        }

        Err(self.redact(map_http_error(status, &bytes, retry_after_ms)))
    }

    /// Removes the API key from any error text (defence in depth: provider error bodies can echo
    /// request data).
    fn redact(&self, error: MemcoreError) -> MemcoreError {
        let Some(key) = self.api_key.as_deref().filter(|key| key.len() >= 4) else {
            return error;
        };
        let scrub = |text: String| text.replace(key, "[redacted]");
        match error {
            MemcoreError::ProviderError(message) => MemcoreError::ProviderError(scrub(message)),
            MemcoreError::ProviderThrottled {
                status,
                retry_after_ms,
                message,
            } => MemcoreError::ProviderThrottled {
                status,
                retry_after_ms,
                message: scrub(message),
            },
            other => other,
        }
    }
}

fn normalize_base_url(base_url: String) -> String {
    let trimmed = base_url.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return "https://api.openai.com/v1".to_string();
    }
    trimmed
}

fn map_transport_error(err: reqwest::Error) -> MemcoreError {
    if err.is_timeout() {
        return MemcoreError::ProviderError("OpenAI request timed out".to_string());
    }
    MemcoreError::ProviderError(format!("OpenAI HTTP request failed: {err}"))
}

/// A `Retry-After` value in milliseconds: delta-seconds or an HTTP date.
/// Negative, non-numeric or absurd values (more than a day) are ignored.
pub fn parse_retry_after_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return (seconds <= 86_400).then_some(seconds * 1_000);
    }
    let when = httpdate::parse_http_date(value).ok()?;
    let wait = when
        .duration_since(std::time::SystemTime::now())
        .unwrap_or(Duration::ZERO);
    (wait.as_secs() <= 86_400).then_some(wait.as_millis() as u64)
}

fn map_http_error(status: StatusCode, body: &[u8], retry_after_ms: Option<u64>) -> MemcoreError {
    // Upstream load refusals carry their Retry-After to the retry policy.
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        let message = serde_json::from_slice::<ApiErrorResponse>(body)
            .map(|parsed| parsed.error.message)
            .unwrap_or_else(|_| "provider is throttling requests".to_string());
        return MemcoreError::ProviderThrottled {
            status: status.as_u16(),
            retry_after_ms,
            message,
        };
    }
    if status == StatusCode::UNAUTHORIZED {
        return MemcoreError::ProviderError("OpenAI API key is unauthorized".to_string());
    }

    if let Ok(parsed) = serde_json::from_slice::<ApiErrorResponse>(body) {
        return MemcoreError::ProviderError(format!(
            "OpenAI API error ({}): {}",
            status.as_u16(),
            parsed.error.message
        ));
    }

    MemcoreError::ProviderError(format!("OpenAI API error with status {}", status.as_u16()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A provider timeout above 90 s reaches the HTTP client (CPU inference); up to
    /// 90 s (and with no provider timeout) the HTTP timeout stays exactly 90 s, as before.
    #[test]
    fn http_timeout_is_the_larger_of_the_default_and_the_provider_timeout() {
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(http_timeout(None), Duration::from_secs(90));
        assert_eq!(http_timeout(secs(30)), Duration::from_secs(90));
        assert_eq!(http_timeout(secs(90)), Duration::from_secs(90));
        assert_eq!(http_timeout(secs(300)), Duration::from_secs(300));
        assert!(
            OpenAiClient::with_optional_key_and_timeout(None, "http://127.0.0.1:1/v1", secs(300))
                .is_ok()
        );
    }

    #[test]
    fn missing_api_key_returns_validation_error() {
        let error = OpenAiClient::new("  ", "https://api.openai.com/v1").expect_err("should fail");
        assert_eq!(
            error,
            MemcoreError::ValidationError("OPENAI_API_KEY cannot be empty".to_string())
        );
    }

    #[test]
    fn responses_input_sends_objects_as_json_text_and_keeps_arrays_and_strings() {
        let client = OpenAiClient::new("test-key", "https://api.openai.com/v1")
            .expect("client should build");
        let payload = json!({"candidate_fact": {"content": "x"}, "existing_facts": []});
        let body = client.responses_request_body(
            "gpt-4o-mini",
            "instructions",
            payload.clone(),
            "memcore_fact_classification",
            json!({"type": "object"}),
        );
        let v = serde_json::to_value(&body).expect("serialize");
        assert_eq!(v["input"], Value::String(payload.to_string()));
        let text = client.responses_text_request_body("gpt-4o-mini", "i", payload.clone());
        assert_eq!(
            serde_json::to_value(&text).expect("serialize")["input"],
            Value::String(payload.to_string())
        );

        let items = json!([{"role": "user", "content": "hello"}]);
        let body = client.responses_request_body("m", "i", items.clone(), "n", json!({}));
        assert_eq!(
            serde_json::to_value(&body).expect("serialize")["input"],
            items
        );
        let body = client.responses_request_body("m", "i", json!("plain"), "n", json!({}));
        assert_eq!(
            serde_json::to_value(&body).expect("serialize")["input"],
            json!("plain")
        );
    }

    #[test]
    fn responses_request_includes_json_schema_format() {
        let client = OpenAiClient::new("test-key", "https://api.openai.com/v1")
            .expect("client should build");
        let body = client.responses_request_body(
            "gpt-4.1-mini",
            "system instructions",
            json!([{"role":"user","content":"hello"}]),
            "fact_extraction",
            json!({"type":"object"}),
        );

        let serialized = serde_json::to_value(&body).expect("serialize");
        assert_eq!(serialized["model"], "gpt-4.1-mini");
        assert_eq!(serialized["text"]["format"]["type"], "json_schema");
        assert_eq!(serialized["text"]["format"]["name"], "fact_extraction");
        assert!(serialized["text"]["format"]["strict"].as_bool().unwrap());
    }
}
