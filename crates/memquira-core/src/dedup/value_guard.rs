//! Value-aware deduplication (`MEMQUIRA_DEDUP_VALUE_GUARD`).
//!
//! The legacy near-duplicate rules compare wording only: token Jaccard >= 0.90
//! ([`super::detect_duplicate`]) or embedding cosine >= the threshold
//! ([`super::detect_embedding_duplicate`]). A dedup hit is a silent NOOP that ends processing, so
//! the update classifier never sees the candidate. Two sentences that differ only in a value score
//! as near-identical ("Wilma's birthday is on the 14th of June." / "…16th of June.": embedding cosine
//! 0.996), and the new value was discarded. In an internal evaluation 6 of 13 dedup discards were
//! such losses, and the cosines of distinct and equivalent pairs overlap, so no
//! threshold separates them.
//!
//! With the guard on, a near-duplicate hit counts only when the candidate is **value-compatible**
//! with the matched fact:
//! - it states no value the matched fact lacks (numbers, dates, months, weekdays, times, currencies,
//!   identifiers, names), after normalisation ("16th" = "16", "two" = "2", "$1,620" = "1620 dollars",
//!   "20%" = "20 percent", "the 16th of June" = "June 16");
//! - both or neither are negated;
//! - their settled event times do not exclude each other (disjoint intervals).
//!
//! An incompatible hit is skipped, and the next hit is examined. A candidate with no compatible hit
//! continues to the related-fact lookup and the classifier like any non-duplicate. The guard never
//! decides an update: a different value only means "not silently discarded". Exact normalised
//! equality is never guarded, so identical retries and replays stay NOOPs. Values the candidate
//! omits never make it distinct (a less specific restatement is still a duplicate). Anything the
//! normaliser cannot interpret is compared as a literal token, which can only make the guard keep a
//! candidate, never discard one.

use std::collections::BTreeSet;

use crate::TemporalValue;

/// Whether near-duplicate dedup checks values (`MEMQUIRA_DEDUP_VALUE_GUARD`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DedupValueGuard {
    /// Legacy: a near-duplicate by wording is a duplicate. Default; the previous behaviour.
    #[default]
    Off,
    /// Opt-in: a near-duplicate is a duplicate only when value-compatible.
    On,
}

impl DedupValueGuard {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }

    pub fn is_on(self) -> bool {
        self == Self::On
    }
}

/// Why a near-duplicate hit was not treated as a duplicate (metric and audit label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueConflict {
    /// The candidate states a value the matched fact does not.
    NewValue,
    /// One sentence is negated and the other is not.
    Polarity,
    /// Both have settled event times and the intervals do not overlap.
    EventTime,
}

impl ValueConflict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewValue => "new_value",
            Self::Polarity => "polarity",
            Self::EventTime => "event_time",
        }
    }
}

/// The comparable values of one sentence.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SentenceValues {
    /// Typed, normalised values: `num:16`, `month:6`, `weekday:3`, `time:9:30`, `cur:usd`,
    /// `id:555-0142`, `name:wilma`.
    pub values: BTreeSet<String>,
    /// Every lower-cased word of the sentence (a name the other sentence writes in lower case still
    /// matches: "the Basic plan" / "the basic plan").
    pub words: BTreeSet<String>,
    pub negated: bool,
}

/// `Ok(())` when `candidate` may be treated as a duplicate of `existing`, else the first conflict.
pub fn value_compatibility(
    candidate: &str,
    candidate_time: Option<&TemporalValue>,
    existing: &str,
    existing_time: Option<&TemporalValue>,
) -> Result<(), ValueConflict> {
    if let (Some(c), Some(e)) = (candidate_time, existing_time)
        && (c.end < e.start || e.end < c.start)
    {
        return Err(ValueConflict::EventTime);
    }
    let cand = sentence_values(candidate);
    let ex = sentence_values(existing);
    if cand.negated != ex.negated {
        return Err(ValueConflict::Polarity);
    }
    let new_value = cand.values.iter().any(|value| {
        if ex.values.contains(value) {
            return false;
        }
        // A capitalised word the other sentence has in lower case is the same word.
        match value.strip_prefix("name:") {
            Some(name) => !ex.words.contains(name),
            None => true,
        }
    });
    if new_value {
        return Err(ValueConflict::NewValue);
    }
    Ok(())
}

