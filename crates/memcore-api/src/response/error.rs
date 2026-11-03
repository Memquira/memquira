use axum::http::StatusCode;
use memcore_common::{MemcoreError, safe_error_message};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
                details: None,
                request_id: None,
            },
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.error.details = Some(details);
        self
    }

    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.error.request_id = request_id;
        self
    }

    pub fn from_memcore_error(error: MemcoreError) -> (StatusCode, Self) {
        // Every MemcoreError that becomes an HTTP response passes through here exactly once, which
        // makes this the one place the truncation counter cannot be missed.
        if let MemcoreError::ProviderOutputTruncated { operation, .. } = &error {
            crate::metrics::ops::record_provider_output_truncated(operation);
        }
        let status = match &error {
            MemcoreError::Unauthorized => StatusCode::UNAUTHORIZED,
            MemcoreError::Forbidden => StatusCode::FORBIDDEN,
            MemcoreError::BadRequest(_) => StatusCode::BAD_REQUEST,
            MemcoreError::NotFound(_) => StatusCode::NOT_FOUND,
            MemcoreError::Conflict(_) => StatusCode::CONFLICT,
            MemcoreError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            MemcoreError::ProviderError(_) => StatusCode::BAD_GATEWAY,
            MemcoreError::StorageError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MemcoreError::MigrationError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MemcoreError::ValidationError(_) => StatusCode::BAD_REQUEST,
            MemcoreError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MemcoreError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            MemcoreError::Overloaded(_) => StatusCode::SERVICE_UNAVAILABLE,
            MemcoreError::ProviderThrottled { .. } => StatusCode::SERVICE_UNAVAILABLE,
            MemcoreError::QuotaExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
            MemcoreError::ProviderGuardrailViolation { .. } => StatusCode::FORBIDDEN,
            // The upstream model returned an unusable (incomplete) response.
            MemcoreError::ProviderOutputTruncated { .. } => StatusCode::BAD_GATEWAY,
            MemcoreError::ProviderOutputInvalid { .. } => StatusCode::BAD_GATEWAY,
        };

        let code = match &error {
            MemcoreError::Unauthorized => "UNAUTHORIZED",
            MemcoreError::Forbidden => "FORBIDDEN",
            MemcoreError::BadRequest(_) => "BAD_REQUEST",
            MemcoreError::NotFound(_) => "NOT_FOUND",
            // A reused client message_id that identifies a different message.
            error @ MemcoreError::Conflict(_) if error.is_message_id_conflict() => {
                "MESSAGE_ID_CONFLICT"
            }
            // Append-import retry / re-application and deleted-id conflicts.
            error @ MemcoreError::Conflict(_) if error.is_import_already_applied() => {
                "IMPORT_ALREADY_APPLIED"
            }
            error @ MemcoreError::Conflict(_) if error.is_import_id_conflict() => {
                "IMPORT_ID_CONFLICT"
            }
            MemcoreError::Conflict(_) => "CONFLICT",
            MemcoreError::RateLimited => "RATE_LIMITED",
            MemcoreError::ProviderError(_) => "PROVIDER_ERROR",
            MemcoreError::StorageError(_) => "STORAGE_ERROR",
            MemcoreError::MigrationError(_) => "MIGRATION_ERROR",
            MemcoreError::ValidationError(_) => "VALIDATION_ERROR",
            MemcoreError::Internal(_) => "INTERNAL_ERROR",
            MemcoreError::Timeout(_) => "TIMEOUT",
            MemcoreError::Overloaded(_) => "OVERLOADED",
            MemcoreError::ProviderThrottled { .. } => "PROVIDER_THROTTLED",
            MemcoreError::QuotaExceeded { .. } => "QUOTA_EXCEEDED",
            MemcoreError::ProviderGuardrailViolation { .. } => "PROVIDER_GUARDRAIL_VIOLATION",
            MemcoreError::ProviderOutputTruncated { .. } => "PROVIDER_OUTPUT_TRUNCATED",
            MemcoreError::ProviderOutputInvalid { .. } => "PROVIDER_OUTPUT_INVALID",
        };

        let body = Self::new(code, safe_error_message(&error));
        let body = match &error {
            MemcoreError::QuotaExceeded {
                kind,
                limit,
                current,
                requested,
                ..
            } => body.with_details(json!({
                "kind": kind,
                "limit": limit,
                "current": current,
                "requested": requested,
            })),
            MemcoreError::ProviderGuardrailViolation {
                code: guard_code, ..
            } => body.with_details(json!({ "guardrail_code": guard_code })),
            MemcoreError::ProviderOutputTruncated {
                operation,
                max_output_tokens,
            } => body.with_details(json!({
                "operation": operation,
                "max_output_tokens": max_output_tokens,
            })),
            MemcoreError::ProviderOutputInvalid { operation, reason } => body.with_details(json!({
                "operation": operation,
                "reason": reason,
            })),
            _ => body,
        };

        (status, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_a_distinct_bad_gateway_error_with_its_operation_and_limit() {
        let (status, body) = ErrorBody::from_memcore_error(
            MemcoreError::provider_output_truncated("extract_facts", Some(2_048)),
        );

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body.error.code, "PROVIDER_OUTPUT_TRUNCATED");
        assert_eq!(
            body.error.details,
            Some(json!({ "operation": "extract_facts", "max_output_tokens": 2048 }))
        );
        // The message is built from the operation name alone, never from model output.
        assert_eq!(
            body.error.message,
            "provider output truncated at the output-token limit during extract_facts"
        );
    }

    #[test]
    fn a_message_id_conflict_is_a_409_naming_only_the_position() {
        let (status, body) = ErrorBody::from_memcore_error(MemcoreError::message_id_conflict(2));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.error.code, "MESSAGE_ID_CONFLICT");
        assert!(body.error.message.contains("messages[2].message_id"));
        assert!(body.error.details.is_none());

        // Any other conflict keeps its old code.
        let (status, body) =
            ErrorBody::from_memcore_error(MemcoreError::Conflict("already exists".to_string()));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.error.code, "CONFLICT");
    }

    #[test]
    fn a_generic_provider_error_keeps_its_old_code() {
        let (status, body) =
            ErrorBody::from_memcore_error(MemcoreError::ProviderError("upstream 500".to_string()));
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body.error.code, "PROVIDER_ERROR");
    }
}
