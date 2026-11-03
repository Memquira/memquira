//! Deterministic resolution of common time expressions, used to cross-check the model.
//!
//! Structured extraction asks the model to resolve "last Saturday" against the time of the message
//! it appeared in. Models get this wrong in predictable ways — counting from the wrong day, or
//! from today instead of the message — so every event time the model returns is checked here.
//!
//! The resolver covers a fixed set of patterns and nothing else. For an expression it covers, its
//! answer is exact and wins over the model's. For anything outside that set it returns `None` and
//! the model's (validated) answer stands. [`reconcile`] applies that rule and reports which case
//! occurred, so disagreement rates are measurable (acceptance criterion P1-17).
//!
//! Weeks are ISO weeks (Monday to Sunday). "Last Saturday" is the most recent Saturday strictly
//! before the anchor day; "next Saturday" is the first one strictly after it.

use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::{TemporalGranularity, TemporalValue};

/// How far past the anchor an event time may lie before it is treated as a model error. Plans a
/// few years out are ordinary; decades out, in a conversation, almost always mean a wrong year.
const MAX_YEARS_AFTER_ANCHOR: i32 = 30;

/// Earliest plausible event year. Anything earlier is a parsing or model error.
const MIN_EVENT_YEAR: i32 = 1000;

/// Resolves `text` against `anchor` if it is one of the covered patterns.
pub fn resolve_expression(text: &str, anchor: DateTime<Utc>) -> Option<TemporalValue> {
    let head = possessive_head(text).unwrap_or(text);
    let normalized = strip_modifier(&strip_hedge(&expand_weekday_abbreviations(&normalize(
        head,
    ))));
    if normalized.is_empty() {
        return None;
    }
    let day = anchor.date_naive();

    let resolved = resolve_relative_day(&normalized, day)
        .or_else(|| resolve_named_weekday(&normalized, day))
        .or_else(|| resolve_relative_period(&normalized, day))
        .or_else(|| resolve_weekend(&normalized, day))
        .or_else(|| resolve_ago(&normalized, day))
        .or_else(|| resolve_vague_ago(&normalized, day))
        .or_else(|| resolve_season(&normalized, day))
        .or_else(|| resolve_month_year(&normalized))
        .or_else(|| resolve_year(&normalized))
        .or_else(|| resolve_explicit_date(&normalized, head));

    let (start, end, granularity) = resolved?;
    TemporalValue::new(start_of(start), end_of(end), granularity, anchor).ok()
}

/// The time part of a possessive expression: "last week's" or "last week's game" -> "last week".
pub fn possessive_head(text: &str) -> Option<&str> {
    ["'s", "\u{2019}s"].iter().find_map(|marker| {
        let index = text.find(marker)?;
        let after = &text[index + marker.len()..];
        after
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
            .then(|| text[..index].trim())
            .filter(|head| !head.is_empty())
    })
}

/// Why a model's event time was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalRejection {
    TooEarly,
    TooFarAfterAnchor,
}

/// Rejects event times that cannot be right: before year 1000, or more than 30 years after the
/// message they were resolved against. (A span ending before it starts is already impossible to
/// construct.)
pub fn validate_event_time(value: &TemporalValue) -> Result<(), TemporalRejection> {
    if value.start.year() < MIN_EVENT_YEAR {
        return Err(TemporalRejection::TooEarly);
    }
    let limit = value
        .anchor
        .checked_add_months(Months::new((MAX_YEARS_AFTER_ANCHOR as u32) * 12))
        .unwrap_or(value.anchor);
    if value.start > limit {
        return Err(TemporalRejection::TooFarAfterAnchor);
    }
    Ok(())
}

/// Which case applied when settling a fact's event time. One is recorded per structured fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalOutcome {
    /// Model and resolver produced the same span.
    Agree,
    /// They differed; the resolver's answer was used because it fully covers the expression.
    DeterministicOverride,
    /// Only the model produced a time (the expression is outside the resolver's patterns).
    LlmOnly,
    /// Only the resolver produced a time (the model gave none).
    DeterministicOnly,
    /// The model's time was impossible and the resolver could not replace it; no time stored.
    Rejected,
    /// No time expression and no time.
    None,
    /// The model gave a time, but the conversation contains no expression it came from — in
    /// practice the message's own date stamped onto a timeless fact ("Priya likes gardening").
    /// It is not an event time: the fact keeps `observed_at` only (spec §1.4 fallback).
    Ungrounded,
    /// The expression cannot be placed on a calendar from the conversation: vague recency
    /// ("recently", "the other day"), an age or life stage ("when I was 10"), a frequency ("every
    /// week"), a time relative to another event ("after the concert"), or a pure duration ("for
    /// five years"). Whatever time the model gave is not stored; the fact keeps `observed_at`.
    NonDatable,
    /// The expression is relative ("yesterday", "two years ago", "this summer") and the request
    /// carried no client message or session time to resolve it against (P2-20). The event time is
    /// unknown: the time memcore received the request is never used in its place. The sentence and
    /// the expression are kept as written.
    Unanchored,
}

impl TemporalOutcome {
    pub const ALL: [Self; 9] = [
        Self::Agree,
        Self::DeterministicOverride,
        Self::LlmOnly,
        Self::DeterministicOnly,
        Self::Rejected,
        Self::None,
        Self::Ungrounded,
        Self::NonDatable,
        Self::Unanchored,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agree => "agree",
            Self::DeterministicOverride => "deterministic_override",
            Self::LlmOnly => "llm_only",
            Self::DeterministicOnly => "deterministic_only",
            Self::Rejected => "rejected",
            Self::None => "none",
            Self::Ungrounded => "ungrounded",
            Self::NonDatable => "non_datable",
            Self::Unanchored => "unanchored",
        }
    }
}

/// Per-outcome tally for one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalResolutionCounts {
    pub agree: usize,
    pub deterministic_override: usize,
    pub llm_only: usize,
    pub deterministic_only: usize,
    pub rejected: usize,
    pub none: usize,
    #[serde(default)]
    pub ungrounded: usize,
    #[serde(default)]
    pub non_datable: usize,
    #[serde(default)]
    pub unanchored: usize,
}

impl TemporalResolutionCounts {
    pub fn record(&mut self, outcome: TemporalOutcome) {
        match outcome {
            TemporalOutcome::Agree => self.agree += 1,
            TemporalOutcome::DeterministicOverride => self.deterministic_override += 1,
            TemporalOutcome::LlmOnly => self.llm_only += 1,
            TemporalOutcome::DeterministicOnly => self.deterministic_only += 1,
            TemporalOutcome::Rejected => self.rejected += 1,
            TemporalOutcome::None => self.none += 1,
            TemporalOutcome::Ungrounded => self.ungrounded += 1,
            TemporalOutcome::NonDatable => self.non_datable += 1,
            TemporalOutcome::Unanchored => self.unanchored += 1,
        }
    }

    pub fn get(&self, outcome: TemporalOutcome) -> usize {
        match outcome {
            TemporalOutcome::Agree => self.agree,
            TemporalOutcome::DeterministicOverride => self.deterministic_override,
            TemporalOutcome::LlmOnly => self.llm_only,
            TemporalOutcome::DeterministicOnly => self.deterministic_only,
            TemporalOutcome::Rejected => self.rejected,
            TemporalOutcome::None => self.none,
            TemporalOutcome::Ungrounded => self.ungrounded,
            TemporalOutcome::NonDatable => self.non_datable,
            TemporalOutcome::Unanchored => self.unanchored,
        }
    }

    pub fn total(&self) -> usize {
        TemporalOutcome::ALL
            .iter()
            .map(|outcome| self.get(*outcome))
            .sum()
    }
}

