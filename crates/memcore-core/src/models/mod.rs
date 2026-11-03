mod api_key;
mod entity;
mod episode;
mod event;
mod fact;
mod memory;
mod temporal;
mod tenant;
mod validation;

pub use api_key::{ApiKeyRecord, ApiKeyScope};
pub use entity::{
    AliasSource, Entity, EntityAlias, EntityRelation, EntityRole, EntityType, FactEntityLink,
    normalize_entity_name,
};
pub use episode::{
    Episode, MIN_EMBEDDED_EPISODE_CHARS, message_role_from_str, message_role_to_str,
};
pub use event::{MemoryEvent, MemoryEventOperation};
pub use fact::{CandidateFact, Fact, FactOperationDecision, FactStructure, MemorySearchResult};
pub use memory::{FactOperation, MemorySource, MemoryType};
pub use temporal::{TemporalGranularity, TemporalValue};
pub use tenant::TenantContext;

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::{CandidateFact, Fact, FactOperation, MemorySource, MemoryType, TenantContext};
    use memcore_common::MemcoreError;

    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn fact_creation_succeeds_with_valid_input() {
        let timestamp = now();
        let fact = Fact::new(
            Uuid::new_v4(),
            "org_123",
            "user_456",
            MemoryType::Skill,
            "User is learning Rust.",
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            timestamp,
            timestamp,
            json!({}),
        )
        .expect("fact should be created");

        assert_eq!(fact.org_id, "org_123");
        assert_eq!(fact.user_id, "user_456");
        assert_eq!(fact.memory_type, MemoryType::Skill);
    }

    #[test]
    fn candidate_fact_creation_succeeds_with_valid_input() {
        let candidate = CandidateFact::new(
            "User prefers concise answers.",
            MemoryType::Preference,
            0.85,
            0.7,
            None,
            json!({ "source": "chat" }),
        )
        .expect("candidate fact should be created");

        assert_eq!(candidate.content, "User prefers concise answers.");
        assert_eq!(candidate.memory_type, MemoryType::Preference);
    }

    #[test]
    fn tenant_context_creation_succeeds_with_valid_input() {
        let tenant = TenantContext::new("org_abc", "user_xyz").expect("tenant should be created");
        assert_eq!(tenant.org_id, "org_abc");
        assert_eq!(tenant.user_id, "user_xyz");
    }

    #[test]
    fn enum_serialization_uses_snake_case() {
        let memory_type =
            serde_json::to_string(&MemoryType::Conversation).expect("memory type should serialize");
        assert_eq!(memory_type, "\"conversation\"");

        let operation = serde_json::to_string(&FactOperation::Summarize)
            .expect("fact operation should serialize");
        assert_eq!(operation, "\"summarize\"");

        let source = serde_json::to_string(&MemorySource::ApiImport)
            .expect("memory source should serialize");
        assert_eq!(source, "\"api_import\"");
    }

    #[test]
    fn enum_deserialization_works() {
        let memory_type: MemoryType =
            serde_json::from_str("\"entity\"").expect("memory type should deserialize");
        assert_eq!(memory_type, MemoryType::Entity);
    }

    #[test]
    fn validation_fails_for_empty_tenant_fields() {
        let error = TenantContext::new("", "user_123").expect_err("empty org_id should fail");
        assert_eq!(
            error,
            MemcoreError::ValidationError("org_id cannot be empty".to_string())
        );

        let error = TenantContext::new("org_123", "   ").expect_err("empty user_id should fail");
        assert_eq!(
            error,
            MemcoreError::ValidationError("user_id cannot be empty".to_string())
        );
    }

    #[test]
    fn validation_fails_for_invalid_confidence_and_importance() {
        let timestamp = now();
        let confidence_error = Fact::new(
            Uuid::new_v4(),
            "org_123",
            "user_456",
            MemoryType::Profile,
            "content",
            None,
            MemorySource::Manual,
            1.2,
            0.5,
            None,
            None,
            timestamp,
            timestamp,
            json!({}),
        )
        .expect_err("invalid confidence should fail");
        assert_eq!(
            confidence_error,
            MemcoreError::ValidationError("confidence must be between 0.0 and 1.0".to_string())
        );

        let importance_error =
            CandidateFact::new("content", MemoryType::Task, 0.5, -0.1, None, json!({}))
                .expect_err("invalid importance should fail");
        assert_eq!(
            importance_error,
            MemcoreError::ValidationError("importance must be between 0.0 and 1.0".to_string())
        );
    }
}
