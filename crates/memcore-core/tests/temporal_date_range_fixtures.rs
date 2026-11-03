//! An ambiguous day range such as "March 1-10"
//! must never become a fabricated signed year. An internal long-conversation evaluation: the lenient
//! layout `%B %d %Y` read "march 1-10" as 1 March of year -10 (chrono's `%Y` accepts a sign and a
//! layout space matches no space), and that event time was stored.
//!
//! Fixtures: the range itself and similar ranges; explicit dates with years; relative dates;
//! explicit signed (BCE) years where the format defines them; session timestamps versus event
//! dates. Everything except the range cases pins the behaviour from before the correction.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use memcore_core::temporal::{
    ModelTimestamp, TemporalOutcome, parse_lenient_timestamp, parse_model_timestamp_value,
    reconcile, resolve_expression,
};
use memcore_core::{TemporalGranularity, TemporalValue};

/// The session time of the c17 request ("12 March 2024").
fn session() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 3, 12, 0, 0, 0).unwrap()
}

fn day(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
}

// ------------------------------------------------------------------ ambiguous day ranges

#[test]
fn march_1_10_is_not_a_date_with_a_signed_year() {
    for text in [
        "March 1-10",
        "march 1-10",
        "Mar 1-10",
        "March 1 -10",
        "1 March -10",
    ] {
        assert_eq!(parse_lenient_timestamp(text), None, "{text}");
    }
    assert_eq!(resolve_expression("March 1-10", session()), None);
    assert_eq!(resolve_expression("on March 1-10", session()), None);
}

#[test]
fn other_ambiguous_day_ranges_yield_no_date() {
    for text in [
        "June 10-12",
        "Oct 3-5",
        "May 1-31",
        "March 1-10, 2024",
        "1-10 March",
        "between March 1 and 10",
    ] {
        assert_eq!(parse_lenient_timestamp(text), None, "{text}");
        assert_eq!(resolve_expression(text, session()), None, "{text}");
    }
}

#[test]
fn a_range_keeps_the_models_valid_time_or_stores_none_never_a_fabricated_year() {
    // No model time: the expression is kept as text only (no structured time).
    let (settled, _) = reconcile(None, Some("March 1-10"), session());
    assert_eq!(settled, None);

    // A valid model time is kept as it was before (LLM-only: the resolver does not cover it).
    let model = TemporalValue::new(
        day(2024, 3, 1),
        Utc.with_ymd_and_hms(2024, 3, 10, 23, 59, 59).unwrap(),
        TemporalGranularity::Day,
        session(),
    )
    .unwrap();
    let (settled, outcome) = reconcile(Some(&model), Some("March 1-10"), session());
    assert_eq!(outcome, TemporalOutcome::LlmOnly);
    assert_eq!(settled.unwrap().start.year(), 2024);

    // A model `valid_at` written as the range is unparsable, not year -10.
    let value = serde_json::json!("March 1-10");
    assert!(matches!(
        parse_model_timestamp_value(&value),
        ModelTimestamp::Unparsable(_)
    ));
}

// ------------------------------------------------------------------ unchanged formats

#[test]
fn explicit_dates_with_years_are_unchanged() {
    for (text, expected) in [
        ("2024-03-01", day(2024, 3, 1)),
        ("2024/03/01", day(2024, 3, 1)),
        ("1 March 2024", day(2024, 3, 1)),
        ("1 Mar 2024", day(2024, 3, 1)),
        ("March 1, 2024", day(2024, 3, 1)),
        ("Mar 1, 2024", day(2024, 3, 1)),
        ("March 1 2024", day(2024, 3, 1)),
        ("Mar 1 2024", day(2024, 3, 1)),
        ("01/03/2024", day(2024, 3, 1)),
        (
            "2024-03-01T09:30:00Z",
            Utc.with_ymd_and_hms(2024, 3, 1, 9, 30, 0).unwrap(),
        ),
        (
            "2024-03-01 09:30",
            Utc.with_ymd_and_hms(2024, 3, 1, 9, 30, 0).unwrap(),
        ),
    ] {
        assert_eq!(parse_lenient_timestamp(text), Some(expected), "{text}");
    }
    let resolved = resolve_expression("1 February 2024", session()).unwrap();
    assert_eq!(
        (resolved.start, resolved.granularity),
        (day(2024, 2, 1), TemporalGranularity::Day)
    );
}

#[test]
fn relative_dates_are_unchanged() {
    let yesterday = resolve_expression("yesterday", session()).unwrap();
    assert_eq!(yesterday.start, day(2024, 3, 11));
    let ago = resolve_expression("3 days ago", session()).unwrap();
    assert_eq!(ago.start, day(2024, 3, 9));
    let year = resolve_expression("last year", session()).unwrap();
    assert_eq!(
        (year.start.year(), year.granularity),
        (2023, TemporalGranularity::Year)
    );
}

#[test]
fn explicit_signed_years_where_the_year_leads_still_parse() {
    // ISO 8601 expanded years: the sign leads the whole value, so it is a year, not a range.
    assert_eq!(
        parse_lenient_timestamp("-0044-03-15"),
        Some(day(-44, 3, 15))
    );
    assert_eq!(
        parse_lenient_timestamp("-0044-03-15T12:00:00"),
        Some(Utc.with_ymd_and_hms(-44, 3, 15, 12, 0, 0).unwrap())
    );
    assert_eq!(
        parse_lenient_timestamp("+2024-03-01"),
        Some(day(2024, 3, 1))
    );
}

#[test]
fn session_timestamps_and_event_dates_stay_distinct() {
    // A session timestamp in the conversational form parses to its own instant.
    assert_eq!(
        parse_lenient_timestamp("4:15 pm on 12 March, 2024"),
        Some(Utc.with_ymd_and_hms(2024, 3, 12, 16, 15, 0).unwrap())
    );
    // An absolute event date resolves to itself whatever the session time.
    let event = resolve_expression("1 February 2024", day(2024, 1, 26)).unwrap();
    assert_eq!(event.start, day(2024, 2, 1));
    // A range never borrows the session's year or date to become a structured time.
    assert_eq!(resolve_expression("March 1-10", day(2024, 1, 26)), None);
}