/// Settles a fact's event time from the model's answer and the original expression.
///
/// Rule: a model value with no time expression behind it is not an event time (`Ungrounded`); for an
/// expression the resolver covers, the resolver's value is used; an expression it does not cover
/// that cannot be dated from the conversation yields no time (`NonDatable`); an impossible model
/// value is discarded; otherwise the model's value is used. The returned value always carries
/// `anchor`.
pub fn reconcile(
    model_value: Option<&TemporalValue>,
    text: Option<&str>,
    anchor: DateTime<Utc>,
) -> (Option<TemporalValue>, TemporalOutcome) {
    let text = text.map(str::trim).filter(|text| !text.is_empty());
    if text.is_none() && model_value.is_some() {
        return (None, TemporalOutcome::Ungrounded);
    }
    let deterministic = text.and_then(|text| resolve_expression(text, anchor));
    if deterministic.is_none() && text.is_some_and(is_non_datable) {
        return (None, TemporalOutcome::NonDatable);
    }
    let (model_valid, model_rejected) = match model_value {
        Some(value) => {
            let anchored = TemporalValue {
                anchor,
                ..value.clone()
            };
            match validate_event_time(&anchored) {
                Ok(()) => (Some(anchored), false),
                Err(_) => (None, true),
            }
        }
        None => (None, false),
    };

    match (model_valid, deterministic) {
        (Some(model), Some(exact)) => {
            if same_span(&model, &exact) {
                (Some(exact), TemporalOutcome::Agree)
            } else {
                (Some(exact), TemporalOutcome::DeterministicOverride)
            }
        }
        (Some(model), None) => (Some(model), TemporalOutcome::LlmOnly),
        (None, Some(exact)) if model_rejected => {
            (Some(exact), TemporalOutcome::DeterministicOverride)
        }
        (None, Some(exact)) => (Some(exact), TemporalOutcome::DeterministicOnly),
        (None, None) if model_rejected => (None, TemporalOutcome::Rejected),
        (None, None) => (None, TemporalOutcome::None),
    }
}

/// Settles a fact's event time when the anchor may be missing (P2-20).
///
/// With a client-supplied anchor (the time of the message the fact cites, or the request's session
/// time) this is exactly [`reconcile`]. Without one, nothing may be resolved relative to "now", so:
///
/// - no expression: a model time is `Ungrounded`, as with an anchor;
/// - a non-datable expression stays `NonDatable`;
/// - an **absolute** expression the resolver covers ("8 May 2023", "May 2023", "2019", "summer
///   2022") is resolved: its value is the same whatever the anchor (see [`resolve_absolute`]);
/// - an expression the resolver does not cover keeps the model's value only if the expression names
///   a year and the model's span lies within the years it names;
/// - anything else is relative and becomes `Unanchored`: no event time.
///
/// A value settled without an anchor carries its own start as `anchor`, since nothing was resolved
/// against a message time.
pub fn settle_event_time(
    model_value: Option<&TemporalValue>,
    text: Option<&str>,
    anchor: Option<DateTime<Utc>>,
) -> (Option<TemporalValue>, TemporalOutcome) {
    if let Some(anchor) = anchor {
        return reconcile(model_value, text, anchor);
    }
    let text = text.map(str::trim).filter(|text| !text.is_empty());
    let Some(text) = text else {
        return if model_value.is_some() {
            (None, TemporalOutcome::Ungrounded)
        } else {
            (None, TemporalOutcome::None)
        };
    };
    if let Some(exact) = resolve_absolute(text) {
        let outcome = match model_value {
            Some(model) if same_span(model, &exact) => TemporalOutcome::Agree,
            Some(_) => TemporalOutcome::DeterministicOverride,
            None => TemporalOutcome::DeterministicOnly,
        };
        return (Some(exact), outcome);
    }
    if is_non_datable(text) {
        return (None, TemporalOutcome::NonDatable);
    }
    if let Some(model) = model_value
        && within_named_years(model, text)
    {
        let own = TemporalValue {
            anchor: model.start,
            ..model.clone()
        };
        if validate_event_time(&own).is_ok() {
            return (Some(own), TemporalOutcome::LlmOnly);
        }
    }
    (None, TemporalOutcome::Unanchored)
}

/// Resolves `text` only if it names a fixed point in time: the resolver gives the same span against
/// two anchors decades apart. "8 May 2023" and "summer 2022" qualify; "yesterday", "last summer" and
/// "two years ago" do not. The returned value's anchor is its own start.
pub fn resolve_absolute(text: &str) -> Option<TemporalValue> {
    let early = Utc.with_ymd_and_hms(1990, 3, 15, 12, 0, 0).single()?;
    let late = Utc.with_ymd_and_hms(2090, 9, 15, 12, 0, 0).single()?;
    let a = resolve_expression(text, early)?;
    let b = resolve_expression(text, late)?;
    if !same_span(&a, &b) {
        return None;
    }
    Some(TemporalValue {
        anchor: a.start,
        ..a
    })
}

/// Whether `text` names at least one year and the model's span lies within the years it names.
fn within_named_years(model: &TemporalValue, text: &str) -> bool {
    let years: Vec<i32> = words_of(text)
        .iter()
        .filter_map(|word| parse_year(word))
        .collect();
    let (Some(first), Some(last)) = (years.iter().min(), years.iter().max()) else {
        return false;
    };
    let start = model.start.year();
    let end = model.end.year();
    *first <= start && end <= *last
}

/// Two spans agree when they cover the same calendar days.
fn same_span(a: &TemporalValue, b: &TemporalValue) -> bool {
    a.start.date_naive() == b.start.date_naive() && a.end.date_naive() == b.end.date_naive()
}

// ------------------------------------------------------------------------------------ patterns

type Resolved = (NaiveDate, NaiveDate, TemporalGranularity);

fn normalize(text: &str) -> String {
    let lowered = text.trim().to_ascii_lowercase();
    let trimmed = lowered.trim_end_matches(['.', ',', '!', '?', ';', ':']);
    let mut words: Vec<&str> = trimmed.split_whitespace().collect();
    while let Some(first) = words.first() {
        if matches!(*first, "on" | "in" | "during" | "at" | "since") {
            words.remove(0);
        } else {
            break;
        }
    }
    words.join(" ")
}

/// Words that soften an expression without changing what it points at: "about five years ago" is
/// resolved as "five years ago". Only hedges that keep the same best guess are listed; "over a year
/// ago" or "almost a year ago" are left to the model.
const HEDGES: [&str; 4] = ["about", "around", "roughly", "approximately"];

fn strip_hedge(text: &str) -> String {
    for hedge in HEDGES {
        if let Some(rest) = text
            .strip_prefix(hedge)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            return rest.to_string();
        }
    }
    text.to_string()
}

