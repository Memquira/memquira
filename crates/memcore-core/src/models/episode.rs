use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::ports::MessageRole;

/// Episodes shorter than this many characters (Unicode scalar values, after trimming) are stored but
/// not embedded, so semantic episode search cannot return them .
pub const MIN_EMBEDDED_EPISODE_CHARS: usize = 20;

/// One message a client sent, kept as source-message-level memory.
///
/// Episodes are a higher-fidelity fallback relative to extracted facts and support provenance,
/// audit and export. They are not a lossless copy: `content` is stored after PII redaction when
/// redaction is enabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub id: Uuid,
    pub org_id: String,
    pub user_id: String,
    /// The client's session ID, if the request carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The client's message ID: the same ID facts cite in `source_message_ids`. Episodes are unique
    /// per organization, user, session and message ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The message's speaker name, if sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub role: MessageRole,
    pub content: String,
    /// The message's own timestamp, else the session timestamp, else `None`. Never the time memcore
    /// received the request (P2-20); that is `recorded_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// When memcore stored the episode.
    pub recorded_at: DateTime<Utc>,
    /// Whether the episode's vector has been stored.
    #[serde(default)]
    pub embedded: bool,
    #[serde(default)]
    pub metadata: Value,
}

impl Episode {
    /// Whether the episode is long enough to be embedded (`MIN_EMBEDDED_EPISODE_CHARS`).
    pub fn is_embeddable(&self) -> bool {
        self.content.trim().chars().count() >= MIN_EMBEDDED_EPISODE_CHARS
    }

    /// Whether `other`, an incoming episode with the same client `message_id` in the
    /// same organization, user and session, is the same message occurrence as this stored one.
    ///
    /// Compared: role, speaker, content and the client's message time. Both sides are in stored
    /// form (the incoming episode is built by the same path, after the same PII redaction), and the
    /// time is compared at the microsecond precision both stores keep. Not compared: the episode ID
    /// and `recorded_at` (new on every attempt), `embedded` (storage state) and the request-level
    /// `metadata` (it annotates the request, not the message).
    pub fn same_occurrence(&self, other: &Episode) -> bool {
        let micros = |time: Option<DateTime<Utc>>| time.map(|t| t.timestamp_micros());
        self.role == other.role
            && self.speaker == other.speaker
            && self.content == other.content
            && micros(self.observed_at) == micros(other.observed_at)
    }
}

pub fn message_role_to_str(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

pub fn message_role_from_str(value: &str) -> Option<MessageRole> {
    match value {
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        "system" => Some(MessageRole::System),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn episode(content: &str) -> Episode {
        Episode {
            id: Uuid::new_v4(),
            org_id: "org".to_string(),
            user_id: "user".to_string(),
            session_id: None,
            message_id: None,
            speaker: None,
            role: MessageRole::User,
            content: content.to_string(),
            observed_at: None,
            recorded_at: Utc::now(),
            embedded: false,
            metadata: Value::Null,
        }
    }

    #[test]
    fn embeddable_threshold_counts_trimmed_characters() {
        assert!(!episode("Thanks!").is_embeddable());
        assert!(!episode("   nineteen chars..   ").is_embeddable());
        assert!(episode("twenty characters!!!").is_embeddable());
        // Characters, not bytes: 20 two-byte characters qualify, 19 do not.
        assert!(episode(&"é".repeat(20)).is_embeddable());
        assert!(!episode(&"é".repeat(19)).is_embeddable());
    }

    #[test]
    fn same_occurrence_compares_the_message_not_the_attempt() {
        let mut stored = episode("I keep three beehives.");
        stored.speaker = Some("Priya".to_string());
        stored.observed_at = Some(Utc::now());

        // A retry: new ID, receipt time, request metadata and sub-microsecond time noise.
        let mut retry = stored.clone();
        retry.id = Uuid::new_v4();
        retry.recorded_at = Utc::now();
        retry.embedded = true;
        retry.metadata = serde_json::json!({ "attempt": 2 });
        retry.observed_at = stored.observed_at.map(|t| {
            t.with_nanosecond(t.nanosecond() / 1_000 * 1_000 + 999)
                .unwrap()
        });
        assert!(stored.same_occurrence(&retry));

        let changes: [fn(&mut Episode); 6] = [
            |e| e.content = "I keep two beehives.".to_string(),
            |e| e.content = "I keep three beehives. ".to_string(),
            |e| e.role = MessageRole::Assistant,
            |e| e.speaker = None,
            |e| e.observed_at = None,
            |e| e.observed_at = e.observed_at.map(|t| t + chrono::Duration::microseconds(1)),
        ];
        for change in changes {
            let mut other = stored.clone();
            change(&mut other);
            assert!(!stored.same_occurrence(&other), "{other:?}");
        }
    }

    #[test]
    fn roles_round_trip() {
        for role in [
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::System,
        ] {
            assert_eq!(
                message_role_from_str(message_role_to_str(&role)),
                Some(role)
            );
        }
        assert_eq!(message_role_from_str("robot"), None);
    }
}
