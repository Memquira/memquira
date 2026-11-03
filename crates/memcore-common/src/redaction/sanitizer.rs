use serde_json::{Map, Value};

use crate::error::{MemcoreError, PROVIDER_CIRCUIT_OPEN_MESSAGE, PROVIDER_TIMEOUT_MESSAGE};

use super::patterns::{
    DATABASE_URL_SCHEMES, KEY_VALUE_SECRET_KEYS, PROVIDER_KEY_PREFIXES, REDACTED_PLACEHOLDER,
    REDIS_URL_SCHEMES, RedactionConfig, SENSITIVE_JSON_KEYS,
};

/// Deterministic, pattern-based secret redactor.
#[derive(Debug, Default, Clone, Copy)]
pub struct Redactor;

impl Redactor {
    pub fn redact_str(input: &str) -> String {
        Self::redact_str_with_config(input, &RedactionConfig::default())
    }

    pub fn redact_str_with_config(input: &str, config: &RedactionConfig) -> String {
        let mut out = input.to_string();

        if config.redact_bearer_tokens {
            out = redact_authorization_bearer(&out);
            out = redact_prefixed_token(&out, "Bearer ");
            out = redact_prefixed_token(&out, "bearer ");
        }

        if config.redact_database_urls {
            for scheme in DATABASE_URL_SCHEMES {
                out = redact_url_scheme(&out, scheme);
            }
            // Handle bare `sqlite:` after `sqlite://`.
            out = redact_url_scheme(&out, "sqlite:");
        }

        if config.redact_redis_urls {
            for scheme in REDIS_URL_SCHEMES {
                out = redact_url_scheme(&out, scheme);
            }
        }

        if config.redact_provider_keys {
            for prefix in PROVIDER_KEY_PREFIXES {
                out = redact_prefixed_token(&out, prefix);
            }
        }

        if config.redact_api_keys {
            for key in KEY_VALUE_SECRET_KEYS {
                out = redact_key_value(&out, key);
            }
        }

        if config.redact_emails {
            out = redact_emails(&out);
        }

        out
    }

    pub fn redact_json(value: Value) -> Value {
        Self::redact_json_with_config(value, &RedactionConfig::default())
    }

    pub fn redact_json_with_config(value: Value, config: &RedactionConfig) -> Value {
        match value {
            Value::Object(map) => Value::Object(redact_object(map, config)),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| Self::redact_json_with_config(item, config))
                    .collect(),
            ),
            Value::String(text) => Value::String(Self::redact_str_with_config(&text, config)),
            other => other,
        }
    }
}

/// Backwards-compatible alias for string redaction.
pub fn sanitize_error_message(message: &str) -> String {
    Redactor::redact_str(message)
}

/// User-facing API error message. Infrastructure details are replaced with safe generics.
pub fn safe_error_message(error: &MemcoreError) -> String {
    match error {
        MemcoreError::Unauthorized => "unauthorized".to_string(),
        MemcoreError::Forbidden => "forbidden".to_string(),
        MemcoreError::RateLimited => "rate limit exceeded".to_string(),
        MemcoreError::StorageError(_) => "database operation failed".to_string(),
        MemcoreError::MigrationError(_) => "database migration failed".to_string(),
        MemcoreError::Internal(_) => "internal error".to_string(),
        MemcoreError::ProviderError(message) if message == PROVIDER_CIRCUIT_OPEN_MESSAGE => {
            PROVIDER_CIRCUIT_OPEN_MESSAGE.to_string()
        }
        MemcoreError::ProviderError(message) if looks_like_provider_auth_failure(message) => {
            "provider authentication failed".to_string()
        }
        MemcoreError::ProviderError(_) => "provider operation failed".to_string(),
        MemcoreError::Timeout(message) if message == PROVIDER_TIMEOUT_MESSAGE => {
            PROVIDER_TIMEOUT_MESSAGE.to_string()
        }
        MemcoreError::Timeout(_) => "operation timed out".to_string(),
        MemcoreError::Overloaded(_) => "server is at capacity; retry later".to_string(),
        MemcoreError::ProviderThrottled { .. } => {
            "the model provider is throttling requests; retry later".to_string()
        }
        MemcoreError::ValidationError(message)
        | MemcoreError::BadRequest(message)
        | MemcoreError::NotFound(message)
        | MemcoreError::Conflict(message) => Redactor::redact_str(message),
        MemcoreError::QuotaExceeded { message, .. } => Redactor::redact_str(message),
        MemcoreError::ProviderGuardrailViolation { message, .. } => Redactor::redact_str(message),
        // Built only from a fixed operation name and a number, so it carries no user or model text.
        error @ MemcoreError::ProviderOutputTruncated { .. } => error.to_string(),
        // Built only from fixed operation and reason names: no user or model text.
        error @ MemcoreError::ProviderOutputInvalid { .. } => error.to_string(),
    }
}