const NEGATORS: &[&str] = &[
    "not", "no", "never", "cannot", "nobody", "nothing", "none", "neither", "nor", "without",
];

/// Capitalised words that start a sentence without naming anything.
const SENTENCE_STARTERS: &[&str] = &[
    "the",
    "a",
    "an",
    "user",
    "user's",
    "in",
    "on",
    "at",
    "between",
    "around",
    "this",
    "that",
    "these",
    "those",
    "it",
    "its",
    "he",
    "she",
    "they",
    "we",
    "i",
    "his",
    "her",
    "their",
    "our",
    "my",
    "your",
    "since",
    "after",
    "before",
    "during",
    "from",
    "for",
    "by",
    "as",
    "when",
    "while",
    "if",
    "there",
    "today",
    "yesterday",
    "tomorrow",
    "last",
    "next",
    "every",
    "each",
    "both",
    "some",
    "most",
    "all",
    "no",
    "not",
    "one",
    "and",
    "but",
    "so",
    "also",
];

const MONTHS: [(&str, u8); 23] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("sept", 9),
    ("oct", 10),
    ("nov", 11),
];

const WEEKDAYS: [(&str, u8); 7] = [
    ("monday", 1),
    ("tuesday", 2),
    ("wednesday", 3),
    ("thursday", 4),
    ("friday", 5),
    ("saturday", 6),
    ("sunday", 7),
];

/// Number words. "one" and "a" are not values: "one of my hobbies", "the old one" and "a dog" are
/// too common to count; the digit "1" still does.
const UNITS: [(&str, u64); 19] = [
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("thirteen", 13),
    ("fourteen", 14),
    ("fifteen", 15),
    ("sixteen", 16),
    ("seventeen", 17),
    ("eighteen", 18),
    ("nineteen", 19),
    ("zero", 0),
];

const TENS: [(&str, u64); 8] = [
    ("twenty", 20),
    ("thirty", 30),
    ("forty", 40),
    ("fifty", 50),
    ("sixty", 60),
    ("seventy", 70),
    ("eighty", 80),
    ("ninety", 90),
];

const ORDINALS: [(&str, u64); 31] = [
    ("first", 1),
    ("second", 2),
    ("third", 3),
    ("fourth", 4),
    ("fifth", 5),
    ("sixth", 6),
    ("seventh", 7),
    ("eighth", 8),
    ("ninth", 9),
    ("tenth", 10),
    ("eleventh", 11),
    ("twelfth", 12),
    ("thirteenth", 13),
    ("fourteenth", 14),
    ("fifteenth", 15),
    ("sixteenth", 16),
    ("seventeenth", 17),
    ("eighteenth", 18),
    ("nineteenth", 19),
    ("twentieth", 20),
    ("twenty-first", 21),
    ("twenty-second", 22),
    ("twenty-third", 23),
    ("twenty-fourth", 24),
    ("twenty-fifth", 25),
    ("twenty-sixth", 26),
    ("twenty-seventh", 27),
    ("twenty-eighth", 28),
    ("twenty-ninth", 29),
    ("thirtieth", 30),
    ("thirty-first", 31),
];

const CURRENCY_WORDS: [(&str, &str); 10] = [
    ("dollar", "usd"),
    ("dollars", "usd"),
    ("usd", "usd"),
    ("euro", "eur"),
    ("euros", "eur"),
    ("eur", "eur"),
    ("pound", "gbp"),
    ("pounds", "gbp"),
    ("gbp", "gbp"),
    ("yen", "jpy"),
];

fn lookup<T: Copy>(table: &[(&str, T)], word: &str) -> Option<T> {
    table.iter().find(|(w, _)| *w == word).map(|(_, v)| *v)
}

