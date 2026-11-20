mod pii;

pub use pii::PiiRedactor;

use crate::ports::MemoryMessage;

/// Redacts message bodies when PII redaction is enabled for LLM extraction.
pub fn redact_messages_for_extraction(messages: &[MemoryMessage]) -> Vec<MemoryMessage> {
    messages
        .iter()
        // Only the content is redacted. The speaker name, timestamp and message ID are kept: extraction
        // needs them to attribute, date and trace a fact.
        .map(|message| MemoryMessage {
            content: PiiRedactor::redact_text(&message.content),
            ..message.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::redact_messages_for_extraction;
    use crate::ports::{MemoryMessage, MessageRole};

    #[test]
    fn redaction_keeps_the_speaker_timestamp_and_message_id() {
        use chrono::TimeZone;
        let timestamp = chrono::Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap();
        let message = MemoryMessage {
            role: MessageRole::User,
            content: "Email me at someone@example.com".to_string(),
            name: Some("Priya".to_string()),
            timestamp: Some(timestamp),
            message_id: Some("D1:3".to_string()),
        };
        let redacted = redact_messages_for_extraction(&[message]);
        assert!(!redacted[0].content.contains("someone@example.com"));
        assert_eq!(redacted[0].name.as_deref(), Some("Priya"));
        assert_eq!(redacted[0].timestamp, Some(timestamp));
        assert_eq!(redacted[0].message_id.as_deref(), Some("D1:3"));
    }

    #[test]
    fn redact_messages_applies_to_user_content() {
        let messages = vec![MemoryMessage {
            role: MessageRole::User,
            content: "Reach me at user@example.com".to_string(),
            ..Default::default()
        }];
        let redacted = redact_messages_for_extraction(&messages);
        assert!(redacted[0].content.contains("[REDACTED_EMAIL]"));
    }
}
