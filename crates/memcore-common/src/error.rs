use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MemcoreError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("rate limited")]
    RateLimited,
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("storage error: {0}")]
    StorageError(String),
    #[error("migration error: {0}")]
    MigrationError(String),
    #[error("validation error: {0}")]
    ValidationError(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("timeout: {0}")]
    Timeout(String),
    /// The server is at capacity (admission / provider bounds): retry later. Maps to
    /// `503 OVERLOADED` with `Retry-After`; never retried inside the provider policy.
    #[error("overloaded: {0}")]
    Overloaded(String),
    /// The upstream model provider refused for load (429 / 503), possibly with a Retry-After
    /// . Distinct from memcore's own `Overloaded` admission shedding.
    #[error("provider throttled ({status}): {message}")]
    ProviderThrottled {
        status: u16,
        retry_after_ms: Option<u64>,
        message: String,
    },
    #[error("quota exceeded: {message}")]
    QuotaExceeded {
        message: String,
        kind: String,
        limit: u64,
        current: u64,
        requested: u64,
    },
    /// Provider call blocked by test-mode / cost guardrails (safe message only).
    #[error("provider guardrail violation: {message}")]
    ProviderGuardrailViolation { code: String, message: String },
    /// The model stopped because it hit the output-token limit, so its response is incomplete.
    ///
    /// Kept apart from `ProviderError` for two reasons. It is not retryable: the same request with
    /// the same limit produces the same cut-off response, and an OpenAI-compatible cache in front
    /// of the model will replay it verbatim. And it is a configuration problem, not a provider
    /// outage, so it must not trip the circuit breaker. Before this existed, a truncated extraction
    /// surfaced as an unhelpful JSON parse failure.
    #[error("provider output truncated at the output-token limit during {operation}")]
    ProviderOutputTruncated {
        /// A fixed operation name from the code (`extract_facts`, …), safe as a metric label.
        operation: &'static str,
        /// The limit that was sent with the request, when one was.
        max_output_tokens: Option<u32>,
    },
    /// The model's output parsed but breaks the operation's contract, e.g. a classification with an
    /// unknown operation label (`"None"`), a non-UUID target for an update or delete, or a confidence
    /// outside 0..=1. Like truncation it is not a provider outage: it is not
    /// retried and does not trip the circuit breaker. Callers that can reject the affected item on
    /// its own (one candidate fact) do so and account for it; otherwise it is a 502.
    #[error("provider output invalid during {operation}: {reason}")]
    ProviderOutputInvalid {
        /// A fixed operation name from the code, safe as a metric label.
        operation: &'static str,
        /// A fixed reason from the code (no model text).
        reason: &'static str,
    },
}

pub const PROVIDER_TIMEOUT_MESSAGE: &str = "provider operation timed out";
pub const PROVIDER_CIRCUIT_OPEN_MESSAGE: &str = "provider circuit is open";
/// Prefix of the [`MemcoreError::message_id_conflict`] message.
pub const MESSAGE_ID_CONFLICT_PREFIX: &str = "message_id conflict:";
/// Prefix of the [`MemcoreError::import_already_applied`] message.
pub const IMPORT_ALREADY_APPLIED_PREFIX: &str = "import already applied:";
/// Prefix of the [`MemcoreError::import_id_conflict`] message.
pub const IMPORT_ID_CONFLICT_PREFIX: &str = "import id conflict:";

pub type MemcoreResult<T> = Result<T, MemcoreError>;

impl MemcoreError {
    pub fn provider_timeout() -> Self {
        Self::Timeout(PROVIDER_TIMEOUT_MESSAGE.to_string())
    }

    pub fn is_provider_timeout(&self) -> bool {
        matches!(self, Self::Timeout(msg) if msg == PROVIDER_TIMEOUT_MESSAGE)
    }

    pub fn provider_circuit_open() -> Self {
        Self::ProviderError(PROVIDER_CIRCUIT_OPEN_MESSAGE.to_string())
    }

    pub fn is_provider_circuit_open(&self) -> bool {
        matches!(self, Self::ProviderError(msg) if msg == PROVIDER_CIRCUIT_OPEN_MESSAGE)
    }

    /// The request's message at `position` reuses a client `message_id` that is
    /// already stored for a different message in the same organization, user and session. The
    /// message names only the position, never the ID or any message content.
    pub fn message_id_conflict(position: usize) -> Self {
        Self::Conflict(format!(
            "{MESSAGE_ID_CONFLICT_PREFIX} messages[{position}].message_id already identifies a \
             different message in this organization, user and session; use a new message_id for \
             a different message"
        ))
    }

    pub fn is_message_id_conflict(&self) -> bool {
        matches!(self, Self::Conflict(msg) if msg.starts_with(MESSAGE_ID_CONFLICT_PREFIX))
    }

    /// An append import whose exported facts are already stored for this
    /// tenant with the same id and identical content (a re-applied or retried import). Rejected
    /// before any write. The message names no id and no content.
    pub fn import_already_applied() -> Self {
        Self::Conflict(format!(
            "{IMPORT_ALREADY_APPLIED_PREFIX} the export's facts are already stored for this user; \
             nothing was imported. A failed import is recovered with mode=replace"
        ))
    }