/// "1,620" -> "1620", "061" -> "61", "3.50" -> "3.5", "2.0" -> "2". `None` when not a number.
fn canonical_number(raw: &str) -> Option<String> {
    let s: String = raw.chars().filter(|c| *c != ',').collect();
    if s.is_empty()
        || !s.chars().all(|c| c.is_ascii_digit() || c == '.')
        || s.matches('.').count() > 1
    {
        return None;
    }
    if s.starts_with('.') || s.ends_with('.') {
        return None;
    }
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f.trim_end_matches('0')),
        None => (s.as_str(), ""),
    };
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    Some(if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    })
}

/// Splits a sentence into raw tokens with their sentence-initial flag. Sentence punctuation ends a
/// sentence; a date prefix ("On 9 July 2024, …") is one more clause, which is fine: its first word
/// is a starter.
fn tokens(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut initial = true;
    for raw in text.split_whitespace() {
        let ends_sentence = raw.ends_with(['.', '!', '?', ';', ':']);
        let token = raw
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '.' | ','
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '"'
                        | '\''
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                        | '—'
                        | '–'
                )
            })
            .to_string();
        if !token.is_empty() {
            out.push((token, initial));
            initial = false;
        }
        if ends_sentence {
            initial = true;
        }
    }
    out
}

/// The comparable values of a sentence (see [`SentenceValues`]).
pub fn sentence_values(text: &str) -> SentenceValues {
    let mut sv = SentenceValues::default();
    let toks = tokens(text);
    let mut i = 0;
    while i < toks.len() {
        let (raw, initial) = (&toks[i].0, toks[i].1);
        let lower = raw.to_lowercase();
        let bare = lower
            .strip_suffix("'s")
            .or_else(|| lower.strip_suffix("’s"))
            .unwrap_or(&lower)
            .to_string();
        for part in bare.split(['-', '/']) {
            if !part.is_empty() && part.chars().all(char::is_alphabetic) {
                sv.words.insert(part.to_string());
            }
        }
        sv.words.insert(bare.clone());

        // Negation, including contractions ("don't", "isn't", "can't").
        if NEGATORS.contains(&bare.as_str()) || bare.ends_with("n't") || bare.ends_with("n’t") {
            sv.negated = true;
            i += 1;
            continue;
        }

        if let Some(n) = lookup(&ORDINALS, &bare) {
            sv.values.insert(format!("num:{n}"));
            i += 1;
            continue;
        }

        // Number words, possibly compound: "four hundred", "twenty-five", "two thousand".
        if let Some(value) = number_word_run(&toks, &mut i) {
            sv.values.insert(format!("num:{value}"));
            continue;
        }

        if let Some(code) = lookup(&CURRENCY_WORDS, &bare) {
            sv.values.insert(format!("cur:{code}"));
            i += 1;
            continue;
        }
        if matches!(bare.as_str(), "percent" | "per") {
            i += 1;
            continue;
        }

        // Months ("May" only when capitalised: "may" is a verb), weekdays (singular or plural).
        let month = lookup(&MONTHS, &bare).filter(|_| bare != "may" || raw.starts_with('M'));
        if let Some(m) = month {
            sv.values.insert(format!("month:{m}"));
            i += 1;
            continue;
        }
        let weekday_word = bare.strip_suffix('s').unwrap_or(&bare);
        if let Some(d) = lookup(&WEEKDAYS, weekday_word) {
            sv.values.insert(format!("weekday:{d}"));
            i += 1;
            continue;
        }

        if raw.chars().any(|c| c.is_ascii_digit())
            || raw.starts_with(['@', '#', '$', '€', '£', '¥'])
        {
            digit_token_values(&lower, &mut sv.values);
            i += 1;
            continue;
        }

        if raw.chars().next().is_some_and(char::is_uppercase)
            && !(initial && SENTENCE_STARTERS.contains(&bare.as_str()))
            && bare != "i"
        {
            sv.values.insert(format!("name:{bare}"));
        }
        i += 1;
    }
    sv
}

