//! Making untrusted model output safe to put in a log line.
//!
//! Extraction diagnostics have to name the value that was rejected, otherwise a skip counter says
//! only that something went wrong. But a model's response is untrusted input: it can echo
//! conversation text into any field, be steered there by prompt injection, or simply be malformed.
//! Anything from it that reaches a log is user data leaving the process, often to a third-party log
//! sink.
//!
//! `sanitize_for_log` keeps what a diagnosis needs — the shape and the first few characters — and
//! drops the rest. It bounds the length, so a whole conversation cannot be written to the log
//! through a mislabelled field, and strips control characters, so a value cannot forge log
//! structure by embedding newlines.

/// The longest a sanitized value may be. A legitimate memory-type label is one word and a
/// legitimate timestamp is around 30 characters, so anything longer is already an anomaly and only
/// its beginning is worth recording.
pub(crate) const MAX_LOGGED_VALUE_CHARS: usize = 48;

/// Bounds and neutralizes an untrusted value for logging.
///
/// Control characters (including newline, carriage return and tab) become `.`, so the value cannot
/// break out of its field or inject a fake log record. Values longer than
/// `MAX_LOGGED_VALUE_CHARS` are cut and marked with the original character count, which keeps the
/// signal that something oversized arrived without reproducing it.
pub(crate) fn sanitize_for_log(value: &str) -> String {
    let mut out = String::with_capacity(MAX_LOGGED_VALUE_CHARS + 24);
    let mut taken = 0usize;
    let mut total = 0usize;

    for ch in value.chars() {
        total += 1;
        if taken < MAX_LOGGED_VALUE_CHARS {
            out.push(if ch.is_control() { '.' } else { ch });
            taken += 1;
        }
    }

    if total > MAX_LOGGED_VALUE_CHARS {
        out.push_str(&format!("…[truncated, {total} chars]"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ordinary_values_pass_through_unchanged() {
        for value in ["Knowledge", "2023-05-07", "4:15 pm on 12 March, 2024"] {
            assert_eq!(sanitize_for_log(value), value);
        }
    }

    #[test]
    fn long_values_are_cut_and_marked() {
        let long = "a".repeat(500);
        let sanitized = sanitize_for_log(&long);
        assert!(
            sanitized.starts_with(&"a".repeat(MAX_LOGGED_VALUE_CHARS)),
            "the beginning should survive for diagnosis"
        );
        assert!(sanitized.contains("truncated, 500 chars"));
        assert!(
            sanitized.chars().count() < 100,
            "a bounded value must stay bounded"
        );
    }

    /// A whole conversation turn arriving in a `memory_type` field must not end up in the log.
    #[test]
    fn a_long_sentence_of_user_content_does_not_reach_the_log_intact() {
        let content = "Priya mentioned that her mother's address is 14 Elmwood Terrace and that she \
             worries about the surgery scheduled for next Tuesday afternoon.";
        let sanitized = sanitize_for_log(content);
        assert!(!sanitized.contains("Elmwood"));
        assert!(!sanitized.contains("surgery"));
        assert!(sanitized.contains("truncated"));
    }

    #[test]
    fn control_characters_cannot_forge_log_structure() {
        let injected = "Knowledge\nERROR fake log line\r\ttab";
        let sanitized = sanitize_for_log(injected);
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\r'));
        assert!(!sanitized.contains('\t'));
        assert_eq!(sanitized, "Knowledge.ERROR fake log line..tab");
    }

    #[test]
    fn multibyte_values_are_counted_by_character_not_byte() {
        // Cutting on a byte boundary would panic or produce invalid UTF-8.
        let value = "日本語".repeat(100);
        let sanitized = sanitize_for_log(&value);
        assert!(sanitized.contains("truncated, 300 chars"));
        assert_eq!(
            sanitized.chars().take(MAX_LOGGED_VALUE_CHARS).count(),
            MAX_LOGGED_VALUE_CHARS
        );
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(sanitize_for_log(""), "");
    }
}