    pub fn is_import_already_applied(&self) -> bool {
        matches!(self, Self::Conflict(msg) if msg.starts_with(IMPORT_ALREADY_APPLIED_PREFIX))
    }

    /// An append import whose exported fact ids include a fact this tenant
    /// deleted. Rejected before any write. The message names no id and no content.
    pub fn import_id_conflict() -> Self {
        Self::Conflict(format!(
            "{IMPORT_ID_CONFLICT_PREFIX} the export contains fact ids of facts this user deleted; \
             nothing was imported"
        ))
    }

    pub fn is_import_id_conflict(&self) -> bool {
        matches!(self, Self::Conflict(msg) if msg.starts_with(IMPORT_ID_CONFLICT_PREFIX))
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::BadRequest(_) => "bad_request",
            Self::NotFound(_) => "not_found",
            Self::Conflict(msg) if msg.starts_with(MESSAGE_ID_CONFLICT_PREFIX) => {
                "message_id_conflict"
            }
            Self::Conflict(msg) if msg.starts_with(IMPORT_ALREADY_APPLIED_PREFIX) => {
                "import_already_applied"
            }
            Self::Conflict(msg) if msg.starts_with(IMPORT_ID_CONFLICT_PREFIX) => {
                "import_id_conflict"
            }
            Self::Conflict(_) => "conflict",
            Self::RateLimited => "rate_limited",
            Self::ProviderError(msg) if msg == PROVIDER_CIRCUIT_OPEN_MESSAGE => {
                "provider_circuit_open"
            }
            Self::ProviderError(_) => "provider_error",
            Self::StorageError(_) => "storage_error",
            Self::MigrationError(_) => "migration_error",
            Self::ValidationError(_) => "validation_error",
            Self::Internal(_) => "internal",
            Self::Timeout(msg) if msg == PROVIDER_TIMEOUT_MESSAGE => "provider_timeout",
            Self::Timeout(_) => "timeout",
            Self::Overloaded(_) => "overloaded",
            Self::ProviderThrottled { .. } => "provider_throttled",
            Self::QuotaExceeded { .. } => "quota_exceeded",
            Self::ProviderGuardrailViolation { .. } => "provider_guardrail_violation",
            Self::ProviderOutputTruncated { .. } => "provider_output_truncated",
            Self::ProviderOutputInvalid { .. } => "provider_output_invalid",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    pub fn quota_exceeded(
        message: impl Into<String>,
        kind: impl Into<String>,
        limit: u64,
        current: u64,
        requested: u64,
    ) -> Self {
        Self::QuotaExceeded {
            message: message.into(),
            kind: kind.into(),
            limit,
            current,
            requested,
        }
    }

    pub fn provider_guardrail_violation(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::ProviderGuardrailViolation {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn is_provider_guardrail_violation(&self) -> bool {
        matches!(self, Self::ProviderGuardrailViolation { .. })
    }

    pub fn provider_output_truncated(
        operation: &'static str,
        max_output_tokens: Option<u32>,
    ) -> Self {
        Self::ProviderOutputTruncated {
            operation,
            max_output_tokens,
        }
    }

    pub fn is_provider_output_truncated(&self) -> bool {
        matches!(self, Self::ProviderOutputTruncated { .. })
    }

    pub fn provider_output_invalid(operation: &'static str, reason: &'static str) -> Self {
        Self::ProviderOutputInvalid { operation, reason }
    }

    pub fn is_provider_output_invalid(&self) -> bool {
        matches!(self, Self::ProviderOutputInvalid { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::{MemcoreError, MemcoreResult};

    #[test]
    fn display_message_includes_variant_text() {
        let error = MemcoreError::BadRequest("missing user_id".to_string());
        assert_eq!(error.to_string(), "bad request: missing user_id");
    }

    #[test]
    fn provider_timeout_uses_dedicated_error_code() {
        let error = MemcoreError::provider_timeout();
        assert_eq!(error.code(), "provider_timeout");
        assert!(error.is_provider_timeout());
    }

    #[test]
    fn provider_circuit_open_uses_dedicated_error_code() {
        let error = MemcoreError::provider_circuit_open();
        assert_eq!(error.code(), "provider_circuit_open");
        assert!(error.is_provider_circuit_open());
    }

    #[test]
    fn quota_exceeded_uses_dedicated_error_code() {
        let error = MemcoreError::quota_exceeded("limit exceeded", "MemoriesPerOrg", 10, 10, 1);
        assert_eq!(error.code(), "quota_exceeded");
    }

    #[test]
    fn code_mapping_matches_expected_values() {
        let error = MemcoreError::StorageError("db unavailable".to_string());
        assert_eq!(error.code(), "storage_error");
        let error = MemcoreError::MigrationError("checksum mismatch".to_string());
        assert_eq!(error.code(), "migration_error");
    }

    #[test]
    fn memcore_result_usage_works() {
        fn validate_user_id(user_id: &str) -> MemcoreResult<()> {
            if user_id.is_empty() {
                return Err(MemcoreError::ValidationError(
                    "user_id cannot be empty".to_string(),
                ));
            }
            Ok(())
        }

        assert!(validate_user_id("user_123").is_ok());

        let error = validate_user_id("").expect_err("empty user_id should return an error");
        assert_eq!(
            error,
            MemcoreError::ValidationError("user_id cannot be empty".to_string())
        );
    }
}
