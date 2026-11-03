/// Replacement text used for redacted secret values.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// JSON object keys treated as secret-bearing when redacting structured values.
pub const SENSITIVE_JSON_KEYS: &[&str] = &[
    "password",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "key_hash",
    "provider_secret",
    "authorization",
    "openai_api_key",
    "bearer",
    "raw_key",
    "pepper",
];

/// Toggle flags for pattern-based redaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedactionConfig {
    pub redact_emails: bool,
    pub redact_bearer_tokens: bool,
    pub redact_api_keys: bool,
    pub redact_database_urls: bool,
    pub redact_provider_keys: bool,
    pub redact_redis_urls: bool,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        default_redaction_config()
    }
}

pub fn default_redaction_config() -> RedactionConfig {
    RedactionConfig {
        redact_emails: false,
        redact_bearer_tokens: true,
        redact_api_keys: true,
        redact_database_urls: true,
        redact_provider_keys: true,
        redact_redis_urls: true,
    }
}

pub(crate) const DATABASE_URL_SCHEMES: &[&str] = &[
    "postgres://",
    "postgresql://",
    "mysql://",
    "mongodb://",
    "sqlite://",
];

pub(crate) const REDIS_URL_SCHEMES: &[&str] = &["redis://", "rediss://"];

pub(crate) const KEY_VALUE_SECRET_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "password",
    "secret",
    "token",
    "key_hash",
    "provider_secret",
    "openai_api_key",
    "memcore_dev_api_key",
    "memcore_api_key_pepper",
    "memcore_postgres_url",
    "memcore_redis_url",
];

pub(crate) const PROVIDER_KEY_PREFIXES: &[&str] = &["sk-", "sk_live_", "sk_test_", "mc_live_"];