/// The value of one number word ("four", "twenty", "hundred" -> marker), if it is one.
fn number_piece(piece: &str) -> Option<u64> {
    lookup(&UNITS, piece).or_else(|| lookup(&TENS, piece))
}

/// Consumes a run of number words at `i` and returns its value ("four hundred" -> 400,
/// "twenty-five" -> 25, "two thousand" -> 2000). A hyphenated word that starts with a number word
/// ("two-bedroom") gives that number and ends the run.
fn number_word_run(toks: &[(String, bool)], i: &mut usize) -> Option<u64> {
    let mut total: u64 = 0;
    let mut current: u64 = 0;
    let mut seen = false;
    let mut j = *i;
    while j < toks.len() {
        let w = toks[j].0.to_lowercase();
        let pieces: Vec<&str> = w.split('-').collect();
        let all_numeric = pieces.iter().all(|p| {
            number_piece(p).is_some() || matches!(*p, "hundred" | "thousand" | "million" | "dozen")
        });
        if !all_numeric {
            if !seen
                && pieces.len() > 1
                && let Some(v) = number_piece(pieces[0])
            {
                *i = j + 1;
                return Some(v);
            }
            break;
        }
        for piece in &pieces {
            match *piece {
                "hundred" => current = current.max(1) * 100,
                "thousand" => {
                    total += current.max(1) * 1_000;
                    current = 0;
                }
                "million" => {
                    total += current.max(1) * 1_000_000;
                    current = 0;
                }
                "dozen" => current = current.max(1) * 12,
                p => current += number_piece(p).unwrap_or(0),
            }
        }
        // a bare multiplier with nothing before it ("hundreds", "a dozen") is not a stated number
        if !seen && number_piece(pieces[0]).is_none() {
            return None;
        }
        seen = true;
        j += 1;
    }
    if !seen {
        return None;
    }
    *i = j;
    Some(total + current)
}

