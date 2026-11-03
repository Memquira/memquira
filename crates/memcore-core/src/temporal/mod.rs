//! Lenient parsing of the timestamps a language model writes.
//!
//! Measured this as the second of two bugs that cost memcore most of its extracted facts:
//! `valid_at` was deserialized as a strict `DateTime<Utc>`, so one fact carrying a date in any
//! other shape failed `serde` for the whole response and every sibling fact in that batch was lost
//! — 3.1% of batches with ISO metadata, and 8 of 18 in a controlled test using a conversational
//! timestamp format.
//!
//! The rule here is that a timestamp is a best-effort field: a value that cannot be understood
//! yields `None` and is counted, and the fact itself survives.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};

mod resolver;

pub use resolver::{
    TemporalOutcome, TemporalRejection, TemporalResolutionCounts, expression_occurs_in,
    expression_variants, is_duration, is_non_datable, is_recurrence, is_relative_expression,
    names_time, possessive_head, reconcile, resolve_absolute, resolve_expression,
    settle_event_time, validate_event_time,
};

/// Date-and-time layouts accepted in addition to RFC 3339, tried in order.
const NAIVE_DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M:%S",
    // Conversational forms, e.g. "4:15 pm on 12 March, 2024".
    "%I:%M %p on %d %B, %Y",
    "%I:%M %p on %d %b, %Y",
    "%I:%M %p on %d %B %Y",
    "%I:%M %p on %B %d, %Y",
    "%H:%M on %d %B, %Y",
    "%H:%M on %d %b, %Y",
];

/// Date-only layouts, resolved to midnight UTC.
const NAIVE_DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d",
    "%Y/%m/%d",
    "%d %B %Y",
    "%d %b %Y",
    "%B %d, %Y",
    "%b %d, %Y",
    "%B %d %Y",
    "%b %d %Y",
    "%d/%m/%Y",
];

/// Parses a timestamp a model wrote, returning `None` when no supported layout matches.
///
/// Accepts RFC 3339 with any offset, naive date-times (read as UTC), plain dates (midnight UTC),
/// and the conversational forms above. Callers must treat `None` as "no event time known", never
/// as a reason to discard the fact.
pub fn parse_lenient_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(parsed.with_timezone(&Utc));
    }

    // A trailing "UTC"/"Z" or a comma before the year is common in model output and is not part of
    // any chrono format string, so normalize it away before the table below.
    let normalized = trimmed
        .trim_end_matches(" UTC")
        .trim_end_matches(" utc")
        .trim();
    let signed_trailing_year = trailing_year_is_signed(normalized);

    for format in NAIVE_DATETIME_FORMATS {
        if signed_trailing_year && format.ends_with("%Y") {
            continue;
        }
        if let Ok(parsed) = NaiveDateTime::parse_from_str(normalized, format) {
            return Some(Utc.from_utc_datetime(&parsed));
        }
    }

    for format in NAIVE_DATE_FORMATS {
        if signed_trailing_year && format.ends_with("%Y") {
            continue;
        }
        if let Ok(parsed) = NaiveDate::parse_from_str(normalized, format) {
            return Some(Utc.from_utc_datetime(&parsed.and_hms_opt(0, 0, 0)?));
        }
    }

    None
}

/// Whether `text` ends in digits written with a sign ("march 1-10", "1 March -10").
///
/// In a layout whose year comes last, such a year is a day range or a typo, never a date: chrono's
/// `%Y` accepts a sign and a layout space also matches no space, so `%B %d %Y` would read
/// "march 1-10" as 1 March of year -10 (observed in an internal long-conversation evaluation). A signed year stays
/// valid where it leads the value (ISO 8601 "-0044-03-15", the `%Y-...` layouts).
fn trailing_year_is_signed(text: &str) -> bool {
    text.trim_end_matches(|c: char| c.is_ascii_digit())
        .ends_with(['-', '+'])
}

