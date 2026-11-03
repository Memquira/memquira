//! Query understanding.
//!
//! [`analyze`] is a pure, deterministic, English-only function: no model, no I/O, no entity system
//! and no system clock. Its contract, including every rule below, is frozen in
//! the fixture
//! `tests/fixtures/phase4_analyzer_contract_v1.json`. Proper names stay plain query text.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::temporal::{resolve_absolute, resolve_expression};
use crate::{TemporalGranularity, TemporalValue};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionType {
    When,
    Count,
    YesNo,
    List,
    Inference,
    #[default]
    Factoid,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalOrdinal {
    Earliest,
    Latest,
    #[default]
    None,
}

/// An inclusive UTC range; either end may be open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalConstraint {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub granularity: TemporalGranularity,
}

impl TemporalConstraint {
    /// Whether an event time overlaps the range (open ends are unbounded).
    pub fn overlaps(&self, event: &TemporalValue) -> bool {
        self.start.is_none_or(|start| event.end >= start)
            && self.end.is_none_or(|end| event.start <= end)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryAnalysis {
    pub question_type: QuestionType,
    pub temporal_constraint: Option<TemporalConstraint>,
    /// The relative time expression found, lower-case, whether or not it could be resolved.
    pub relative_expression: Option<String>,
    pub temporal_ordinal: TemporalOrdinal,
    /// Clause sub-queries for deterministic multi-query; empty unless the question has 2–3 genuine
    /// clauses. The original query is always used in addition.
    pub sub_queries: Vec<String>,
}

const INFERENCE: [&str; 7] = [
    "would", "might", "likely", "probably", "possibly", "perhaps", "probable",
];
const WHEN_HEADS: [&str; 5] = ["date", "day", "year", "month", "time"];
const AUXILIARIES: [&str; 16] = [
    "is", "are", "was", "were", "do", "does", "did", "has", "have", "had", "can", "could", "will",
    "shall", "should", "may",
];
const LIST_STOP: [&str; 13] = [
    "was", "has", "does", "its", "hers", "ours", "yours", "theirs", "as", "yes", "news", "series",
    "species",
];
const WH_WORDS: [&str; 9] = [
    "what", "when", "where", "who", "whom", "whose", "which", "why", "how",
];
const SEASONS: [&str; 5] = ["spring", "summer", "autumn", "fall", "winter"];
const ORDINAL_EXCLUDED_NEXT: [&str; 3] = ["name", "names", "place"];
const PERIOD_WORDS: [&str; 5] = ["week", "weekend", "month", "year", "night"];
const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
const MAX_CLAUSES: usize = 3;
const MIN_CLAUSE_WORDS: usize = 3;
const MAX_RELATIVE_WINDOW: usize = 5;

/// One whitespace-separated word: its normalised form and its byte span in the query.
struct Word {
    norm: String,
    start: usize,
    end: usize,
}

fn words_of(query: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut start = None;
    for (index, ch) in query
        .char_indices()
        .chain(std::iter::once((query.len(), ' ')))
    {
        if ch.is_whitespace() {
            if let Some(begin) = start.take() {
                let raw: &str = &query[begin..index];
                let norm = raw
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase();
                words.push(Word {
                    norm,
                    start: begin,
                    end: index,
                });
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    words
}

/// Analyses one query. `reference_time` only resolves relative expressions; its UTC
/// calendar date is the anchor day.
pub fn analyze(query: &str, reference_time: Option<DateTime<FixedOffset>>) -> QueryAnalysis {
    let words = words_of(query);
    let norms: Vec<&str> = words.iter().map(|word| word.norm.as_str()).collect();
    let question_type = question_type(&norms);
    let clauses = clause_ranges(&norms);
    let (temporal_constraint, relative_expression) = temporal_constraint(
        &norms,
        &clauses,
        reference_time.map(|time| time.with_timezone(&Utc)),
    );
    QueryAnalysis {
        question_type,
        temporal_constraint,
        relative_expression,
        temporal_ordinal: temporal_ordinal(&norms, question_type),
        sub_queries: sub_queries(query, &words, &clauses),
    }
}

fn starts_with(norms: &[&str], prefix: &[&str]) -> bool {
    norms.len() >= prefix.len() && norms[..prefix.len()] == *prefix
}

fn contains_seq(norms: &[&str], seq: &[&str]) -> bool {
    norms.windows(seq.len()).any(|window| window == seq)
}

fn question_type(norms: &[&str]) -> QuestionType {
    let first = norms.first().copied().unwrap_or("");
    let second = norms.get(1).copied().unwrap_or("");
    if norms.iter().any(|word| INFERENCE.contains(word)) {
        return QuestionType::Inference;
    }
    if first == "when"
        || (matches!(first, "what" | "which") && WHEN_HEADS.contains(&second))
        || [
            &["how", "long", "ago"][..],
            &["on", "what", "day"],
            &["in", "what", "year"],
            &["in", "which", "year"],
            &["on", "which", "day"],
        ]
        .iter()
        .any(|prefix| starts_with(norms, prefix))
    {
        return QuestionType::When;
    }
    if contains_seq(norms, &["how", "many"])
        || contains_seq(norms, &["how", "often"])
        || contains_seq(norms, &["number", "of"])
    {
        return QuestionType::Count;
    }
    if AUXILIARIES.contains(&first) {
        return QuestionType::YesNo;
    }
    let plural_head = matches!(first, "what" | "which")
        && second.ends_with('s')
        && !second.ends_with("ss")
        && !second.ends_with("us")
        && !second.ends_with("is")
        && !LIST_STOP.contains(&second);
    if matches!(first, "list" | "name")
        || (first == "what" && matches!(second, "are" | "were"))
        || plural_head
    {
        return QuestionType::List;
    }
    QuestionType::Factoid
}

fn temporal_ordinal(norms: &[&str], question_type: QuestionType) -> TemporalOrdinal {
    let is_when = question_type == QuestionType::When;
    let next_of = |index: usize| norms.get(index + 1).copied().unwrap_or("");
    let earliest = contains_seq(norms, &["first", "time"])
        || norms.contains(&"earliest")
        || (is_when
            && norms.iter().enumerate().any(|(index, word)| {
                *word == "first" && !ORDINAL_EXCLUDED_NEXT.contains(&next_of(index))
            }));
    let latest = contains_seq(norms, &["most", "recent"])
        || contains_seq(norms, &["most", "recently"])
        || contains_seq(norms, &["last", "time"])
        || norms.contains(&"latest")
        || (is_when
            && norms.iter().enumerate().any(|(index, word)| {
                *word == "last"
                    && !ORDINAL_EXCLUDED_NEXT.contains(&next_of(index))
                    && !is_period_word(next_of(index))
            }));
    match (earliest, latest) {
        (true, false) => TemporalOrdinal::Earliest,
        (false, true) => TemporalOrdinal::Latest,
        _ => TemporalOrdinal::None,
    }
}

fn is_period_word(word: &str) -> bool {
    PERIOD_WORDS.contains(&word) || WEEKDAYS.contains(&word) || parse_month(word).is_some()
}

// ------------------------------------------------------------------------------------ clauses

/// Word ranges of the clauses: one range for the whole query unless it splits.
fn clause_ranges(norms: &[&str]) -> Vec<(usize, usize)> {
    // (conjunction start, first word after the conjunction)
    let mut points: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < norms.len() {
        let conjunction_len = if norms[index] == "and" {
            1
        } else if norms[index..].starts_with(&["but", "also"]) {
            2
        } else if norms[index..].starts_with(&["as", "well", "as"]) {
            3
        } else {
            0
        };
        if conjunction_len > 0 {
            let after = index + conjunction_len;
            if let Some(next) = norms.get(after)
                && (WH_WORDS.contains(next) || AUXILIARIES.contains(next))
            {
                points.push((index, after));
                index = after;
                continue;
            }
        }
        index += 1;
    }
    loop {
        let ranges = ranges_from(norms.len(), &points);
        if points.is_empty() {
            return ranges;
        }
        match ranges
            .iter()
            .position(|(start, end)| end - start < MIN_CLAUSE_WORDS)
        {
            None => return ranges,
            Some(short) => {
                // Drop the split that created the short clause: the one before it, or for the first
                // clause the one after it.
                let drop = if short == 0 { 0 } else { short - 1 };
                points.remove(drop);
            }
        }
    }
}

fn ranges_from(len: usize, points: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(points.len() + 1);
    let mut start = 0;
    for (conjunction, after) in points {
        ranges.push((start, *conjunction));
        start = *after;
    }
    ranges.push((start, len));
    ranges
}

fn sub_queries(query: &str, words: &[Word], clauses: &[(usize, usize)]) -> Vec<String> {
    if clauses.len() < 2 {
        return Vec::new();
    }
    let question = query.trim_end().ends_with('?');
    clauses
        .iter()
        .take(MAX_CLAUSES)
        .map(|(start, end)| {
            let text = query[words[*start].start..words[end - 1].end]
                .trim()
                .trim_end_matches([',', ';', ':', '.', '!', '?'])
                .trim_end();
            if question {
                format!("{text}?")
            } else {
                text.to_string()
            }
        })
        .collect()
}

// --------------------------------------------------------------------------------- dates

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
    if word.len() != 4 || !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = word.parse().ok()?;
    (1900..=2099).contains(&year).then_some(year)
}

fn parse_day(word: &str) -> Option<u32> {
    let digits = word
        .strip_suffix("st")
        .or_else(|| word.strip_suffix("nd"))
        .or_else(|| word.strip_suffix("rd"))
        .or_else(|| word.strip_suffix("th"))
        .unwrap_or(word);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let day: u32 = digits.parse().ok()?;
    (1..=31).contains(&day).then_some(day)
}

fn parse_iso(word: &str) -> Option<NaiveDate> {
    if word.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(word, "%Y-%m-%d").ok()
}

fn day_start(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists"))
}

fn day_end(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59).expect("end of day exists"))
}

fn span(
    first: NaiveDate,
    last: NaiveDate,
    granularity: TemporalGranularity,
) -> Option<TemporalConstraint> {
    (first <= last).then(|| TemporalConstraint {
        start: Some(day_start(first)),
        end: Some(day_end(last)),
        granularity,
    })
}

fn month_span(year: i32, month: u32) -> Option<TemporalConstraint> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let last = first.checked_add_months(chrono::Months::new(1))? - Duration::days(1);
    span(first, last, TemporalGranularity::Month)
}

/// An absolute expression starting at `at` (§2.2 A1–A5): the longest match and its word count.
fn absolute_at(norms: &[&str], at: usize) -> Option<(TemporalConstraint, usize)> {
    let word = |offset: usize| norms.get(at + offset).copied().unwrap_or("");
    let date = |year: i32, month: u32, day: u32| NaiveDate::from_ymd_opt(year, month, day);
    // A2: between D and D Month YYYY
    if word(0) == "between"
        && word(2) == "and"
        && let (Some(first), Some(last), Some(month), Some(year)) = (
            parse_day(word(1)),
            parse_day(word(3)),
            parse_month(word(4)),
            parse_year(word(5)),
        )
        && let Some(found) = span(
            date(year, month, first)?,
            date(year, month, last)?,
            TemporalGranularity::Day,
        )
    {
        return Some((found, 6));
    }
    // A3: the D of Month YYYY
    if word(0) == "the"
        && word(2) == "of"
        && let (Some(day), Some(month), Some(year)) = (
            parse_day(word(1)),
            parse_month(word(3)),
            parse_year(word(4)),
        )
    {
        let found = date(year, month, day)?;
        return Some((span(found, found, TemporalGranularity::Day)?, 5));
    }
    // A3: D Month YYYY
    if let (Some(day), Some(month), Some(year)) = (
        parse_day(word(0)),
        parse_month(word(1)),
        parse_year(word(2)),
    ) {
        let found = date(year, month, day)?;
        return Some((span(found, found, TemporalGranularity::Day)?, 3));
    }
    if let Some(month) = parse_month(word(0)) {
        // A3: Month D YYYY (the comma is stripped by normalisation)
        if let (Some(day), Some(year)) = (parse_day(word(1)), parse_year(word(2))) {
            let found = date(year, month, day)?;
            return Some((span(found, found, TemporalGranularity::Day)?, 3));
        }
        // A4: Month of YYYY / Month YYYY
        if word(1) == "of"
            && let Some(year) = parse_year(word(2))
        {
            return Some((month_span(year, month)?, 3));
        }
        if let Some(year) = parse_year(word(1)) {
            return Some((month_span(year, month)?, 2));
        }
    }
    // A1: ISO date
    if let Some(found) = parse_iso(word(0)) {
        return Some((span(found, found, TemporalGranularity::Day)?, 1));
    }
    // A5: a standalone year
    if let Some(year) = parse_year(word(0)) {
        return Some((
            span(
                NaiveDate::from_ymd_opt(year, 1, 1)?,
                NaiveDate::from_ymd_opt(year, 12, 31)?,
                TemporalGranularity::Year,
            )?,
            1,
        ));
    }
    None
}

/// A relative expression starting at `at`: the longest window the resolver covers that is
/// neither absolute nor seasonal.
fn relative_at(norms: &[&str], at: usize) -> Option<(String, usize)> {
    let probe = Utc
        .with_ymd_and_hms(2000, 1, 15, 12, 0, 0)
        .single()
        .expect("valid probe anchor");
    (1..=MAX_RELATIVE_WINDOW.min(norms.len() - at))
        .rev()
        .find_map(|len| {
            let window = &norms[at..at + len];
            if window
                .iter()
                .any(|word| word.is_empty() || SEASONS.contains(word))
            {
                return None;
            }
            let text = window.join(" ");
            (resolve_expression(&text, probe).is_some() && resolve_absolute(&text).is_none())
                .then_some((text, len))
        })
}

fn temporal_constraint(
    norms: &[&str],
    clauses: &[(usize, usize)],
    reference: Option<DateTime<Utc>>,
) -> (Option<TemporalConstraint>, Option<String>) {
    for at in 0..norms.len() {
        let absolute = absolute_at(norms, at);
        let relative = relative_at(norms, at);
        let (found, relative_text) = match (absolute, relative) {
            (None, None) => continue,
            (Some((found, _)), None) => (Some(found), None),
            (Some((found, absolute_len)), Some((_, relative_len)))
                if absolute_len >= relative_len =>
            {
                (Some(found), None)
            }
            (_, Some((text, _))) => {
                let resolved = reference
                    .and_then(|anchor| resolve_expression(&text, anchor))
                    .map(|value| TemporalConstraint {
                        start: Some(value.start),
                        end: Some(value.end),
                        granularity: value.granularity,
                    });
                (resolved, Some(text))
            }
        };
        let found = found.map(|constraint| open_ended(norms, clauses, at, constraint));
        return (found, relative_text);
    }
    (None, None)
}

/// §2.2: the nearest `before` / `after` earlier in the same clause opens the range.
fn open_ended(
    norms: &[&str],
    clauses: &[(usize, usize)],
    at: usize,
    constraint: TemporalConstraint,
) -> TemporalConstraint {
    let clause_start = clauses
        .iter()
        .find(|(start, end)| *start <= at && at < *end)
        .map_or(0, |(start, _)| *start);
    let relation = norms[clause_start..at]
        .iter()
        .rev()
        .find(|word| matches!(**word, "before" | "after"));
    match relation {
        Some(&"before") => TemporalConstraint {
            start: None,
            end: constraint.start.map(|start| start - Duration::seconds(1)),
            granularity: constraint.granularity,
        },
        Some(&"after") => TemporalConstraint {
            start: constraint.end.map(|end| end + Duration::seconds(1)),
            end: None,
            granularity: constraint.granularity,
        },
        _ => constraint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_queries_give_defaults() {
        assert_eq!(analyze("", None), QueryAnalysis::default());
        assert_eq!(analyze("   \t ", None), QueryAnalysis::default());
    }

    #[test]
    fn overlap_respects_open_ends() {
        let at = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();
        let event = TemporalValue::new(
            at(2021, 3, 10),
            at(2021, 3, 10),
            TemporalGranularity::Day,
            at(2023, 5, 1),
        )
        .unwrap();
        let before = TemporalConstraint {
            start: None,
            end: Some(at(2021, 3, 1)),
            granularity: TemporalGranularity::Month,
        };
        let after = TemporalConstraint {
            start: Some(at(2021, 3, 1)),
            end: None,
            granularity: TemporalGranularity::Month,
        };
        assert!(!before.overlaps(&event));
        assert!(after.overlaps(&event));
    }

    #[test]
    fn arbitrary_unicode_never_panics_and_is_deterministic() {
        // A fixed xorshift stream over a mixed alphabet: letters, digits, punctuation, combining marks,
        // right-to-left and astral characters, and whitespace of several kinds.
        let alphabet: Vec<char> = "aZ9 ,.?!'-\u{2019}\u{e9}\u{301}\u{5d0}\u{1f600}\u{3000}\t\nwhen last first and 2023 may"
            .chars()
            .collect();
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..2_000 {
            let mut text = String::new();
            for _ in 0..(state % 40) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                text.push(alphabet[(state % alphabet.len() as u64) as usize]);
            }
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            assert_eq!(analyze(&text, None), analyze(&text, None));
        }
    }
}
