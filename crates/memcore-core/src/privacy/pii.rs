use std::sync::OnceLock;

use regex::Regex;

const REDACTED_EMAIL: &str = "[REDACTED_EMAIL]";
const REDACTED_PHONE: &str = "[REDACTED_PHONE]";
const REDACTED_CARD: &str = "[REDACTED_CARD]";
const REDACTED_SECRET: &str = "[REDACTED_SECRET]";

fn bearer_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._=-]+").expect("bearer regex should compile")
    })
}

fn api_key_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:sk|pk|api)[-_][A-Za-z0-9]{16,}\b").expect("api key regex should compile")
    })
}

fn email_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")
            .expect("email regex should compile")
    })
}

fn phone_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:\+?1[-.\s]?)?(?:\(\d{3}\)|\d{3})[-.\s]?\d{3}[-.\s]?\d{4}\b")
            .expect("phone regex should compile")
    })
}

fn card_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:\d{4}[-\s]?){3}\d{4}\b").expect("card regex should compile")
    })
}

/// Regex-based PII redaction for user-provided text (foundation only).
pub struct PiiRedactor;

impl PiiRedactor {
    pub fn redact_text(input: &str) -> String {
        let mut text = input.to_string();
        text = bearer_regex()
            .replace_all(&text, format!("Bearer {REDACTED_SECRET}"))
            .into_owned();
        text = api_key_regex()
            .replace_all(&text, REDACTED_SECRET)
            .into_owned();
        text = email_regex()
            .replace_all(&text, REDACTED_EMAIL)
            .into_owned();
        text = phone_regex()
            .replace_all(&text, REDACTED_PHONE)
            .into_owned();
        text = card_regex().replace_all(&text, REDACTED_CARD).into_owned();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::PiiRedactor;

    #[test]
    fn email_redaction() {
        let out = PiiRedactor::redact_text("Contact me at alice@example.com please");
        assert!(!out.contains("alice@example.com"));
        assert!(out.contains("[REDACTED_EMAIL]"));
    }

    #[test]
    fn phone_redaction() {
        let out = PiiRedactor::redact_text("Call 555-123-4567 tomorrow");
        assert!(!out.contains("555-123-4567"));
        assert!(out.contains("[REDACTED_PHONE]"));
    }

    #[test]
    fn card_like_number_redaction() {
        let out = PiiRedactor::redact_text("Paid with 4111-1111-1111-1111");
        assert!(!out.contains("4111-1111-1111-1111"));
        assert!(out.contains("[REDACTED_CARD]"));
    }

    #[test]
    fn bearer_token_redaction() {
        let out = PiiRedactor::redact_text("Use Authorization: Bearer eyJhbGciOiJIUzI1NiJ9");
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
        assert!(out.contains("Bearer [REDACTED_SECRET]"));
    }

    #[test]
    fn api_key_like_token_redaction() {
        let out = PiiRedactor::redact_text("key is sk-abcdefghijklmnopqrstuvwxyz123456");
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
        assert!(out.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn normal_text_remains_mostly_unchanged() {
        let input = "I am learning Rust and building a memory engine.";
        assert_eq!(PiiRedactor::redact_text(input), input);
    }

    #[test]
    fn multiple_pii_values_in_one_text() {
        let out = PiiRedactor::redact_text(
            "Email bob@test.com phone 555-123-4567 card 4111-1111-1111-1111",
        );
        assert!(out.contains("[REDACTED_EMAIL]"));
        assert!(out.contains("[REDACTED_PHONE]"));
        assert!(out.contains("[REDACTED_CARD]"));
        assert!(!out.contains("bob@test.com"));
    }
}