/// "last fri" -> "last friday". Applied word by word to lower-case text.
fn expand_weekday_abbreviations(text: &str) -> String {
    text.split(' ')
        .map(|word| match word {
            "mon" => "monday",
            "tue" | "tues" => "tuesday",
            "wed" | "weds" => "wednesday",
            "thu" | "thur" | "thurs" => "thursday",
            "fri" => "friday",
            "sat" => "saturday",
            "sun" => "sunday",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lower-case words of `text`, weekday abbreviations expanded.
fn words_of(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let spaced: String = lowered
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    expand_weekday_abbreviations(&spaced.split_whitespace().collect::<Vec<_>>().join(" "))
        .split(' ')
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `expression` occurs in `text` as a run of whole words, ignoring case, punctuation, a
/// leading hedge ("about") and weekday abbreviations ("Fri"). Used to check that a time the model
/// reports really comes from the message it cites.
pub fn expression_occurs_in(expression: &str, text: &str) -> bool {
    let haystack = words_of(text);
    let full = words_of(expression);
    let unhedged = words_of(&strip_hedge(&full.join(" ")));
    [full, unhedged].iter().any(|needle| {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle.as_slice())
    })
}

/// The ways a time expression may be written in a sentence: as given, with weekday abbreviations
/// expanded, and without a leading hedge. Lower case, most specific first, without duplicates.
pub fn expression_variants(expression: &str) -> Vec<String> {
    let given = expression.trim().to_lowercase();
    let expanded = expand_weekday_abbreviations(&given);
    let mut variants = Vec::new();
    for variant in [
        given.clone(),
        expanded.clone(),
        strip_hedge(&given),
        strip_hedge(&expanded),
    ] {
        if !variant.is_empty() && !variants.contains(&variant) {
            variants.push(variant);
        }
    }
    variants
}

fn single_day(day: NaiveDate) -> Resolved {
    (day, day, TemporalGranularity::Day)
}

fn resolve_relative_day(text: &str, day: NaiveDate) -> Option<Resolved> {
    let offset = match text {
        "today" | "this morning" | "this afternoon" | "this evening" | "tonight" => 0,
        "yesterday"
        | "last night"
        | "yesterday morning"
        | "yesterday afternoon"
        | "yesterday evening" => -1,
        "the day before yesterday" => -2,
        "tomorrow" | "tomorrow morning" | "tomorrow evening" => 1,
        "the day after tomorrow" => 2,
        _ => return None,
    };
    Some(single_day(day + Duration::days(offset)))
}

fn parse_weekday(word: &str) -> Option<Weekday> {
    Some(match word {
        "monday" => Weekday::Mon,
        "tuesday" => Weekday::Tue,
        "wednesday" => Weekday::Wed,
        "thursday" => Weekday::Thu,
        "friday" => Weekday::Fri,
        "saturday" => Weekday::Sat,
        "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn resolve_named_weekday(text: &str, day: NaiveDate) -> Option<Resolved> {
    let (direction, name) = if let Some(rest) = text.strip_prefix("last ") {
        (-1, rest)
    } else if let Some(rest) = text.strip_prefix("this past ") {
        (-1, rest)
    } else if let Some(rest) = text.strip_prefix("next ") {
        (1, rest)
    } else {
        return None;
    };
    let target = parse_weekday(name)?;
    let current = day.weekday().num_days_from_monday() as i64;
    let wanted = target.num_days_from_monday() as i64;
    let offset = if direction < 0 {
        // strictly before the anchor day: 1..=7 days back
        let back = (current - wanted).rem_euclid(7);
        -(if back == 0 { 7 } else { back })
    } else {
        let forward = (wanted - current).rem_euclid(7);
        if forward == 0 { 7 } else { forward }
    };
    Some(single_day(day + Duration::days(offset)))
}

fn week_of(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    let monday = day - Duration::days(day.weekday().num_days_from_monday() as i64);
    (monday, monday + Duration::days(6))
}

fn month_of(year: i32, month: u32) -> Option<(NaiveDate, NaiveDate)> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = first.checked_add_months(Months::new(1))?;
    Some((first, next - Duration::days(1)))
}

fn year_of(year: i32) -> Option<(NaiveDate, NaiveDate)> {
    Some((
        NaiveDate::from_ymd_opt(year, 1, 1)?,
        NaiveDate::from_ymd_opt(year, 12, 31)?,
    ))
}

/// "last weekend" / "this past weekend" is the Saturday and Sunday before the anchor's week, also
/// when the anchor is itself a weekend day ("last weekend", said on a Sunday, means the one before).
/// "two weekends ago" goes one more week back per count. "next weekend" is ambiguous and left to
/// the model.
fn resolve_weekend(text: &str, day: NaiveDate) -> Option<Resolved> {
    let weeks_back = match text {
        "last weekend" | "this past weekend" | "the past weekend" => 1,
        _ => {
            let rest = text
                .strip_suffix(" weekends ago")
                .or_else(|| text.strip_suffix(" weekend ago"))?;
            parse_count(rest)?
        }
    };
    let (monday, _) = week_of(day);
    let saturday = monday - Duration::days(7 * (weeks_back - 1) + 2);
    Some((
        saturday,
        saturday + Duration::days(1),
        TemporalGranularity::Approximate,
    ))
}

fn resolve_relative_period(text: &str, day: NaiveDate) -> Option<Resolved> {
    let (shift, unit) = if let Some(unit) = text.strip_prefix("last ") {
        (-1, unit)
    } else if let Some(unit) = text.strip_prefix("this ") {
        (0, unit)
    } else if let Some(unit) = text.strip_prefix("next ") {
        (1, unit)
    } else {
        return None;
    };
    match unit {
        "week" => {
            let (start, end) = week_of(day + Duration::days(7 * shift));
            Some((start, end, TemporalGranularity::Week))
        }
        "month" => {
            let first = NaiveDate::from_ymd_opt(day.year(), day.month(), 1)?;
            let shifted = shift_months(first, shift)?;
            let (start, end) = month_of(shifted.year(), shifted.month())?;
            Some((start, end, TemporalGranularity::Month))
        }
        "year" => {
            let (start, end) = year_of(day.year() + shift as i32)?;
            Some((start, end, TemporalGranularity::Year))
        }
        _ => None,
    }
}

fn shift_months(date: NaiveDate, months: i64) -> Option<NaiveDate> {
    if months >= 0 {
        date.checked_add_months(Months::new(months as u32))
    } else {
        date.checked_sub_months(Months::new((-months) as u32))
    }
}

fn parse_count(word: &str) -> Option<i64> {
    if let Ok(value) = word.parse::<i64>() {
        return (1..=100).contains(&value).then_some(value);
    }
    Some(match word {
        "a" | "an" | "one" => 1,
        "couple" => 2,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        _ => return None,
    })
}

fn resolve_ago(text: &str, day: NaiveDate) -> Option<Resolved> {
    let rest = text.strip_suffix(" ago")?;
    // "a couple of weeks ago" -> "couple weeks ago"
    let couple;
    let rest = match rest
        .strip_prefix("a couple of ")
        .or_else(|| rest.strip_prefix("couple of "))
    {
        Some(unit) => {
            couple = format!("couple {unit}");
            couple.as_str()
        }
        None => rest,
    };
    let mut parts = rest.split_whitespace();
    let count = parse_count(parts.next()?)?;
    let unit = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    match unit.trim_end_matches('s') {
        "day" => Some(single_day(day - Duration::days(count))),
        "week" => {
            let (start, end) = week_of(day - Duration::days(7 * count));
            Some((start, end, TemporalGranularity::Week))
        }
        "month" => {
            let first = NaiveDate::from_ymd_opt(day.year(), day.month(), 1)?;
            let shifted = shift_months(first, -count)?;
            let (start, end) = month_of(shifted.year(), shifted.month())?;
            Some((start, end, TemporalGranularity::Month))
        }
        "year" => {
            let (start, end) = year_of(day.year() - count as i32)?;
            Some((start, end, TemporalGranularity::Year))
        }
        _ => None,
    }
}

fn parse_month(word: &str) -> Option<u32> {
    Some(match word {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

fn parse_year(word: &str) -> Option<i32> {
    let year = word.parse::<i32>().ok()?;
    (1900..=2200).contains(&year).then_some(year)
}

fn resolve_month_year(text: &str) -> Option<Resolved> {
    let cleaned = text.replace(',', " ");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let (month, year) = match words.as_slice() {
        [month, year] => (parse_month(month)?, parse_year(year)?),
        [month, "of", year] => (parse_month(month)?, parse_year(year)?),
        _ => return None,
    };
    let (start, end) = month_of(year, month)?;
    Some((start, end, TemporalGranularity::Month))
}

fn resolve_year(text: &str) -> Option<Resolved> {
    let year = parse_year(text)?;
    let (start, end) = year_of(year)?;
    Some((start, end, TemporalGranularity::Year))
}

fn resolve_explicit_date(normalized: &str, original: &str) -> Option<Resolved> {
    let parsed = super::parse_lenient_timestamp(normalized)
        .or_else(|| super::parse_lenient_timestamp(original))?;
    Some(single_day(parsed.date_naive()))
}

/// Words that narrow a period without moving it: "early June 2023" is resolved as June 2023 and
/// "earlier this week" as this week, each at the period's own precision.
const MODIFIERS: [&str; 13] = [
    "sometime",
    "some time",
    "earlier",
    "later",
    "early",
    "late",
    "mid",
    "the start of",
    "the beginning of",
    "the end of",
    "the middle of",
    "the first half of",
    "the second half of",
];

fn strip_modifier(text: &str) -> String {
    for modifier in MODIFIERS {
        if let Some(rest) = text
            .strip_prefix(modifier)
            .and_then(|rest| rest.strip_prefix([' ', '-']))
        {
            return rest.trim().to_string();
        }
    }
    text.to_string()
}

/// "a few days ago" / "several weeks ago": an approximate span 2-4 ("few") or 3-6 ("several") units
/// back, in whole weeks, months or years for those units.
fn resolve_vague_ago(text: &str, day: NaiveDate) -> Option<Resolved> {
    let rest = text.strip_suffix(" ago")?;
    let rest = rest.strip_prefix("a ").unwrap_or(rest);
    let (near, far, unit) = if let Some(unit) = rest.strip_prefix("few ") {
        (2, 4, unit)
    } else if let Some(unit) = rest.strip_prefix("several ") {
        (3, 6, unit)
    } else {
        return None;
    };
    let (start, end) = match unit {
        "days" => (day - Duration::days(far), day - Duration::days(near)),
        "weeks" => (
            week_of(day - Duration::days(7 * far)).0,
            week_of(day - Duration::days(7 * near)).1,
        ),
        "months" => {
            let first = NaiveDate::from_ymd_opt(day.year(), day.month(), 1)?;
            let start = shift_months(first, -far)?;
            let last = shift_months(first, -near)?;
            (start, month_of(last.year(), last.month())?.1)
        }
        "years" => (
            year_of(day.year() - far as i32)?.0,
            year_of(day.year() - near as i32)?.1,
        ),
        _ => return None,
    };
    Some((start, end, TemporalGranularity::Approximate))
}

/// Meteorological seasons, northern hemisphere: spring March-May, summer June-August, autumn
/// September-November, winter December-February (named by the year of its December).
fn season_span(season: &str, year: i32) -> Option<(NaiveDate, NaiveDate)> {
    let first_month = match season {
        "spring" => 3,
        "summer" => 6,
        "autumn" | "fall" => 9,
        "winter" => 12,
        _ => return None,
    };
    let start = NaiveDate::from_ymd_opt(year, first_month, 1)?;
    let end = start.checked_add_months(Months::new(3))? - Duration::days(1);
    Some((start, end))
}

/// "last summer", "this past winter", "this spring", "summer 2022", "the summer of 2022". "Last" is
/// the most recent one that ended before the anchor day; "this" is the one containing the anchor,
/// else the one in the anchor's year. "Next summer" is ambiguous and left to the model, as is a
/// winter with a year ("winter 2023"), which spans two years.
fn resolve_season(text: &str, day: NaiveDate) -> Option<Resolved> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (start, end) = match words.as_slice() {
        ["last" | "past", season] | ["this" | "the", "past", season] => (day.year() - 2
            ..=day.year())
            .rev()
            .filter_map(|year| season_span(season, year))
            .find(|(_, end)| *end < day)?,
        ["this", season] => [day.year() - 1, day.year()]
            .iter()
            .filter_map(|year| season_span(season, *year))
            .find(|(start, end)| *start <= day && day <= *end)
            .or_else(|| season_span(season, day.year()))?,
        [season, year] | ["the", season, "of", year] | [season, "of", year]
            if *season != "winter" =>
        {
            season_span(season, parse_year(year)?)?
        }
        _ => return None,
    };
    Some((start, end, TemporalGranularity::Approximate))
}

/// A duration ("for 4 years", "two hours", "seven years now") has a length but no position in time,
/// unlike a moment ("two years ago", "May 2023"): it has a length unit and none of the words that
/// turn a length into a moment.
pub fn is_duration(expression: &str) -> bool {
    const UNITS: [&str; 14] = [
        "minute", "minutes", "hour", "hours", "day", "days", "week", "weeks", "month", "months",
        "year", "years", "decade", "decades",
    ];
    const MOMENT: [&str; 10] = [
        "ago", "last", "next", "this", "past", "since", "in", "on", "before", "after",
    ];
    let words = words_of(expression);
    words.iter().any(|word| UNITS.contains(&word.as_str()))
        && !words.iter().any(|word| MOMENT.contains(&word.as_str()))
}

/// Phrases that never place an event on a calendar by themselves.
const NON_DATABLE_PHRASES: &[&str] = &[
    // vague recency or future
    "recently",
    "recent",
    "lately",
    "the other day",
    "a while",
    "some time",
    "sometime",
    "not long ago",
    "a long time",
    "long time",
    "since we last",
    "since i last",
    "last time",
    "these days",
    "nowadays",
    "already",
    "earlier",
    "before",
    "back then",
    "in the past",
    "once",
    "at some point",
    "one day",
    "someday",
    "some day",
    "soon",
    "eventually",
    "in the future",
    "later",
    "ever since",
    // sports or TV seasons, not calendar seasons
    "last season",
    "this season",
    "next season",
    "the season",
    // age and life stage
    "when i was",
    "when we were",
    "when he was",
    "when she was",
    "when they were",
    "as a kid",
    "as kids",
    "as a child",
    "as children",
    "as a teen",
    "as a teenager",
    "growing up",
    "years old",
    "year old",
    "at age",
    "at the age",
    "in high school",
    "in college",
    "in middle school",
    "in school",
    "or so",
    "childhood",
    // frequency
    "every",
    "each",
    "daily",
    "weekly",
    "monthly",
    "yearly",
    "annually",
    "always",
    "usually",
    "often",
    "sometimes",
    "regularly",
    "weekends",
    "twice",
    "times a",
    "per week",
    "per month",
    "mondays",
    "tuesdays",
    "wednesdays",
    "thursdays",
    "fridays",
    "saturdays",
    "sundays",
];

/// Words that start an expression relative to another event ("after the concert").
const EVENT_RELATIVE_STARTS: [&str; 9] = [
    "after", "before", "during", "when", "while", "until", "till", "since", "upon",
];

/// Whether an expression the resolver does not cover cannot be dated from the conversation. An
/// expression that names a calendar anchor itself (a month, weekday, year or relative day) is never
/// non-datable: the model's reading of it is kept and validated as before.
pub fn is_non_datable(expression: &str) -> bool {
    let words = words_of(expression);
    if words.is_empty() {
        return false;
    }
    let has_anchor = words.iter().enumerate().any(|(index, word)| {
        // "may" is a month when it ends the expression or a number follows it ("May 5", "May
        // 2023"); otherwise it is the verb ("we may go").
        let month = parse_month(word).is_some()
            && (word != "may"
                || words
                    .get(index + 1)
                    .is_none_or(|next| next.parse::<u32>().is_ok()));
        month
            || parse_weekday(word).is_some()
            || parse_year(word).is_some()
            || matches!(
                word.as_str(),
                "today" | "yesterday" | "tomorrow" | "tonight"
            )
    });
    if has_anchor {
        return false;
    }
    if is_duration(expression) {
        return true;
    }
    // A bare small number is an age ("17"), never a date.
    if words.len() == 1 && words[0].parse::<u32>().is_ok_and(|n| n <= 120) {
        return true;
    }
    if EVENT_RELATIVE_STARTS.contains(&words[0].as_str()) {
        return true;
    }
    let joined = format!(" {} ", words.join(" "));
    NON_DATABLE_PHRASES
        .iter()
        .any(|phrase| joined.contains(&format!(" {phrase} ")))
}

/// Words that only ever appear in a time expression, or that make a phrase one.
const TIME_WORDS: &[&str] = &[
    "today",
    "yesterday",
    "tomorrow",
    "tonight",
    "ago",
    "now",
    "morning",
    "mornings",
    "afternoon",
    "evening",
    "evenings",
    "night",
    "nights",
    "noon",
    "midnight",
    "weekend",
    "weekends",
    "day",
    "days",
    "week",
    "weeks",
    "fortnight",
    "month",
    "months",
    "year",
    "years",
    "decade",
    "decades",
    "century",
    "hour",
    "hours",
    "minute",
    "minutes",
    "season",
    "spring",
    "summer",
    "autumn",
    "fall",
    "winter",
    "christmas",
    "easter",
    "thanksgiving",
    "holiday",
    "holidays",
    "quarter",
    "semester",
    "term",
    "am",
    "pm",
    "daily",
    "weekly",
    "monthly",
    "yearly",
    "annually",
];

/// Whether `expression` could be a time expression at all. The model sometimes
/// reports a span with no time in it as the fact's time expression ("in Room 4B" for a kickoff
/// whose date the conversation never gave); such a span is not grounded in any time, so the time
/// the model attached to it is not an event time. True for anything that names a calendar unit, a
/// month, weekday, year, relative day, season or time of day, a written date ("12/05", "3rd",
/// "9:30"), or a phrase the resolver or [`is_non_datable`] recognises; false only when none of
/// these appears.
pub fn names_time(expression: &str) -> bool {
    let words = words_of(expression);
    if words.iter().any(|word| {
        parse_month(word).is_some()
            || parse_weekday(word).is_some()
            || parse_year(word).is_some()
            || TIME_WORDS.contains(&word.as_str())
            || is_ordinal(word)
            || is_clock_time(word)
    }) {
        return true;
    }
    let numeric_date = expression.split_whitespace().any(|token| {
        let parts: Vec<&str> = token
            .trim_matches(|c: char| !c.is_ascii_digit())
            .split(['/', '-', '.', ':'])
            .collect();
        parts.len() >= 2
            && parts.iter().all(|part| {
                !part.is_empty() && part.len() <= 4 && part.chars().all(|c| c.is_ascii_digit())
            })
    });
    numeric_date || is_non_datable(expression) || resolve_absolute(expression).is_some()
}

/// "3rd", "21st": a day of the month in speech.
fn is_ordinal(word: &str) -> bool {
    let digits = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &word[digits.len()..];
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && matches!(suffix, "st" | "nd" | "rd" | "th")
}

/// "9am", "5pm": a time of day written as one word.
fn is_clock_time(word: &str) -> bool {
    ["am", "pm"].iter().any(|suffix| {
        word.strip_suffix(suffix)
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Whether a time expression describes a repeating time ("every Sunday", "the last
/// Sunday of each month", "twice a week") rather than one point in time. [`is_non_datable`] keeps
/// any expression that names a weekday or month datable; a recurrence that does is still not a
/// date.
pub fn is_recurrence(expression: &str) -> bool {
    let words = words_of(expression);
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "every" | "each" | "daily" | "weekly" | "monthly" | "yearly" | "annually"
        )
    })
}

/// Words that make an expression's meaning depend on when it was said.
const RELATIVE_WORDS: &[&str] = &[
    "ago",
    "last",
    "next",
    "this",
    "yesterday",
    "today",
    "tomorrow",
    "tonight",
    "past",
    "previous",
    "coming",
    "following",
    "upcoming",
];

/// Whether a time expression is relative to when it was said ("next Friday", "two
/// weeks ago", "on Saturday") rather than a written date ("14 March", "May 2023"). A relative
/// expression becomes wrong once the conversation is in the past, so the stored sentence must not
/// keep it as written; a written date stays true.
pub fn is_relative_expression(expression: &str) -> bool {
    let words = words_of(expression);
    if words
        .iter()
        .any(|word| RELATIVE_WORDS.contains(&word.as_str()))
    {
        return true;
    }
    // A bare weekday ("on Saturday") means the one nearest the message; with a month or a day
    // number it is a written date ("Saturday 13 May").
    words.iter().any(|word| parse_weekday(word).is_some())
        && !words
            .iter()
            .any(|word| parse_month(word).is_some() || word.chars().all(|c| c.is_ascii_digit()))
}

fn start_of(day: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight exists"))
}

fn end_of(day: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&day.and_hms_opt(23, 59, 59).expect("end of day exists"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monday 8 May 2023, 13:56 UTC.
    fn monday() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2023, 5, 8, 13, 56, 0).unwrap()
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// (expression, anchor, expected (start, end, granularity) or None)
    fn cases() -> Vec<(&'static str, DateTime<Utc>, Option<Resolved>)> {
        use TemporalGranularity::*;
        let mon = monday();
        let sat = at(2023, 5, 13, 9); // Saturday
        let sun = at(2023, 5, 14, 9); // Sunday
        let new_year = at(2024, 1, 1, 10); // Monday, year boundary
        let leap = at(2024, 3, 1, 12); // day after 29 Feb 2024
        let month_end = at(2023, 3, 31, 12);
        vec![
            // relative days
            ("today", mon, Some((day(2023, 5, 8), day(2023, 5, 8), Day))),
            (
                "yesterday",
                mon,
                Some((day(2023, 5, 7), day(2023, 5, 7), Day)),
            ),
            (
                "Yesterday.",
                mon,
                Some((day(2023, 5, 7), day(2023, 5, 7), Day)),
            ),
            (
                "last night",
                mon,
                Some((day(2023, 5, 7), day(2023, 5, 7), Day)),
            ),
            (
                "tomorrow",
                mon,
                Some((day(2023, 5, 9), day(2023, 5, 9), Day)),
            ),
            (
                "the day before yesterday",
                mon,
                Some((day(2023, 5, 6), day(2023, 5, 6), Day)),
            ),
            (
                "the day after tomorrow",
                mon,
                Some((day(2023, 5, 10), day(2023, 5, 10), Day)),
            ),
            (
                "yesterday",
                new_year,
                Some((day(2023, 12, 31), day(2023, 12, 31), Day)),
            ),
            (
                "yesterday",
                leap,
                Some((day(2024, 2, 29), day(2024, 2, 29), Day)),
            ),
            (
                "tonight",
                mon,
                Some((day(2023, 5, 8), day(2023, 5, 8), Day)),
            ),
            // named weekdays, anchored on different weekdays
            (
                "last Saturday",
                mon,
                Some((day(2023, 5, 6), day(2023, 5, 6), Day)),
            ),
            (
                "last saturday",
                sat,
                Some((day(2023, 5, 6), day(2023, 5, 6), Day)),
            ),
            (
                "last Sunday",
                sun,
                Some((day(2023, 5, 7), day(2023, 5, 7), Day)),
            ),
            (
                "last Friday",
                sat,
                Some((day(2023, 5, 12), day(2023, 5, 12), Day)),
            ),
            (
                "last Monday",
                mon,
                Some((day(2023, 5, 1), day(2023, 5, 1), Day)),
            ),
            (
                "this past Friday",
                mon,
                Some((day(2023, 5, 5), day(2023, 5, 5), Day)),
            ),
            (
                "on last Tuesday",
                mon,
                Some((day(2023, 5, 2), day(2023, 5, 2), Day)),
            ),
            (
                "next Friday",
                mon,
                Some((day(2023, 5, 12), day(2023, 5, 12), Day)),
            ),
            (
                "next Monday",
                mon,
                Some((day(2023, 5, 15), day(2023, 5, 15), Day)),
            ),
            (
                "next Sunday",
                sat,
                Some((day(2023, 5, 14), day(2023, 5, 14), Day)),
            ),
            (
                "last Sunday",
                new_year,
                Some((day(2023, 12, 31), day(2023, 12, 31), Day)),
            ),
            // weeks (ISO, Monday–Sunday)
            (
                "last week",
                mon,
                Some((day(2023, 5, 1), day(2023, 5, 7), Week)),
            ),
            (
                "this week",
                sun,
                Some((day(2023, 5, 8), day(2023, 5, 14), Week)),
            ),
            (
                "next week",
                mon,
                Some((day(2023, 5, 15), day(2023, 5, 21), Week)),
            ),
            (
                "last week",
                new_year,
                Some((day(2023, 12, 25), day(2023, 12, 31), Week)),
            ),
            // months, including year and month-length boundaries
            (
                "last month",
                mon,
                Some((day(2023, 4, 1), day(2023, 4, 30), Month)),
            ),
            (
                "this month",
                mon,
                Some((day(2023, 5, 1), day(2023, 5, 31), Month)),
            ),
            (
                "next month",
                mon,
                Some((day(2023, 6, 1), day(2023, 6, 30), Month)),
            ),
            (
                "last month",
                new_year,
                Some((day(2023, 12, 1), day(2023, 12, 31), Month)),
            ),
            (
                "last month",
                leap,
                Some((day(2024, 2, 1), day(2024, 2, 29), Month)),
            ),
            (
                "next month",
                month_end,
                Some((day(2023, 4, 1), day(2023, 4, 30), Month)),
            ),
            // years
            (
                "last year",
                mon,
                Some((day(2022, 1, 1), day(2022, 12, 31), Year)),
            ),
            (
                "this year",
                mon,
                Some((day(2023, 1, 1), day(2023, 12, 31), Year)),
            ),
            (
                "next year",
                new_year,
                Some((day(2025, 1, 1), day(2025, 12, 31), Year)),
            ),
            // N units ago
            (
                "3 days ago",
                mon,
                Some((day(2023, 5, 5), day(2023, 5, 5), Day)),
            ),
            (
                "two days ago",
                new_year,
                Some((day(2023, 12, 30), day(2023, 12, 30), Day)),
            ),
            (
                "a week ago",
                mon,
                Some((day(2023, 5, 1), day(2023, 5, 7), Week)),
            ),
            (
                "2 weeks ago",
                mon,
                Some((day(2023, 4, 24), day(2023, 4, 30), Week)),
            ),
            (
                "a month ago",
                mon,
                Some((day(2023, 4, 1), day(2023, 4, 30), Month)),
            ),
            (
                "six months ago",
                mon,
                Some((day(2022, 11, 1), day(2022, 11, 30), Month)),
            ),
            (
                "5 years ago",
                mon,
                Some((day(2018, 1, 1), day(2018, 12, 31), Year)),
            ),
            (
                "one year ago",
                leap,
                Some((day(2023, 1, 1), day(2023, 12, 31), Year)),
            ),
            // month + year, year alone
            (
                "in May 2023",
                mon,
                Some((day(2023, 5, 1), day(2023, 5, 31), Month)),
            ),
            (
                "February 2024",
                mon,
                Some((day(2024, 2, 1), day(2024, 2, 29), Month)),
            ),
            (
                "Sept 2022",
                mon,
                Some((day(2022, 9, 1), day(2022, 9, 30), Month)),
            ),
            (
                "in 2019",
                mon,
                Some((day(2019, 1, 1), day(2019, 12, 31), Year)),
            ),
            (
                "2022",
                mon,
                Some((day(2022, 1, 1), day(2022, 12, 31), Year)),
            ),
            // explicit dates
            (
                "2023-05-06",
                mon,
                Some((day(2023, 5, 6), day(2023, 5, 6), Day)),
            ),
            (
                "7 May 2023",
                mon,
                Some((day(2023, 5, 7), day(2023, 5, 7), Day)),
            ),
            (
                "May 7, 2023",
                mon,
                Some((day(2023, 5, 7), day(2023, 5, 7), Day)),
            ),
            // weekday abbreviations and hedges
            (
                "last Fri",
                at(2023, 10, 13, 10),
                Some((day(2023, 10, 6), day(2023, 10, 6), Day)),
            ),
            (
                "next tues",
                mon,
                Some((day(2023, 5, 9), day(2023, 5, 9), Day)),
            ),
            (
                "about five years ago",
                at(2023, 8, 28, 15),
                Some((day(2018, 1, 1), day(2018, 12, 31), Year)),
            ),
            (
                "around 2019",
                mon,
                Some((day(2019, 1, 1), day(2019, 12, 31), Year)),
            ),
            // weekends: Saturday and Sunday before the anchor's week
            (
                "last weekend",
                mon,
                Some((day(2023, 5, 6), day(2023, 5, 7), Approximate)),
            ),
            (
                "last weekend",
                at(2023, 7, 15, 13),
                Some((day(2023, 7, 8), day(2023, 7, 9), Approximate)),
            ),
            (
                "this past weekend",
                at(2023, 10, 20, 18),
                Some((day(2023, 10, 14), day(2023, 10, 15), Approximate)),
            ),
            (
                "two weekends ago",
                at(2023, 7, 17, 14),
                Some((day(2023, 7, 8), day(2023, 7, 9), Approximate)),
            ),
            // deliberately not covered: left to the model
            ("over a year ago", mon, None),
            ("almost two years ago", mon, None),
            ("next weekend", mon, None),
            (
                "a few days ago",
                mon,
                Some((day(2023, 5, 4), day(2023, 5, 6), Approximate)),
            ),
            ("recently", mon, None),
            ("when I was a kid", mon, None),
            ("Saturday", mon, None),
            ("in May", mon, None),
            ("", mon, None),
            ("150 years ago", mon, None),
        ]
    }

    #[test]
    fn table_of_expressions_resolves_exactly() {
        let cases = cases();
        assert!(cases.len() >= 40, "P1 requires at least 40 resolver cases");
        for (text, anchor, expected) in cases {
            let resolved = resolve_expression(text, anchor);
            match (resolved, expected) {
                (None, None) => {}
                (Some(value), Some((start, end, granularity))) => {
                    assert_eq!(value.start.date_naive(), start, "{text:?} start");
                    assert_eq!(value.end.date_naive(), end, "{text:?} end");
                    assert_eq!(value.granularity, granularity, "{text:?} granularity");
                    assert_eq!(value.anchor, anchor, "{text:?} anchor");
                    assert_eq!(value.start.time(), chrono::NaiveTime::MIN, "{text:?}");
                }
                (got, want) => panic!("{text:?} at {anchor}: got {got:?}, expected {want:?}"),
            }
        }
    }

    #[test]
    fn expressions_are_found_as_whole_words_in_the_cited_text() {
        let text = "Oh, and I fixed the old bike last Fri, rides great now.";
        assert!(expression_occurs_in("last Fri", text));
        assert!(expression_occurs_in("last Friday", text));
        assert!(!expression_occurs_in("last night", text));
        let text = "I took up the cello about five years ago; love it.";
        assert!(expression_occurs_in("about five years ago", text));
        assert!(expression_occurs_in("five years ago", text));
        assert!(!expression_occurs_in("years", "Happy new-yearsday"));
        assert!(!expression_occurs_in("", text));
    }

    #[test]
    fn expression_variants_cover_abbreviations_and_hedges() {
        assert_eq!(
            expression_variants("last Fri"),
            vec!["last fri", "last friday"]
        );
        assert_eq!(
            expression_variants("about five years ago"),
            vec!["about five years ago", "five years ago"]
        );
        assert_eq!(expression_variants("yesterday"), vec!["yesterday"]);
    }

    #[test]
    fn resolution_uses_the_anchor_not_wall_clock_time() {
        // The same words said a year apart mean different days.
        let a = resolve_expression("yesterday", at(2021, 6, 10, 9)).unwrap();
        let b = resolve_expression("yesterday", at(2022, 6, 10, 9)).unwrap();
        assert_eq!(a.start.date_naive(), day(2021, 6, 9));
        assert_eq!(b.start.date_naive(), day(2022, 6, 9));
    }

    fn value(start: NaiveDate, end: NaiveDate, anchor: DateTime<Utc>) -> TemporalValue {
        TemporalValue::new(
            start_of(start),
            end_of(end),
            TemporalGranularity::Day,
            anchor,
        )
        .unwrap()
    }

    #[test]
    fn reconcile_agreement() {
        let model = value(day(2023, 5, 6), day(2023, 5, 6), monday());
        let (settled, outcome) = reconcile(Some(&model), Some("last Saturday"), monday());
        assert_eq!(outcome, TemporalOutcome::Agree);
        assert_eq!(settled.unwrap().start.date_naive(), day(2023, 5, 6));
    }

    #[test]
    fn reconcile_deterministic_wins_where_it_covers_the_expression() {
        // Model counted from the wrong day.
        let model = value(day(2023, 5, 13), day(2023, 5, 13), monday());
        let (settled, outcome) = reconcile(Some(&model), Some("last Saturday"), monday());
        assert_eq!(outcome, TemporalOutcome::DeterministicOverride);
        assert_eq!(settled.unwrap().start.date_naive(), day(2023, 5, 6));
    }

    #[test]
    fn reconcile_keeps_the_model_for_uncovered_expressions() {
        let model = value(day(2023, 5, 6), day(2023, 5, 7), monday());
        let (settled, outcome) = reconcile(Some(&model), Some("the first week of May"), monday());
        assert_eq!(outcome, TemporalOutcome::LlmOnly);
        let settled = settled.unwrap();
        assert_eq!(settled.start.date_naive(), day(2023, 5, 6));
        assert_eq!(
            settled.anchor,
            monday(),
            "the stored anchor is the message time"
        );
    }

    #[test]
    fn reconcile_fills_in_when_the_model_gave_no_time() {
        let (settled, outcome) = reconcile(None, Some("yesterday"), monday());
        assert_eq!(outcome, TemporalOutcome::DeterministicOnly);
        assert_eq!(settled.unwrap().start.date_naive(), day(2023, 5, 7));
    }

    #[test]
    fn reconcile_rejects_impossible_model_values() {
        let far_future = value(day(2090, 1, 1), day(2090, 1, 1), monday());
        let (settled, outcome) =
            reconcile(Some(&far_future), Some("at the next reunion"), monday());
        assert_eq!(outcome, TemporalOutcome::Rejected);
        assert!(settled.is_none());

        let ancient = value(day(900, 1, 1), day(900, 1, 1), monday());
        let (settled, outcome) = reconcile(Some(&ancient), Some("yesterday"), monday());
        assert_eq!(outcome, TemporalOutcome::DeterministicOverride);
        assert_eq!(settled.unwrap().start.date_naive(), day(2023, 5, 7));
    }

    #[test]
    fn a_model_time_with_no_expression_is_ungrounded_and_dropped() {
        // The model stamped the message's own date onto a timeless fact.
        let stamped = value(day(2023, 5, 8), day(2023, 5, 8), monday());
        assert_eq!(
            reconcile(Some(&stamped), None, monday()),
            (None, TemporalOutcome::Ungrounded)
        );
        assert_eq!(
            reconcile(Some(&stamped), Some("   "), monday()),
            (None, TemporalOutcome::Ungrounded)
        );
        // "today" is an expression, so the same date is kept when the text says so.
        let (settled, outcome) = reconcile(Some(&stamped), Some("today"), monday());
        assert_eq!(outcome, TemporalOutcome::Agree);
        assert!(settled.is_some());
    }

    #[test]
    fn reconcile_with_nothing_is_none() {
        assert_eq!(
            reconcile(None, None, monday()),
            (None, TemporalOutcome::None)
        );
        assert_eq!(
            reconcile(None, Some("at the next reunion"), monday()),
            (None, TemporalOutcome::None)
        );
    }

    /// Vague recency, life stages, frequencies, times relative to other events and pure durations
    /// cannot be dated; the model's date (usually the message day) is dropped.
    #[test]
    fn non_datable_expressions_yield_no_time_whatever_the_model_said() {
        let stamped = value(day(2023, 5, 8), day(2023, 5, 8), monday());
        for text in [
            "recently",
            "Lately",
            "the other day",
            "since we last spoke",
            "long time no see",
            "already",
            "a while ago",
            "when I was 10",
            "when we were kids",
            "17 or so",
            "17",
            "growing up",
            "every week",
            "on weekends",
            "twice a month",
            "after a day of volunteering",
            "before the move",
            "during the trip",
            "last season",
            "for five years",
            "two hours",
            "seven years now",
        ] {
            assert_eq!(
                reconcile(Some(&stamped), Some(text), monday()),
                (None, TemporalOutcome::NonDatable),
                "{text:?}"
            );
            assert_eq!(
                reconcile(None, Some(text), monday()).1,
                TemporalOutcome::NonDatable,
                "{text:?}"
            );
        }
    }

    #[test]
    fn expressions_with_a_calendar_anchor_are_never_non_datable() {
        for text in [
            "recently, in June",
            "every Saturday",
            "since 2020",
            "before yesterday",
            "may",
        ] {
            assert!(!is_non_datable(text), "{text:?}");
        }
        // Covered expressions resolve before the non-datable check.
        let (settled, outcome) = reconcile(None, Some("a couple of weeks ago"), monday());
        assert_eq!(outcome, TemporalOutcome::DeterministicOnly);
        assert!(settled.is_some());
    }

    #[test]
    fn seasons_resolve_to_their_months() {
        use TemporalGranularity::Approximate;
        let october = at(2023, 10, 6, 12);
        let january = at(2024, 1, 15, 12);
        let july = at(2023, 7, 10, 12);
        for (text, anchor, want) in [
            ("last summer", october, (day(2023, 6, 1), day(2023, 8, 31))),
            (
                "this past summer",
                october,
                (day(2023, 6, 1), day(2023, 8, 31)),
            ),
            ("last summer", july, (day(2022, 6, 1), day(2022, 8, 31))),
            ("this summer", july, (day(2023, 6, 1), day(2023, 8, 31))),
            ("this summer", october, (day(2023, 6, 1), day(2023, 8, 31))),
            ("last winter", october, (day(2022, 12, 1), day(2023, 2, 28))),
            ("this winter", january, (day(2023, 12, 1), day(2024, 2, 29))),
            ("last fall", january, (day(2023, 9, 1), day(2023, 11, 30))),
            ("last autumn", january, (day(2023, 9, 1), day(2023, 11, 30))),
            ("summer 2022", october, (day(2022, 6, 1), day(2022, 8, 31))),
            (
                "the spring of 2021",
                october,
                (day(2021, 3, 1), day(2021, 5, 31)),
            ),
        ] {
            let value = resolve_expression(text, anchor).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(
                (value.start.date_naive(), value.end.date_naive()),
                want,
                "{text:?} at {anchor}"
            );
            assert_eq!(value.granularity, Approximate, "{text:?}");
        }
        for text in ["next summer", "winter 2023", "the summer"] {
            assert!(resolve_expression(text, october).is_none(), "{text:?}");
        }
    }

    #[test]
    fn modifiers_possessives_and_vague_counts() {
        use TemporalGranularity::*;
        let mon = monday();
        for (text, want) in [
            (
                "early June 2022",
                (day(2022, 6, 1), day(2022, 6, 30), Month),
            ),
            ("mid-June 2022", (day(2022, 6, 1), day(2022, 6, 30), Month)),
            (
                "earlier this week",
                (day(2023, 5, 8), day(2023, 5, 14), Week),
            ),
            (
                "later this month",
                (day(2023, 5, 1), day(2023, 5, 31), Month),
            ),
            (
                "sometime last year",
                (day(2022, 1, 1), day(2022, 12, 31), Year),
            ),
            ("last week's", (day(2023, 5, 1), day(2023, 5, 7), Week)),
            (
                "last week's scrimmage",
                (day(2023, 5, 1), day(2023, 5, 7), Week),
            ),
            (
                "a couple of weeks ago",
                (day(2023, 4, 24), day(2023, 4, 30), Week),
            ),
            (
                "a few days ago",
                (day(2023, 5, 4), day(2023, 5, 6), Approximate),
            ),
            (
                "a few weeks ago",
                (day(2023, 4, 10), day(2023, 4, 30), Approximate),
            ),
            (
                "several months ago",
                (day(2022, 11, 1), day(2023, 2, 28), Approximate),
            ),
            (
                "a few years ago",
                (day(2019, 1, 1), day(2021, 12, 31), Approximate),
            ),
        ] {
            let value = resolve_expression(text, mon).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(
                (
                    value.start.date_naive(),
                    value.end.date_naive(),
                    value.granularity
                ),
                want,
                "{text:?}"
            );
        }
    }

    #[test]
    fn durations_have_a_length_but_no_position() {
        for text in [
            "two hours",
            "for 45 minutes",
            "for 4 years",
            "seven years now",
        ] {
            assert!(is_duration(text), "{text:?}");
        }
        for text in ["two years ago", "in two hours", "last week", "May 2023"] {
            assert!(!is_duration(text), "{text:?}");
        }
    }

    fn unanchored_value(start: NaiveDate, end: NaiveDate) -> TemporalValue {
        TemporalValue::new(
            start_of(start),
            end_of(end),
            TemporalGranularity::Day,
            start_of(start),
        )
        .unwrap()
    }

    /// P2-20: an absolute date needs no anchor and keeps its value.
    #[test]
    fn without_an_anchor_explicit_dates_are_kept() {
        use TemporalGranularity::*;
        for (text, want) in [
            ("8 May 2023", (day(2023, 5, 8), day(2023, 5, 8), Day)),
            ("May 2023", (day(2023, 5, 1), day(2023, 5, 31), Month)),
            ("in March 2024", (day(2024, 3, 1), day(2024, 3, 31), Month)),
            ("2019", (day(2019, 1, 1), day(2019, 12, 31), Year)),
            (
                "summer 2022",
                (day(2022, 6, 1), day(2022, 8, 31), Approximate),
            ),
        ] {
            let (settled, outcome) = settle_event_time(None, Some(text), None);
            assert_eq!(outcome, TemporalOutcome::DeterministicOnly, "{text:?}");
            let settled = settled.unwrap_or_else(|| panic!("{text:?}"));
            assert_eq!(
                (
                    settled.start.date_naive(),
                    settled.end.date_naive(),
                    settled.granularity
                ),
                want,
                "{text:?}"
            );
            assert_eq!(settled.anchor, settled.start, "no client anchor: {text:?}");
        }
        // The model agreeing or disagreeing does not matter: the resolver's value is used.
        let wrong = unanchored_value(day(2023, 5, 9), day(2023, 5, 9));
        let (settled, outcome) = settle_event_time(Some(&wrong), Some("8 May 2023"), None);
        assert_eq!(outcome, TemporalOutcome::DeterministicOverride);
        assert_eq!(settled.unwrap().start.date_naive(), day(2023, 5, 8));
    }

    /// P2-20: nothing relative is resolved against "now" when the client sent no time.
    #[test]
    fn without_an_anchor_relative_expressions_are_unanchored_even_if_the_model_dated_them() {
        let model_guess = unanchored_value(day(2025, 9, 22), day(2025, 9, 22));
        for text in [
            "yesterday",
            "last year",
            "two years ago",
            "last Saturday",
            "this summer",
            "last summer",
            "a few weeks ago",
            "next month",
        ] {
            assert_eq!(
                settle_event_time(Some(&model_guess), Some(text), None),
                (None, TemporalOutcome::Unanchored),
                "{text:?}"
            );
            assert_eq!(
                settle_event_time(None, Some(text), None),
                (None, TemporalOutcome::Unanchored),
                "{text:?}"
            );
        }
    }

    #[test]
    fn without_an_anchor_a_model_date_is_kept_only_within_the_years_the_text_names() {
        let fourth_of_july = unanchored_value(day(2021, 7, 4), day(2021, 7, 4));
        // Not a resolver pattern, but it names 2021 and the model's day lies in it.
        let (settled, outcome) =
            settle_event_time(Some(&fourth_of_july), Some("the 4th of July 2021"), None);
        assert_eq!(outcome, TemporalOutcome::LlmOnly);
        assert_eq!(settled.unwrap().anchor, start_of(day(2021, 7, 4)));
        // A year the model's date is not in, or no year at all, is not enough.
        assert_eq!(
            settle_event_time(Some(&fourth_of_july), Some("the 4th of July 2019"), None).1,
            TemporalOutcome::Unanchored
        );
        assert_eq!(
            settle_event_time(Some(&fourth_of_july), Some("the 4th of July"), None).1,
            TemporalOutcome::Unanchored
        );
    }

    #[test]
    fn without_an_anchor_ungrounded_and_non_datable_rules_still_apply() {
        let guess = unanchored_value(day(2025, 9, 22), day(2025, 9, 22));
        assert_eq!(
            settle_event_time(Some(&guess), None, None),
            (None, TemporalOutcome::Ungrounded)
        );
        assert_eq!(
            settle_event_time(None, None, None),
            (None, TemporalOutcome::None)
        );
        assert_eq!(
            settle_event_time(Some(&guess), Some("recently"), None),
            (None, TemporalOutcome::NonDatable)
        );
    }

    #[test]
    fn with_an_anchor_settle_is_exactly_reconcile() {
        let model = value(day(2023, 5, 13), day(2023, 5, 13), monday());
        for text in [
            Some("last Saturday"),
            Some("recently"),
            Some("May 2023"),
            None,
        ] {
            assert_eq!(
                settle_event_time(Some(&model), text, Some(monday())),
                reconcile(Some(&model), text, monday()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn resolve_absolute_accepts_fixed_points_only() {
        assert!(resolve_absolute("8 May 2023").is_some());
        assert!(resolve_absolute("the spring of 2021").is_some());
        assert!(resolve_absolute("yesterday").is_none());
        assert!(resolve_absolute("last year").is_none());
        assert!(resolve_absolute("this spring").is_none());
        assert!(resolve_absolute("the day after tomorrow").is_none());
    }

    #[test]
    fn counts_track_every_outcome() {
        let mut counts = TemporalResolutionCounts::default();
        for outcome in TemporalOutcome::ALL {
            counts.record(outcome);
        }
        assert_eq!(counts.total(), 9);
        for outcome in TemporalOutcome::ALL {
            assert_eq!(counts.get(outcome), 1, "{}", outcome.as_str());
        }
    }
}
