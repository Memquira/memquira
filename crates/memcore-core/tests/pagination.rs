use chrono::Utc;
use memcore_common::MemcoreError;
use memcore_core::{PageCursor, decode_cursor, encode_cursor, parse_optional_cursor};
use uuid::Uuid;

#[test]
fn encode_and_decode_cursor_round_trip() {
    let cursor = PageCursor {
        last_id: Uuid::new_v4().to_string(),
        last_sort_value: Utc::now(),
    };

    let encoded = encode_cursor(&cursor).expect("encode");
    let decoded = decode_cursor(&encoded).expect("decode");

    assert_eq!(decoded, cursor);
}

#[test]
fn invalid_cursor_returns_validation_error() {
    let error = decode_cursor("not-a-valid-cursor").unwrap_err();
    assert!(matches!(error, MemcoreError::ValidationError(message) if message == "invalid cursor"));
}

#[test]
fn empty_cursor_string_returns_validation_error() {
    let error = decode_cursor("   ").unwrap_err();
    assert!(matches!(error, MemcoreError::ValidationError(_)));
}

#[test]
fn parse_optional_cursor_treats_empty_as_none() {
    let parsed = parse_optional_cursor(Some(String::new())).expect("parse");
    assert!(parsed.is_none());
}

#[test]
fn parse_optional_cursor_decodes_valid_cursor() {
    let cursor = PageCursor {
        last_id: "user_123".to_string(),
        last_sort_value: Utc::now(),
    };
    let encoded = encode_cursor(&cursor).expect("encode");
    let parsed = parse_optional_cursor(Some(encoded)).expect("parse");
    assert_eq!(parsed, Some(cursor));
}
