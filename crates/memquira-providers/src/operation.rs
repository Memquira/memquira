//! Operation-token forwarding for provider-side cost attribution.
//!
//! A trusted caller in front of Memquira (for example a hosted gateway that budgets each request) can attach an
//! opaque operation token to an API request. While that request is served, every OpenAI-compatible provider HTTP
//! call made on its behalf carries the token in [`OPERATION_HEADER`], so a provider-side proxy can charge each
//! call to the caller's operation. Memquira does not interpret the token: the proxy verifies it. Calls made outside
//! a request (startup probes, background jobs, detached refresh tasks) carry no token.
//!
//! Forwarding is off unless `MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED=true`; the API layer only scopes a token
//! when it is enabled. Even then a client sends it only when its base URL's host is listed in
//! `MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS` (the internal proxy), never to any other provider URL. The token is
//! never logged by Memquira.

use std::future::Future;

/// Header read from API requests and sent on provider calls.
pub const OPERATION_HEADER: &str = "x-memquira-operation";

/// Longest accepted token.
pub const MAX_OPERATION_TOKEN_LEN: usize = 512;

tokio::task_local! {
    static OPERATION_TOKEN: String;
}

/// Runs `future` with `token` as the current operation token (no token: runs it unchanged).
pub async fn with_operation_token<F: Future>(token: Option<String>, future: F) -> F::Output {
    match token {
        Some(token) => OPERATION_TOKEN.scope(token, future).await,
        None => future.await,
    }
}

/// The operation token of the request being served on this task, if any.
pub fn current_operation_token() -> Option<String> {
    OPERATION_TOKEN.try_with(Clone::clone).ok()
}

/// Whether a client with `base_url` may forward the token: its host (and port, when an entry names one) is one of
/// `allowed_hosts` (`host` or `host:port`, case-insensitive, exact; no wildcards). Anything unparsable is refused.
pub fn forwarding_allowed(base_url: &str, allowed_hosts: &[String]) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    let port = url.port_or_known_default();
    allowed_hosts.iter().any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        match entry.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => {
                h == host && p.parse::<u16>().ok() == port
            }
            _ => !entry.is_empty() && entry == host,
        }
    })
}

/// A token is 1..=512 characters of `A-Z a-z 0-9 . _ - :` (safe as a header value, never echoed into JSON).
pub fn is_valid_operation_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPERATION_TOKEN_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_is_scoped_to_the_future_and_absent_elsewhere() {
        assert_eq!(current_operation_token(), None);
        // Nested calls on the same task see it.
        let nested = async { current_operation_token() };
        let inside = with_operation_token(Some("v1.op.sig".to_string()), nested).await;
        assert_eq!(inside.as_deref(), Some("v1.op.sig"));
        assert_eq!(
            with_operation_token(None, async { current_operation_token() }).await,
            None
        );
        // A detached task does not inherit it.
        let spawned = with_operation_token(Some("v1.op.sig".to_string()), async {
            tokio::spawn(async { current_operation_token() })
                .await
                .expect("join")
        })
        .await;
        assert_eq!(spawned, None);
    }

    #[test]
    fn forwards_only_to_listed_hosts() {
        let hosts = vec!["vertex-adapter:8790".to_string(), "Localhost".to_string()];
        assert!(forwarding_allowed("http://vertex-adapter:8790/v1", &hosts));
        assert!(forwarding_allowed("http://VERTEX-ADAPTER:8790/v1/", &hosts));
        assert!(forwarding_allowed("http://localhost:1234/v1", &hosts)); // a host entry without a port: any port
        assert!(!forwarding_allowed("http://vertex-adapter:9999/v1", &hosts)); // other port
        assert!(!forwarding_allowed("https://api.openai.com/v1", &hosts));
        assert!(!forwarding_allowed(
            "https://vertex-adapter.evil.example/v1",
            &hosts
        ));
        assert!(!forwarding_allowed(
            "http://evil.example/vertex-adapter:8790",
            &hosts
        ));
        assert!(!forwarding_allowed("not a url", &hosts));
        assert!(!forwarding_allowed("http://vertex-adapter:8790/v1", &[]));
        assert!(!forwarding_allowed(
            "http://vertex-adapter:8790/v1",
            &["".to_string()]
        ));
    }

    #[test]
    fn validates_token_shape() {
        assert!(is_valid_operation_token(
            "v1.0f8e2c1a-9b7d-4c2e-8a1b-3c4d5e6f7a8b.1763800000.AbC-_x"
        ));
        assert!(!is_valid_operation_token(""));
        assert!(!is_valid_operation_token("has space"));
        assert!(!is_valid_operation_token("line\nbreak"));
        assert!(!is_valid_operation_token("quote\""));
        assert!(!is_valid_operation_token(
            &"a".repeat(MAX_OPERATION_TOKEN_LEN + 1)
        ));
        assert!(is_valid_operation_token(
            &"a".repeat(MAX_OPERATION_TOKEN_LEN)
        ));
    }
}