fn looks_like_provider_auth_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("authentication")
        || lower.contains("401")
}

fn redact_object(map: Map<String, Value>, config: &RedactionConfig) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, value) in map {
        if is_sensitive_json_key(&key) {
            out.insert(key, Value::String(REDACTED_PLACEHOLDER.to_string()));
        } else {
            out.insert(key, Redactor::redact_json_with_config(value, config));
        }
    }
    out
}

fn is_sensitive_json_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    SENSITIVE_JSON_KEYS
        .iter()
        .any(|candidate| normalized == *candidate)
}

fn redact_authorization_bearer(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let marker = "authorization:";
    let mut out = String::with_capacity(message.len());
    let mut idx = 0;

    while let Some(rel) = lower[idx..].find(marker) {
        let start = idx + rel;
        out.push_str(&message[idx..start]);
        out.push_str(&message[start..start + marker.len()]);
        let after = start + marker.len();
        let trimmed = message[after..]
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count();
        let value_start = after + trimmed;
        out.push_str(&message[after..value_start]);

        let rest = &message[value_start..];
        let rest_lower = rest.to_ascii_lowercase();
        if rest_lower.starts_with("bearer ") {
            out.push_str("Bearer ");
            out.push_str(REDACTED_PLACEHOLDER);
            let token_start = value_start + "bearer ".len();
            let token_end = message[token_start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '}'))
                .map(|offset| token_start + offset)
                .unwrap_or(message.len());
            idx = token_end;
        } else {
            out.push_str(REDACTED_PLACEHOLDER);
            let end = message[value_start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '}'))
                .map(|offset| value_start + offset)
                .unwrap_or(message.len());
            idx = end;
        }
    }
    out.push_str(&message[idx..]);
    out
}

fn redact_url_scheme(message: &str, scheme: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let scheme_lower = scheme.to_ascii_lowercase();
    let mut out = String::with_capacity(message.len());
    let mut idx = 0;

    while let Some(rel) = lower[idx..].find(&scheme_lower) {
        let start = idx + rel;
        out.push_str(&message[idx..start]);
        out.push_str(scheme);
        out.push_str(REDACTED_PLACEHOLDER);
        let after = start + scheme.len();
        let end = message[after..]
            .find(char::is_whitespace)
            .map(|offset| after + offset)
            .unwrap_or(message.len());
        idx = end;
    }
    out.push_str(&message[idx..]);
    out
}

fn redact_prefixed_token(message: &str, prefix: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let prefix_lower = prefix.to_ascii_lowercase();
    let mut out = String::with_capacity(message.len());
    let mut idx = 0;

    while let Some(rel) = lower[idx..].find(&prefix_lower) {
        let start = idx + rel;
        // Avoid matching mid-word (e.g. keep "token budget" when looking for "token").
        if start > 0 {
            let prev = message[..start].chars().next_back();
            if prev.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                out.push_str(&message[idx..start + 1]);
                idx = start + 1;
                continue;
            }
        }

        out.push_str(&message[idx..start]);
        out.push_str(&message[start..start + prefix.len()]);
        out.push_str(REDACTED_PLACEHOLDER);
        let value_start = start + prefix.len();
        let value_end = message[value_start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '}'))
            .map(|offset| value_start + offset)
            .unwrap_or(message.len());
        idx = value_end;
    }
    out.push_str(&message[idx..]);
    out
}

fn redact_key_value(message: &str, key: &str) -> String {
    let mut current = message.to_string();
    for pattern in [
        format!("{key}="),
        format!("{key}:"),
        format!("\"{key}\":"),
        format!("'{key}':"),
    ] {
        current = redact_after_pattern(&current, &pattern);
    }
    current
}

fn redact_after_pattern(message: &str, pattern: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let pattern_lower = pattern.to_ascii_lowercase();
    let mut out = String::with_capacity(message.len());
    let mut idx = 0;

    while let Some(rel) = lower[idx..].find(&pattern_lower) {
        let start = idx + rel;
        // Require a non-alphanumeric boundary before the key to avoid "token budget".
        if start > 0 {
            let prev = message[..start].chars().next_back();
            if prev.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                out.push_str(&message[idx..start + 1]);
                idx = start + 1;
                continue;
            }
        }

        let value_start = start + pattern.len();
        out.push_str(&message[idx..value_start]);
        let trimmed = message[value_start..]
            .chars()
            .take_while(|ch| ch.is_whitespace() || matches!(*ch, '"' | '\''))
            .count();
        let token_start = value_start + trimmed;
        out.push_str(&message[value_start..token_start]);
        out.push_str(REDACTED_PLACEHOLDER);
        let token_end = message[token_start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '}'))
            .map(|offset| token_start + offset)
            .unwrap_or(message.len());
        idx = token_end;
    }
    out.push_str(&message[idx..]);
    out
}