/// Values of a token containing a digit or starting with a currency / handle sign.
fn digit_token_values(lower: &str, out: &mut BTreeSet<String>) {
    let mut s = lower;
    for (sign, code) in [("$", "usd"), ("€", "eur"), ("£", "gbp"), ("¥", "jpy")] {
        if let Some(rest) = s.strip_prefix(sign) {
            out.insert(format!("cur:{code}"));
            s = rest;
        }
    }
    if s.starts_with(['@', '#']) || s.contains('@') {
        out.insert(format!("id:{s}"));
        return;
    }
    let s = s.strip_suffix('%').unwrap_or(s);
    // ISO date: 2024-06-16
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() == 3
        && parts[0].len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && let (Some(y), Some(m), Some(d)) = (
            canonical_number(parts[0]),
            canonical_number(parts[1]),
            canonical_number(parts[2]),
        )
    {
        out.insert(format!("num:{y}"));
        out.insert(format!("month:{m}"));
        out.insert(format!("num:{d}"));
        return;
    }
    // A small range: "6-7", "10–12".
    if let Some((a, b)) = s.split_once(['-', '–'])
        && a.len() <= 2
        && b.len() <= 2
        && let (Some(a), Some(b)) = (canonical_number(a), canonical_number(b))
    {
        out.insert(format!("num:{a}"));
        out.insert(format!("num:{b}"));
        return;
    }
    // Clock times: "9:30", "10:15", "2pm", "9:30am".
    let (clock, meridiem) = match s.strip_suffix("am").or_else(|| s.strip_suffix("pm")) {
        Some(rest) => (rest, &s[rest.len()..]),
        None => (s, ""),
    };
    if let Some((h, m)) = clock.split_once(':')
        && let (Some(h), Some(m)) = (canonical_number(h), canonical_number(m))
    {
        out.insert(format!("time:{h}:{m}{meridiem}"));
        return;
    }
    if !meridiem.is_empty()
        && let Some(h) = canonical_number(clock)
    {
        out.insert(format!("time:{h}:0{meridiem}"));
        return;
    }
    // A plain number, an ordinal ("16th"), a decade ("1970s") or a number with a unit ("10gb").
    let digits_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.'))
        .unwrap_or(s.len());
    let (num, suffix) = s.split_at(digits_end);
    if let Some(n) = canonical_number(num) {
        let suffix_is_unit = suffix.chars().all(char::is_alphabetic);
        if suffix_is_unit {
            out.insert(format!("num:{n}"));
            return;
        }
    }
    // Anything else with a digit is an identifier, compared literally ("555-0142", "maya_kowal.42").
    out.insert(format!("id:{s}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TemporalGranularity;
    use chrono::{TimeZone, Utc};

    fn compatible(candidate: &str, existing: &str) -> bool {
        value_compatibility(candidate, None, existing, None).is_ok()
    }

    fn day(y: i32, m: u32, d: u32) -> TemporalValue {
        let start = Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();
        TemporalValue {
            start,
            end: start,
            granularity: TemporalGranularity::Day,
            anchor: start,
        }
    }

    #[test]
    fn equivalent_dates_in_different_formats_are_compatible() {
        assert!(compatible(
            "The meeting takes place on the 16th of June.",
            "The meeting is on June 16."
        ));
        assert!(compatible(
            "The meeting is on June 16.",
            "The meeting is on 2024-06-16."
        ));
        assert!(compatible(
            "The meeting is on 16 June.",
            "The meeting is on the sixteenth of June."
        ));
    }

    #[test]
    fn a_different_day_month_or_year_is_a_new_value() {
        assert_eq!(
            value_compatibility(
                "Rhea's birthday is June 16.",
                None,
                "Rhea's birthday is June 14.",
                None
            ),
            Err(ValueConflict::NewValue)
        );
        assert!(!compatible(
            "The launch moved to July 16.",
            "The launch is on June 16."
        ));
        assert!(!compatible(
            "The project finished in 2023.",
            "The project started in 2019."
        ));
        assert!(!compatible(
            "In 2023, Odo ran the Lisbon half marathon.",
            "In 2019, Odo ran the Lisbon half marathon."
        ));
    }

    #[test]
    fn quantities_prices_percentages_and_measurements_are_compared_as_numbers() {
        assert!(compatible("Tomas owns two bikes.", "Tomas owns 2 bikes."));
        assert!(compatible(
            "The rent is 1620 dollars.",
            "The rent is $1,620."
        ));
        assert!(compatible(
            "The discount is 20 percent.",
            "The discount is 20%."
        ));
        assert!(compatible(
            "The trail is five kilometres long.",
            "The trail is 5 km long."
        ));
        assert!(compatible(
            "The collection has about four hundred records.",
            "The collection has 400 records."
        ));
        assert!(!compatible("The rent is $1,700.", "The rent is $1,620."));
        assert!(!compatible("The rent is €1,620.", "The rent is $1,620."));
        assert!(!compatible("The discount is 25%.", "The discount is 20%."));
        assert!(!compatible("Ines is 178 cm tall.", "Ines is 182 cm tall."));
        assert!(!compatible(
            "The team has 14 engineers.",
            "The team has 12 engineers."
        ));
        assert!(!compatible("Mara has three cats.", "Mara has two cats."));
        assert!(!compatible(
            "Mara's salary is 61,500 euros.",
            "Mara's salary is 58,000 euros."
        ));
    }

    #[test]
    fn identifiers_with_digits_are_compared_literally() {
        assert!(!compatible(
            "Pell's phone number is 555-0199.",
            "Pell's phone number is 555-0142."
        ));
        assert!(!compatible(
            "Pell's username is pell_k.43.",
            "Pell's username is pell_k.42."
        ));
        assert!(compatible(
            "Pell's phone number is 555-0142.",
            "Pell can be reached at 555-0142."
        ));
        assert!(!compatible(
            "Pell's handle is @pell.k.",
            "Pell's handle is @pellk."
        ));
    }

    #[test]
    fn times_and_weekdays_are_values() {
        assert!(!compatible(
            "The standup is at 10:15.",
            "The standup is at 9:30."
        ));
        assert!(compatible(
            "The standup is at 09:30.",
            "The standup is at 9:30."
        ));
        assert!(!compatible(
            "The book club meets on Wednesdays.",
            "The book club meets on Mondays."
        ));
        assert!(compatible(
            "The book club meets every Wednesday.",
            "The book club meets on Wednesdays."
        ));
    }

    #[test]
    fn a_new_name_is_a_new_value_but_a_lower_case_restatement_is_not() {
        assert!(!compatible(
            "Odile reports to Kemal Arslan.",
            "Odile reports to Priya Nair."
        ));
        assert!(compatible(
            "Odile is on the basic plan.",
            "Odile is on the Basic plan."
        ));
        assert!(compatible(
            "Yusra has a peanut allergy.",
            "Yusra is allergic to peanuts."
        ));
    }

    #[test]
    fn omitting_a_value_never_makes_a_candidate_distinct() {
        assert!(compatible(
            "Freya collects vinyl records.",
            "Freya has about 400 vinyl records."
        ));
        assert!(compatible(
            "Rhea's birthday is in June.",
            "Rhea's birthday is June 14."
        ));
        assert!(compatible(
            "Tamsin grows basil, mint and rosemary in the garden.",
            "Tamsin grows basil and mint in her garden."
        ));
    }

    #[test]
    fn negation_flips_are_distinct() {
        assert_eq!(
            value_compatibility(
                "Leo is not allergic to peanuts.",
                None,
                "Leo is allergic to peanuts.",
                None
            ),
            Err(ValueConflict::Polarity)
        );
        assert!(!compatible("Dev doesn't own a car.", "Dev owns a car."));
        assert!(compatible(
            "Dev doesn't own a car.",
            "Dev does not own a car."
        ));
    }

    #[test]
    fn disjoint_event_times_are_distinct_and_overlapping_ones_are_not() {
        let march = day(2024, 3, 5);
        let july = day(2024, 7, 9);
        assert_eq!(
            value_compatibility(
                "Bea donated blood.",
                Some(&july),
                "Bea donated blood.",
                Some(&march)
            ),
            Err(ValueConflict::EventTime)
        );
        let mut year = day(2024, 1, 1);
        year.end = Utc.with_ymd_and_hms(2024, 12, 31, 0, 0, 0).unwrap();
        year.granularity = TemporalGranularity::Year;
        assert!(
            value_compatibility(
                "Bea donated blood.",
                Some(&year),
                "Bea donated blood.",
                Some(&march)
            )
            .is_ok()
        );
        assert!(
            value_compatibility(
                "Bea donated blood.",
                None,
                "Bea donated blood.",
                Some(&march)
            )
            .is_ok()
        );
    }

    #[test]
    fn may_the_verb_is_not_a_month_and_sentence_starters_are_not_names() {
        assert!(compatible(
            "The user may move to a new flat.",
            "The user might move to a new flat."
        ));
        assert!(!compatible(
            "The user moves in May.",
            "The user moves in June."
        ));
        assert!(compatible(
            "Between 1 and 3 June, the user is away.",
            "The user is away 1-3 June."
        ));
        assert!(compatible(
            "The user lives in Lisbon.",
            "User lives in Lisbon."
        ));
    }

    #[test]
    fn number_parsing() {
        assert_eq!(canonical_number("1,620").as_deref(), Some("1620"));
        assert_eq!(canonical_number("061").as_deref(), Some("61"));
        assert_eq!(canonical_number("3.50").as_deref(), Some("3.5"));
        assert_eq!(canonical_number("2.0").as_deref(), Some("2"));
        assert_eq!(canonical_number("1.2.3"), None);
        let v = sentence_values("She was born in the 1970s and runs 10K races at 9:30am.");
        assert!(v.values.contains("num:1970"));
        assert!(v.values.contains("num:10"));
        assert!(v.values.contains("time:9:30am"));
    }
}