/// Parses a `valid_at` field straight out of model JSON.
///
/// Models emit this as a string, as `null`, occasionally as a Unix timestamp, and sometimes as
/// something unusable. Every shape is handled here so that deserialization of the enclosing
/// response can never fail on this field.
pub fn parse_model_timestamp_value(value: &serde_json::Value) -> ModelTimestamp {
    match value {
        serde_json::Value::Null => ModelTimestamp::Absent,
        serde_json::Value::String(text) if text.trim().is_empty() => ModelTimestamp::Absent,
        serde_json::Value::String(text) => match parse_lenient_timestamp(text) {
            Some(parsed) => ModelTimestamp::Parsed(parsed),
            None => ModelTimestamp::Unparsable(text.clone()),
        },
        serde_json::Value::Number(number) => number
            .as_i64()
            .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
            .map(ModelTimestamp::Parsed)
            .unwrap_or_else(|| ModelTimestamp::Unparsable(number.to_string())),
        other => ModelTimestamp::Unparsable(other.to_string()),
    }
}

/// What a `valid_at` field from a model turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelTimestamp {
    /// The model gave no time at all.
    Absent,
    /// A time was understood.
    Parsed(DateTime<Utc>),
    /// A time was given but no supported layout matched. The fact is kept without it.
    Unparsable(String),
}

impl ModelTimestamp {
    /// The resolved time, if there is one.
    pub fn parsed(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Parsed(value) => Some(*value),
            _ => None,
        }
    }

    /// Whether a value was supplied that could not be understood.
    pub fn is_unparsable(&self) -> bool {
        matches!(self, Self::Unparsable(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};
    use serde_json::json;

    fn parsed(value: &str) -> DateTime<Utc> {
        parse_lenient_timestamp(value).unwrap_or_else(|| panic!("{value} should parse"))
    }

    #[test]
    fn rfc_3339_with_any_offset_is_normalized_to_utc() {
        assert_eq!(
            parsed("2023-05-07T13:56:00Z"),
            Utc.with_ymd_and_hms(2023, 5, 7, 13, 56, 0).unwrap()
        );
        assert_eq!(
            parsed("2023-05-07T19:26:00+05:30"),
            Utc.with_ymd_and_hms(2023, 5, 7, 13, 56, 0).unwrap()
        );
    }

    #[test]
    fn naive_date_times_are_read_as_utc() {
        for value in [
            "2023-05-07T13:56:00",
            "2023-05-07 13:56:00",
            "2023-05-07T13:56",
            "2023-05-07 13:56:00 UTC",
        ] {
            assert_eq!(
                parsed(value),
                Utc.with_ymd_and_hms(2023, 5, 7, 13, 56, 0).unwrap(),
                "{value}"
            );
        }
    }

    #[test]
    fn plain_dates_resolve_to_midnight() {
        for value in [
            "2023-05-07",
            "2023/05/07",
            "7 May 2023",
            "May 7, 2023",
            "7 May 2023",
        ] {
            let result = parsed(value);
            assert_eq!(
                (result.year(), result.month(), result.day()),
                (2023, 5, 7),
                "{value}"
            );
            assert_eq!(result.hour(), 0, "{value}");
        }
    }

    /// The layout that failed 8 of 18 batches in the controlled test.
    #[test]
    fn conversational_timestamps_parse() {
        let result = parsed("4:15 pm on 12 March, 2024");
        assert_eq!(
            result,
            Utc.with_ymd_and_hms(2024, 3, 12, 16, 15, 0).unwrap()
        );
    }

    #[test]
    fn unsupported_text_yields_none_rather_than_an_error() {
        for value in ["last Saturday", "soon", "", "   ", "not a date"] {
            assert!(parse_lenient_timestamp(value).is_none(), "{value}");
        }
    }

    #[test]
    fn model_values_classify_into_absent_parsed_and_unparsable() {
        assert_eq!(
            parse_model_timestamp_value(&json!(null)),
            ModelTimestamp::Absent
        );
        assert_eq!(
            parse_model_timestamp_value(&json!("")),
            ModelTimestamp::Absent
        );
        assert_eq!(
            parse_model_timestamp_value(&json!("2023-05-07")).parsed(),
            Some(Utc.with_ymd_and_hms(2023, 5, 7, 0, 0, 0).unwrap())
        );
        assert!(parse_model_timestamp_value(&json!("last Saturday")).is_unparsable());
        assert!(parse_model_timestamp_value(&json!({"day": 7})).is_unparsable());
    }

    #[test]
    fn unix_seconds_are_accepted() {
        let expected = Utc.with_ymd_and_hms(2023, 5, 7, 13, 56, 0).unwrap();
        assert_eq!(
            parse_model_timestamp_value(&json!(expected.timestamp())).parsed(),
            Some(expected)
        );
    }
}