fn redact_emails(message: &str) -> String {
    // Lightweight optional email redaction without a regex dependency.
    let mut out = String::with_capacity(message.len());
    let mut chars = message.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch == '@' {
            let local_start = find_email_local_start(message, idx);
            let domain_end = find_email_domain_end(message, idx + 1);
            if local_start < idx && domain_end > idx + 1 {
                // Rewind what we already copied for the local-part.
                while out.len() > local_start {
                    out.pop();
                }
                out.push_str(REDACTED_PLACEHOLDER);
                // Advance iterator past the email.
                while chars.peek().is_some_and(|(i, _)| *i < domain_end) {
                    chars.next();
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn find_email_local_start(message: &str, at_idx: usize) -> usize {
    let bytes = message.as_bytes();
    let mut i = at_idx;
    while i > 0 {
        let prev = bytes[i - 1];
        if prev.is_ascii_alphanumeric() || matches!(prev, b'.' | b'_' | b'%' | b'+' | b'-') {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

fn find_email_domain_end(message: &str, start: usize) -> usize {
    let bytes = message.as_bytes();
    let mut i = start;
    let mut saw_dot = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'-' {
            i += 1;
        } else if b == b'.' {
            saw_dot = true;
            i += 1;
        } else {
            break;
        }
    }
    if saw_dot { i } else { start }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_bearer_token() {
        let message = Redactor::redact_str("Authorization: Bearer super-secret-token");
        assert!(!message.contains("super-secret-token"));
        assert!(message.contains(REDACTED_PLACEHOLDER));
    }

    #[test]
    fn redacts_api_key_query_parameter() {
        let message = Redactor::redact_str("https://api.example/v1?api_key=abc123&limit=10");
        assert!(!message.contains("abc123"));
        assert!(message.contains("api_key=[REDACTED]"));
    }

    #[test]
    fn redacts_json_api_key_and_password_fields() {
        let value = Redactor::redact_json(json!({
            "api_key": "secret-key",
            "password": "hunter2",
            "key_hash": "abc",
            "name": "ok"
        }));
        assert_eq!(value["api_key"], REDACTED_PLACEHOLDER);
        assert_eq!(value["password"], REDACTED_PLACEHOLDER);
        assert_eq!(value["key_hash"], REDACTED_PLACEHOLDER);
        assert_eq!(value["name"], "ok");
    }

    #[test]
    fn redacts_database_and_redis_url_passwords() {
        let message = Redactor::redact_str(
            "connect postgres://user:secret@db/memcore redis://:pass@localhost:6379/0",
        );
        assert!(!message.contains("secret"));
        assert!(!message.contains(":pass@"));
        assert!(message.contains("postgres://[REDACTED]"));
        assert!(message.contains("redis://[REDACTED]"));
    }

    #[test]
    fn redacts_provider_key_prefix() {
        let message = Redactor::redact_str("using key sk-live-abcdef");
        assert!(!message.contains("sk-live-abcdef"));
        assert!(message.contains("sk-[REDACTED]") || message.contains(REDACTED_PLACEHOLDER));
    }

    #[test]
    fn preserves_ordinary_text_and_token_budget() {
        assert_eq!(
            Redactor::redact_str("user_id cannot be empty"),
            "user_id cannot be empty"
        );
        assert_eq!(
            Redactor::redact_str("exceeded token budget for context"),
            "exceeded token budget for context"
        );
        assert_eq!(
            Redactor::redact_str("use a secret manager for credentials"),
            "use a secret manager for credentials"
        );
    }

    #[test]
    fn safe_error_message_uses_generic_infrastructure_text() {
        let storage =
            MemcoreError::StorageError("failed postgres://user:secret@localhost/db".to_string());
        assert_eq!(safe_error_message(&storage), "database operation failed");
        assert!(!safe_error_message(&storage).contains("secret"));

        let provider = MemcoreError::ProviderError("invalid api key sk-abc".to_string());
        assert_eq!(
            safe_error_message(&provider),
            "provider authentication failed"
        );

        let migration = MemcoreError::MigrationError("checksum mismatch on 0001.sql".to_string());
        assert_eq!(safe_error_message(&migration), "database migration failed");
    }
}
