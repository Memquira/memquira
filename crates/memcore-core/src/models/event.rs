use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEventOperation {
    Add,
    Update,
    Delete,
    NoOp,
    ForgetUser,
    /// A fact was superseded and kept with `invalid_at` set (history).
    Invalidate,
    /// A mention was merged into an existing entity as an alias
    /// (reversible; metadata names the entity, alias and similarity).
    EntityMerge,
    /// A merged alias was split back out into an entity of its own.
    EntitySplit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    pub fact_id: Option<Uuid>,
    pub operation: MemoryEventOperation,
    /// Raw user input is not stored by default for privacy.
    pub input_text: Option<String>,
    pub previous_content: Option<String>,
    pub new_content: Option<String>,
    pub provider_name: Option<String>,
    pub model_name: Option<String>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

impl MemoryEvent {
    /// The only shape of event that forget-user keeps for an erased user. Every
    /// free-form field is empty and the metadata is exactly `{"deleted": true}`; the provider name
    /// may only be absent or the fixed marker `maintenance`. Anything else (for example an imported
    /// `forget_user` event whose metadata carries text) is erased. The SQL stores use the same
    /// predicate.
    pub fn is_content_free_tombstone(&self) -> bool {
        self.operation == MemoryEventOperation::ForgetUser
            && self.fact_id.is_none()
            && self.input_text.is_none()
            && self.previous_content.is_none()
            && self.new_content.is_none()
            && self.model_name.is_none()
            && self
                .provider_name
                .as_deref()
                .is_none_or(|name| name == "maintenance")
            && self.metadata == serde_json::json!({ "deleted": true })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        org_id: impl Into<String>,
        user_id: impl Into<String>,
        fact_id: Option<Uuid>,
        operation: MemoryEventOperation,
        previous_content: Option<String>,
        new_content: Option<String>,
        provider_name: Option<String>,
        model_name: Option<String>,
        metadata: Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            org_id: org_id.into(),
            user_id: user_id.into(),
            fact_id,
            operation,
            input_text: None,
            previous_content,
            new_content,
            provider_name,
            model_name,
            metadata,
            created_at: Utc::now(),
        }
    }
}
