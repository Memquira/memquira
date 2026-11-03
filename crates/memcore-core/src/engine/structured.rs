//! Windowing, bounded concurrency, and the
//! finishing step that settles each fact's session, observation time and event time.
//!
//! Only runs when `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` is on. With it off, `add_memory` makes the
//! single legacy extraction call it always has.

use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};

use super::MemoryEngine;
use super::types::{AddMemoryInput, MemorySession};
use crate::ports::{ExtractionMode, FactExtractionInput, FactExtractionOutput, MemoryMessage};
use crate::temporal::{
    TemporalOutcome, TemporalResolutionCounts, expression_occurs_in, expression_variants,
    is_duration, is_recurrence, names_time, possessive_head, settle_event_time,
};
use crate::{CandidateFact, FactStructure};

/// Default messages per extraction window (`MEMCORE_EXTRACTION_WINDOW_TURNS`).
pub const DEFAULT_EXTRACTION_WINDOW_TURNS: usize = 20;
/// Default messages repeated between consecutive windows (`MEMCORE_EXTRACTION_WINDOW_OVERLAP`).
pub const DEFAULT_EXTRACTION_WINDOW_OVERLAP: usize = 2;
/// Default windows extracted at once (`MEMCORE_EXTRACTION_CONCURRENCY`).
pub const DEFAULT_EXTRACTION_CONCURRENCY: usize = 4;

/// How structured extraction splits and schedules a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredExtractionConfig {
    pub window_turns: usize,
    pub window_overlap: usize,
    pub concurrency: usize,
}

impl Default for StructuredExtractionConfig {
    fn default() -> Self {
        Self {
            window_turns: DEFAULT_EXTRACTION_WINDOW_TURNS,
            window_overlap: DEFAULT_EXTRACTION_WINDOW_OVERLAP,
            concurrency: DEFAULT_EXTRACTION_CONCURRENCY,
        }
    }
}

impl StructuredExtractionConfig {
    pub fn validate(&self) -> MemcoreResult<()> {
        if self.window_turns == 0 {
            return Err(MemcoreError::ValidationError(
                "extraction window must hold at least one message".to_string(),
            ));
        }
        if self.window_overlap >= self.window_turns {
            return Err(MemcoreError::ValidationError(
                "extraction window overlap must be smaller than the window".to_string(),
            ));
        }
        if self.concurrency == 0 {
            return Err(MemcoreError::ValidationError(
                "extraction concurrency must be at least 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// Splits messages into windows of at most `turns`, each repeating the last `overlap` messages of
/// the previous one so a fact stated across a boundary is seen whole at least once. Duplicates that
/// the overlap produces are removed by the normal dedup step.
pub fn extraction_windows(
    messages: &[MemoryMessage],
    turns: usize,
    overlap: usize,
) -> Vec<Vec<MemoryMessage>> {
    if messages.len() <= turns {
        return vec![messages.to_vec()];
    }
    let step = turns.saturating_sub(overlap).max(1);
    let mut windows = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + turns).min(messages.len());
        windows.push(messages[start..end].to_vec());
        if end == messages.len() {
            break;
        }
        start += step;
    }
    windows
}

/// Gives every message the client's time for it: its own, else the session's.
///
/// A message with neither stays without a time (P2-20). The time memcore received the request is
/// never used: a relative expression resolved against it ("last year", ingested on some later day)
/// would become a confidently wrong date. `recorded_at` on the stored fact already records receipt.
pub(super) fn with_effective_times(
    messages: Vec<MemoryMessage>,
    session: &MemorySession,
) -> Vec<MemoryMessage> {
    messages
        .into_iter()
        .map(|mut message| {
            if message.timestamp.is_none() {
                message.timestamp = session.timestamp;
            }
            message
        })
        .collect()
}

/// Whether the time expression the model reported is in the messages the fact cites (or, when it
/// cites none that the client identified, anywhere in the request). A time taken from some other
/// message — "last night" from an earlier turn attached to a timeless opinion — is not the fact's
/// event time.
fn expression_is_grounded(expression: &str, cited: &[String], messages: &[MemoryMessage]) -> bool {
    let cited_texts: Vec<&str> = messages
        .iter()
        .filter(|message| {
            message
                .message_id
                .as_ref()
                .is_some_and(|id| cited.contains(id))
        })
        .map(|message| message.content.as_str())
        .collect();
    let texts: Vec<&str> = if cited_texts.is_empty() {
        messages
            .iter()
            .map(|message| message.content.as_str())
            .collect()
    } else {
        cited_texts
    };
    texts
        .iter()
        .any(|text| expression_occurs_in(expression, text))
}

/// Settles one candidate's structure: session, observation time and a validated event time.
fn finish_candidate(
    candidate: CandidateFact,
    messages: &[MemoryMessage],
    session: &MemorySession,
    temporal: &mut TemporalResolutionCounts,
    rewrite: TemporalRewrite,
) -> CandidateFact {
    let mut structure = candidate.structure.clone();
    // The client's time for the cited message, else the session's; `None` when the client sent
    // neither. Never the receipt time (P2-20).
    let observed_at = structure.observed_at.or(session.timestamp);
    // An expression the cited messages do not contain is treated as no expression, so a model time
    // behind it is `Ungrounded` and dropped. Under the preserving rewrite, so is a span with no
    // time in it at all ("in Room 4B").
    let grounded_text = structure.event_time_text.as_deref().filter(|expression| {
        expression_is_grounded(expression, &structure.source_message_ids, messages)
            && (rewrite == TemporalRewrite::Legacy || names_time(expression))
    });
    // A recurrence ("every Sunday", "the last Sunday of each month") is not one point in time,
    // even when it names a weekday; the preserving rewrite stores no event time for it.
    let (event_time, outcome) =
        if rewrite == TemporalRewrite::Preserving && grounded_text.is_some_and(is_recurrence) {
            (None, TemporalOutcome::NonDatable)
        } else {
            settle_event_time(structure.event_time.as_ref(), grounded_text, observed_at)
        };
    temporal.record(outcome);
    structure = FactStructure {
        event_time,
        observed_at,
        session_id: session.id.clone(),
        ..structure
    };
    let valid_at = structure.event_time.as_ref().map(|time| time.start);
    let mut candidate = candidate;
    if let Some(event_time) = &structure.event_time {
        // The stored sentence states the settled time, so a reader of the plain text — including an
        // answering model that never sees the structured fields — gets the same date the resolver
        // settled on, never the model's unchecked one.
        candidate.content = match rewrite {
            TemporalRewrite::Legacy => {
                let sentence = match structure.event_time_text.as_deref() {
                    Some(expression) => remove_time_expression(&candidate.content, expression),
                    None => candidate.content.trim().to_string(),
                };
                format!("{}{}", event_time_prefix(event_time), sentence)
            }
            TemporalRewrite::Preserving => preserving_sentence(
                &candidate.content,
                structure.event_time_text.as_deref(),
                event_time,
                matches!(
                    outcome,
                    TemporalOutcome::Agree
                        | TemporalOutcome::DeterministicOverride
                        | TemporalOutcome::DeterministicOnly
                ),
            ),
        };
    }
    let mut finished = candidate.with_structure(structure);
    // `with_structure` only ever sets `valid_at`; clear it when the model's time did not survive.
    finished.valid_at = valid_at;
    finished
}

/// "On 6 May 2023, " / "In June 2023, " / "In the week of 24 April 2023, " / "In 2023, " /
/// "Between 1 May 2023 and 20 May 2023, " — the settled time at its own precision.
pub fn event_time_prefix(event_time: &crate::TemporalValue) -> String {
    use crate::TemporalGranularity;
    let day = |time: DateTime<Utc>| time.format("%-d %B %Y").to_string();
    match event_time.granularity {
        TemporalGranularity::Day => format!("On {}, ", day(event_time.start)),
        TemporalGranularity::Week => format!("In the week of {}, ", day(event_time.start)),
        TemporalGranularity::Month => format!("In {}, ", event_time.start.format("%B %Y")),
        TemporalGranularity::Year => format!("In {}, ", event_time.start.format("%Y")),
        TemporalGranularity::Approximate => format!(
            "Between {} and {}, ",
            day(event_time.start),
            day(event_time.end)
        ),
    }
}

/// Removes the time expression the date was resolved from, once the settled date is prefixed:
/// "…went hiking in the Alps two weeks ago." would otherwise read wrongly once the conversation is in
/// the past. Models do not reliably leave it out even when asked, so it is removed here. Matching is
/// case-insensitive and whole-phrase; if the expression is not found the sentence is left as is.
///
/// A duration ("for 4 years", "two hours") is part of what the sentence says and is never removed.
/// A possessive time ("last week's game") becomes "the game", keeping the noun it qualifies.
pub fn remove_time_expression(sentence: &str, expression: &str) -> String {
    let sentence = sentence.trim();
    let Some((start, mut end)) = locate_time_expression(sentence, expression) else {
        return sentence.to_string();
    };
    let lower = sentence.to_lowercase();
    // "last week's game" -> "the game".
    let possessive = ["'s ", "\u{2019}s "]
        .iter()
        .find(|marker| lower[end..].starts_with(**marker));
    if let Some(marker) = possessive {
        end += marker.len() - 1;
    }
    let mut out = String::with_capacity(sentence.len());
    out.push_str(sentence[..start].trim_end());
    if possessive.is_some() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("the");
    }
    let rest = sentence[end..].trim_start();
    if !out.is_empty() && !rest.is_empty() && !rest.starts_with(['.', ',', '!', '?', ';', ':']) {
        out.push(' ');
    }
    out.push_str(rest);
    // Tidy what the removal can leave behind: "hiking ." and a dangling "on"/"in" before the stop.
    let mut out = out.replace(" ,", ",").replace(" .", ".").replace(",.", ".");
    for dangling in [" on.", " in.", " at.", " since."] {
        if out.ends_with(dangling) {
            out.truncate(out.len() - dangling.len());
            out.push('.');
        }
    }
    let out = out.trim_start_matches([',', ' ']).to_string();
    let mut chars = out.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => out,
    }
}

/// Where `expression` (or a variant wording of it) is in the trimmed `sentence`, as a byte range;
/// `None` for an empty expression, a duration, or one the sentence does not contain.
fn locate_time_expression(sentence: &str, expression: &str) -> Option<(usize, usize)> {
    let expression = expression.trim();
    if expression.is_empty() || is_duration(expression) {
        return None;
    }
    let expression = possessive_head(expression).unwrap_or(expression);
    let lower = sentence.to_lowercase();
    // Lowercasing can change byte lengths for some scripts; only operate when offsets line up.
    if lower.len() != sentence.len() {
        return None;
    }
    // The sentence may word the expression differently from the conversation ("last Friday" for
    // "last Fri", "five years ago" for "about five years ago").
    expression_variants(expression)
        .iter()
        .find_map(|needle| find_phrase(&lower, needle).map(|start| (start, start + needle.len())))
}

/// How the stored sentence of a fact with a settled event time is written
/// (`MEMCORE_TEMPORAL_REWRITE`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemporalRewrite {
    /// The settled date is prefixed and the model's time expression is removed from the sentence,
    /// whatever the expression is. The default and the reference behaviour.
    #[default]
    Legacy,
    /// No text other than a time expression the resolver recognised is ever
    /// removed. A span with no time in it ("in Room 4B") gives no event time; a model time the
    /// resolver could not check is prefixed but the sentence is kept whole; a date that is the
    /// value of an attribute ("the deadline is 30 May", "moved the party to next Saturday") stays
    /// in place — written dates as they are, relative ones replaced by the settled date — instead
    /// of becoming a one-off event prefix.
    Preserving,
}

/// Words after which a time expression is what the sentence states (its value), not when the rest
/// of the sentence happened: "the birthday is 14 March", "moved the party to next Saturday".
const VALUE_MARKERS: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "being", "to", "until", "till", "by", "from", "for",
    "as", "due",
];

/// Whether the expression at `start` in `sentence` is the value the sentence states. A
/// preposition of place in time ("is on 14 March", "due in May") is skipped, as is one that
/// begins the expression itself.
fn is_value_position(sentence: &str, start: usize, expression: &str) -> bool {
    let lower = sentence.to_lowercase();
    let mut before: Vec<&str> = lower[..start]
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let first_of_expression = expression
        .split(|c: char| !c.is_alphanumeric())
        .find(|word| !word.is_empty())
        .map(str::to_lowercase);
    if first_of_expression
        .as_deref()
        .is_some_and(|word| VALUE_MARKERS.contains(&word))
    {
        return true;
    }
    if before
        .last()
        .is_some_and(|word| matches!(*word, "on" | "in" | "at"))
    {
        before.pop();
    }
    before
        .last()
        .is_some_and(|word| VALUE_MARKERS.contains(word))
}

/// The settled time as it reads inside a sentence: "13 May 2023", "the week of 1 June 2026",
/// "June 2023", "2023", "between 1 May 2023 and 20 May 2023".
fn event_time_phrase(event_time: &crate::TemporalValue) -> String {
    use crate::TemporalGranularity;
    let day = |time: DateTime<Utc>| time.format("%-d %B %Y").to_string();
    match event_time.granularity {
        TemporalGranularity::Day => day(event_time.start),
        TemporalGranularity::Week => format!("the week of {}", day(event_time.start)),
        TemporalGranularity::Month => event_time.start.format("%B %Y").to_string(),
        TemporalGranularity::Year => event_time.start.format("%Y").to_string(),
        TemporalGranularity::Approximate => format!(
            "between {} and {}",
            day(event_time.start),
            day(event_time.end)
        ),
    }
}

/// The stored sentence under [`TemporalRewrite::Preserving`] for a fact whose event time was
/// settled with `outcome`. Only an expression the resolver recognised (`resolver_backed`) is ever
/// taken out of the sentence, and then only where the legacy rewrite leaves a complete sentence.
fn preserving_sentence(
    content: &str,
    expression: Option<&str>,
    event_time: &crate::TemporalValue,
    resolver_backed: bool,
) -> String {
    let sentence = content.trim();
    let located = expression.and_then(|expression| {
        locate_time_expression(sentence, expression).map(|at| (expression, at))
    });
    match located {
        Some((expression, (start, end))) if is_value_position(sentence, start, expression) => {
            if !crate::temporal::is_relative_expression(expression) {
                // A written date that is the attribute's value: right as written.
                sentence.to_string()
            } else if resolver_backed {
                // "moved the party to next Saturday" -> "moved the party to 13 May 2023".
                let mut out = String::with_capacity(sentence.len() + 16);
                out.push_str(&sentence[..start]);
                out.push_str(&event_time_phrase(event_time));
                out.push_str(&sentence[end..]);
                out
            } else {
                // A relative value the resolver could not check: its date is stated in front.
                format!("{}{}", event_time_prefix(event_time), sentence)
            }
        }
        Some((expression, _)) if resolver_backed => format!(
            "{}{}",
            event_time_prefix(event_time),
            remove_time_expression(sentence, expression)
        ),
        // The resolver could not check the expression, or it is not in the sentence: the date is
        // stated in front and nothing is removed.
        _ => format!("{}{}", event_time_prefix(event_time), sentence),
    }
}

/// Byte offset of `needle` in `haystack` where it is not part of a longer word.
fn find_phrase(haystack: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return Some(start);
        }
        from = end;
    }
    None
}

/// What structured extraction adds to the per-request summary.
#[derive(Debug, Clone, Default)]
pub(super) struct StructuredExtraction {
    pub output: FactExtractionOutput,
    pub temporal: TemporalResolutionCounts,
}

impl MemoryEngine {
    pub(super) async fn extract_structured(
        &self,
        input: &AddMemoryInput,
        messages: Vec<MemoryMessage>,
        config: StructuredExtractionConfig,
    ) -> MemcoreResult<StructuredExtraction> {
        let messages = with_effective_times(messages, &input.session);
        let windows = extraction_windows(&messages, config.window_turns, config.window_overlap);
        let window_count = windows.len();

        // `buffered` keeps results in window order while running up to `concurrency` at once.
        let results: Vec<MemcoreResult<FactExtractionOutput>> = stream::iter(windows)
            .map(|window| {
                self.llm_provider.extract_facts(FactExtractionInput {
                    tenant: input.tenant.clone(),
                    messages: window,
                    metadata: input.metadata.clone(),
                    mode: ExtractionMode::Structured,
                })
            })
            .buffered(config.concurrency)
            .collect()
            .await;

        let mut output = FactExtractionOutput::default();
        for result in results {
            output.merge(result?);
        }

        let mut temporal = TemporalResolutionCounts::default();
        output.candidates = std::mem::take(&mut output.candidates)
            .into_iter()
            .map(|candidate| {
                // The legacy prompt has the model copy the request metadata onto every fact. The
                // structured prompt never sends metadata to the model, so it is attached here —
                // exactly, rather than as a model echo — and clients see the same fact metadata
                // with the flag on as off.
                let candidate = CandidateFact {
                    metadata: input.metadata.clone(),
                    ..candidate
                };
                finish_candidate(
                    candidate,
                    &messages,
                    &input.session,
                    &mut temporal,
                    self.temporal_rewrite,
                )
            })
            .collect();

        tracing::debug!(
            event = "structured_extraction",
            windows = window_count,
            candidates = output.candidates.len(),
            temporal_agree = temporal.agree,
            temporal_deterministic_override = temporal.deterministic_override,
            temporal_llm_only = temporal.llm_only,
            invalid_source_message_ids = output.invalid_source_message_ids,
            "structured extraction finished"
        );
        Ok(StructuredExtraction { output, temporal })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::MessageRole;
    use chrono::TimeZone;

    fn messages(n: usize) -> Vec<MemoryMessage> {
        (0..n)
            .map(|index| MemoryMessage {
                role: MessageRole::User,
                content: format!("message {index}"),
                message_id: Some(format!("m{index}")),
                ..Default::default()
            })
            .collect()
    }

    fn ids(window: &[MemoryMessage]) -> Vec<String> {
        window
            .iter()
            .map(|m| m.message_id.clone().unwrap())
            .collect()
    }

    #[test]
    fn a_short_request_is_one_window() {
        let windows = extraction_windows(&messages(2), 20, 2);
        assert_eq!(windows.len(), 1);
        assert_eq!(ids(&windows[0]), vec!["m0", "m1"]);
    }

    #[test]
    fn long_requests_are_split_with_overlap_and_nothing_is_skipped() {
        let all = messages(45);
        let windows = extraction_windows(&all, 20, 2);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].len(), 20);
        assert_eq!(ids(&windows[1])[..2], ids(&windows[0])[18..]);
        assert_eq!(ids(&windows[2]).last().unwrap(), "m44");
        let mut seen: Vec<String> = windows.iter().flat_map(|w| ids(w)).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 45, "every message lands in at least one window");
    }

    #[test]
    fn config_validation() {
        assert!(StructuredExtractionConfig::default().validate().is_ok());
        for bad in [
            StructuredExtractionConfig {
                window_turns: 0,
                ..Default::default()
            },
            StructuredExtractionConfig {
                window_overlap: 20,
                ..Default::default()
            },
            StructuredExtractionConfig {
                concurrency: 0,
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn messages_get_their_own_then_the_session_time_and_never_the_receipt_time() {
        let own = Utc.with_ymd_and_hms(2023, 5, 8, 10, 0, 0).unwrap();
        let session_time = Utc.with_ymd_and_hms(2023, 5, 1, 9, 0, 0).unwrap();
        let mut input = messages(2);
        input[0].timestamp = Some(own);
        let with_session = with_effective_times(
            input.clone(),
            &MemorySession {
                id: None,
                timestamp: Some(session_time),
            },
        );
        assert_eq!(with_session[0].timestamp, Some(own));
        assert_eq!(with_session[1].timestamp, Some(session_time));
        // No client time at all: the message stays without one (P2-20).
        let without = with_effective_times(input, &MemorySession::default());
        assert_eq!(without[0].timestamp, Some(own));
        assert_eq!(without[1].timestamp, None);
    }

    #[test]
    fn a_time_phrase_the_cited_message_does_not_contain_is_dropped() {
        use crate::temporal::TemporalOutcome;
        use crate::{TemporalGranularity, TemporalValue};
        let sent = Utc.with_ymd_and_hms(2023, 8, 14, 14, 24, 0).unwrap();
        let session = MemorySession {
            id: None,
            timestamp: Some(sent),
        };
        let mut conversation = vec![
            MemoryMessage::new(MessageRole::User, "Last night we had a quiz evening!"),
            MemoryMessage::new(MessageRole::User, "Punctuality matters a lot to me."),
        ];
        conversation[0].message_id = Some("D11:1".into());
        conversation[1].message_id = Some("D11:8".into());
        let last_night = TemporalValue::new(
            Utc.with_ymd_and_hms(2023, 8, 13, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2023, 8, 13, 23, 59, 59).unwrap(),
            TemporalGranularity::Day,
            sent,
        )
        .unwrap();
        let fact = |content: &str, cites: &str| {
            CandidateFact::new(
                content,
                crate::MemoryType::Event,
                0.9,
                0.8,
                None,
                serde_json::Value::Null,
            )
            .unwrap()
            .with_structure(FactStructure {
                event_time: Some(last_night.clone()),
                event_time_text: Some("last night".into()),
                source_message_ids: vec![cites.into()],
                ..FactStructure::default()
            })
        };

        let mut counts = TemporalResolutionCounts::default();
        let opinion = finish_candidate(
            fact("Tomas values punctuality.", "D11:8"),
            &conversation,
            &session,
            &mut counts,
            TemporalRewrite::Legacy,
        );
        assert_eq!(opinion.structure.event_time, None);
        assert_eq!(opinion.valid_at, None);
        assert_eq!(opinion.content, "Tomas values punctuality.");
        assert_eq!(counts.get(TemporalOutcome::Ungrounded), 1);

        let concert = finish_candidate(
            fact("Tomas went to a quiz evening last night.", "D11:1"),
            &conversation,
            &session,
            &mut counts,
            TemporalRewrite::Legacy,
        );
        assert_eq!(
            concert.content,
            "On 13 August 2023, Tomas went to a quiz evening."
        );
        assert_eq!(counts.get(TemporalOutcome::Agree), 1);
    }

    /// P2-20: with no client time, a relative expression gets no event time, whatever the model
    /// said, and the sentence stays as written; an explicit date is still settled.
    #[test]
    fn without_a_client_time_relative_dates_stay_unknown_and_explicit_dates_are_kept() {
        use crate::temporal::TemporalOutcome;
        use crate::{TemporalGranularity, TemporalValue};
        let mut first = MemoryMessage::new(MessageRole::User, "I adopted a dog last year.");
        first.message_id = Some("m1".to_string());
        let mut second = MemoryMessage::new(MessageRole::User, "We married on 8 May 2023.");
        second.message_id = Some("m2".to_string());
        let conversation = with_effective_times(vec![first, second], &MemorySession::default());
        assert!(conversation.iter().all(|m| m.timestamp.is_none()));

        // What a model anchored to "now" would say for "last year".
        let guessed = TemporalValue::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap(),
            TemporalGranularity::Year,
            Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();
        let candidate = |content: &str, id: &str, text: &str, time: Option<TemporalValue>| {
            CandidateFact::new(
                content,
                crate::MemoryType::Event,
                0.9,
                0.8,
                None,
                serde_json::Value::Null,
            )
            .unwrap()
            .with_structure(FactStructure {
                event_time: time,
                event_time_text: Some(text.to_string()),
                source_message_ids: vec![id.to_string()],
                ..FactStructure::default()
            })
        };
        let mut counts = TemporalResolutionCounts::default();
        let dog = finish_candidate(
            candidate(
                "Priya adopted a dog last year.",
                "m1",
                "last year",
                Some(guessed),
            ),
            &conversation,
            &MemorySession::default(),
            &mut counts,
            TemporalRewrite::Legacy,
        );
        assert_eq!(dog.structure.event_time, None);
        assert_eq!(dog.valid_at, None);
        assert_eq!(dog.structure.observed_at, None);
        assert_eq!(dog.structure.event_time_text.as_deref(), Some("last year"));
        assert_eq!(dog.content, "Priya adopted a dog last year.");
        assert_eq!(counts.get(TemporalOutcome::Unanchored), 1);

        let wedding = finish_candidate(
            candidate("Priya married on 8 May 2023.", "m2", "8 May 2023", None),
            &conversation,
            &MemorySession::default(),
            &mut counts,
            TemporalRewrite::Legacy,
        );
        let time = wedding
            .structure
            .event_time
            .as_ref()
            .expect("explicit date kept");
        assert_eq!(time.start.date_naive().to_string(), "2023-05-08");
        assert_eq!(wedding.valid_at, Some(time.start));
        assert_eq!(wedding.structure.observed_at, None);
        assert_eq!(wedding.content, "On 8 May 2023, Priya married.");
        assert_eq!(counts.get(TemporalOutcome::DeterministicOnly), 1);
    }

    #[test]
    fn event_time_prefix_renders_each_granularity() {
        use crate::{TemporalGranularity as G, TemporalValue};
        let at = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();
        let value = |start, end, g| TemporalValue::new(start, end, g, at(2023, 5, 8)).unwrap();
        assert_eq!(
            event_time_prefix(&value(at(2023, 5, 6), at(2023, 5, 6), G::Day)),
            "On 6 May 2023, "
        );
        assert_eq!(
            event_time_prefix(&value(at(2023, 6, 1), at(2023, 6, 30), G::Month)),
            "In June 2023, "
        );
        assert_eq!(
            event_time_prefix(&value(at(2023, 4, 24), at(2023, 4, 30), G::Week)),
            "In the week of 24 April 2023, "
        );
        assert_eq!(
            event_time_prefix(&value(at(2022, 1, 1), at(2022, 12, 31), G::Year)),
            "In 2022, "
        );
        assert_eq!(
            event_time_prefix(&value(at(2023, 5, 1), at(2023, 5, 20), G::Approximate)),
            "Between 1 May 2023 and 20 May 2023, "
        );
    }

    #[test]
    fn the_resolved_expression_is_removed_from_the_sentence() {
        for (sentence, expression, expected) in [
            (
                "Tomas went hiking in the Alps two weeks ago.",
                "two weeks ago",
                "Tomas went hiking in the Alps.",
            ),
            (
                "Priya starts a pottery class next month.",
                "next month",
                "Priya starts a pottery class.",
            ),
            (
                "Last Saturday Priya adopted a dog.",
                "Last Saturday",
                "Priya adopted a dog.",
            ),
            (
                "Priya adopted a dog on last Saturday.",
                "last Saturday",
                "Priya adopted a dog.",
            ),
            ("Priya adopted a dog.", "yesterday", "Priya adopted a dog."),
            (
                "Tomas fixed the old bike last Friday.",
                "last Fri",
                "Tomas fixed the old bike.",
            ),
            (
                "Tomas took up the cello five years ago.",
                "about five years ago",
                "Tomas took up the cello.",
            ),
            // a duration is part of what the sentence says, so it stays
            (
                "Tomas has lived in Lisbon for 4 years.",
                "4 years",
                "Tomas has lived in Lisbon for 4 years.",
            ),
            (
                "Mayday parade was fun in May 2023.",
                "May 2023",
                "Mayday parade was fun.",
            ),
        ] {
            assert_eq!(
                remove_time_expression(sentence, expression),
                expected,
                "{sentence:?}"
            );
        }
    }

    #[test]
    fn only_whole_phrases_are_removed() {
        assert_eq!(
            remove_time_expression("The Mayfair event was great.", "may"),
            "The Mayfair event was great."
        );
    }

    #[test]
    fn a_possessive_time_keeps_its_noun() {
        for expression in ["last week's", "last week's match", "last week"] {
            assert_eq!(
                remove_time_expression("Omar scored twice in last week's match.", expression),
                "Omar scored twice in the match.",
                "{expression:?}"
            );
        }
        assert_eq!(
            remove_time_expression("Last week's match went to extra time.", "last week's"),
            "The match went to extra time."
        );
    }

    #[test]
    fn hour_and_minute_durations_stay_in_the_sentence() {
        assert_eq!(
            remove_time_expression("The repair took two hours.", "two hours"),
            "The repair took two hours."
        );
        assert_eq!(
            remove_time_expression("Lena ran for 45 minutes.", "45 minutes"),
            "Lena ran for 45 minutes."
        );
    }

    /// One dated candidate through `finish_candidate` under a rewrite mode. The
    /// conversation is the single message `said`, sent on Monday 8 May 2023.
    fn finish_dated(
        rewrite: TemporalRewrite,
        said: &str,
        content: &str,
        model: Option<(&str, &str, crate::TemporalGranularity)>,
        expression: &str,
    ) -> (CandidateFact, TemporalResolutionCounts) {
        use crate::TemporalValue;
        let sent = Utc.with_ymd_and_hms(2023, 5, 8, 10, 0, 0).unwrap();
        let mut message = MemoryMessage::new(MessageRole::User, said);
        message.message_id = Some("m1".into());
        message.timestamp = Some(sent);
        let day = |text: &str, end: bool| {
            let date = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap();
            let time = if end {
                date.and_hms_opt(23, 59, 59)
            } else {
                date.and_hms_opt(0, 0, 0)
            };
            Utc.from_utc_datetime(&time.unwrap())
        };
        let model_time = model.map(|(start, end, granularity)| {
            TemporalValue::new(day(start, false), day(end, true), granularity, sent).unwrap()
        });
        let candidate = CandidateFact::new(
            content,
            crate::MemoryType::Event,
            0.9,
            0.8,
            None,
            serde_json::Value::Null,
        )
        .unwrap()
        .with_structure(FactStructure {
            event_time: model_time,
            event_time_text: Some(expression.to_string()),
            source_message_ids: vec!["m1".into()],
            ..FactStructure::default()
        });
        let session = MemorySession {
            id: None,
            timestamp: Some(sent),
        };
        let mut counts = TemporalResolutionCounts::default();
        let finished = finish_candidate(candidate, &[message], &session, &mut counts, rewrite);
        (finished, counts)
    }

    /// The V2.2 defect (D1): the legacy rewrite removes whatever span the model calls the time
    /// expression, so "in Room 4B" is deleted and a date the conversation never gave is prefixed.
    /// The preserving rewrite stores the sentence whole and no event time.
    #[test]
    fn a_span_with_no_time_in_it_is_never_removed_or_dated() {
        use crate::TemporalGranularity::Day;
        use crate::temporal::TemporalOutcome;
        let said = "The project kickoff will be in Room 4B.";
        let model = Some(("2023-05-08", "2023-05-08", Day));
        let (legacy, _) = finish_dated(TemporalRewrite::Legacy, said, said, model, "in Room 4B");
        assert_eq!(
            legacy.content,
            "On 8 May 2023, The project kickoff will be."
        );
        let (kept, counts) =
            finish_dated(TemporalRewrite::Preserving, said, said, model, "in Room 4B");
        assert_eq!(kept.content, "The project kickoff will be in Room 4B.");
        assert_eq!(kept.structure.event_time, None);
        assert_eq!(kept.valid_at, None);
        assert_eq!(counts.get(TemporalOutcome::Ungrounded), 1);
        // A flight number and a gate are not times either.
        let flight = "Flight BA2490 leaves from gate 12.";
        let (kept, _) = finish_dated(TemporalRewrite::Preserving, flight, flight, model, "BA2490");
        assert_eq!(kept.content, flight);
        assert_eq!(kept.structure.event_time, None);
    }

    /// An expression the resolver does not cover keeps the model's (validated) date, stated in
    /// front, and the sentence loses nothing.
    #[test]
    fn an_unchecked_expression_is_prefixed_but_not_removed() {
        use crate::TemporalGranularity::{Approximate, Month};
        use crate::temporal::TemporalOutcome;
        let said = "The book club switched to the last Sunday of each month.";
        let model = Some(("2023-05-01", "2023-05-31", Month));
        let expression = "the last Sunday of each month";
        let (legacy, _) = finish_dated(TemporalRewrite::Legacy, said, said, model, expression);
        assert_eq!(legacy.content, "In May 2023, The book club switched to.");
        // A recurrence is no single date: no event time, and the sentence is kept whole.
        let (kept, counts) =
            finish_dated(TemporalRewrite::Preserving, said, said, model, expression);
        assert_eq!(kept.content, said);
        assert_eq!(kept.structure.event_time, None);
        assert_eq!(counts.get(TemporalOutcome::NonDatable), 1);

        let museum = "Tomas visited the museum the week after Easter.";
        let model = Some(("2023-04-10", "2023-04-16", Approximate));
        let (kept, counts) = finish_dated(
            TemporalRewrite::Preserving,
            museum,
            museum,
            model,
            "the week after Easter",
        );
        assert_eq!(counts.get(TemporalOutcome::LlmOnly), 1);
        assert_eq!(
            kept.content,
            "Between 10 April 2023 and 16 April 2023, Tomas visited the museum the week after Easter."
        );
    }

    /// A date that is what the sentence states (a birthday, a deadline, the new date of a moved
    /// event) is the attribute's value, not when the fact happened: a written date stays as
    /// written, a relative one is replaced in place by the settled date. The event time is kept.
    #[test]
    fn a_date_that_is_an_attributes_value_stays_in_the_sentence() {
        use crate::TemporalGranularity::{Day, Month};
        for (said, content, model, expression, legacy_text, preserving_text) in [
            (
                "My birthday is on 14 March.",
                "Priya's birthday is on 14 March.",
                ("2023-03-14", "2023-03-14", Day),
                "14 March",
                "On 14 March 2023, Priya's birthday is.",
                "Priya's birthday is on 14 March.",
            ),
            (
                "The grant application is due on 30 May.",
                "The grant application is due on 30 May.",
                ("2023-05-30", "2023-05-30", Day),
                "30 May",
                "On 30 May 2023, The grant application is due.",
                "The grant application is due on 30 May.",
            ),
            (
                "The trip moved from 3 June to 10 June.",
                "The trip moved from 3 June to 10 June.",
                ("2023-06-10", "2023-06-10", Day),
                "10 June",
                "On 10 June 2023, The trip moved from 3 June to.",
                "The trip moved from 3 June to 10 June.",
            ),
            (
                "The beta launch is planned for next month.",
                "The beta launch is planned for next month.",
                ("2023-06-01", "2023-06-30", Month),
                "next month",
                "In June 2023, The beta launch is planned for.",
                "The beta launch is planned for June 2023.",
            ),
            (
                "The dentist appointment is on next Friday.",
                "The dentist appointment is on next Friday.",
                ("2023-05-12", "2023-05-12", Day),
                "next Friday",
                "On 12 May 2023, The dentist appointment is.",
                "The dentist appointment is on 12 May 2023.",
            ),
            (
                "We moved mum's party to next Saturday so my brother can come.",
                "The user moved their mum's party to next Saturday so their brother can come.",
                ("2023-05-13", "2023-05-13", Day),
                "next Saturday",
                "On 13 May 2023, The user moved their mum's party to so their brother can come.",
                "The user moved their mum's party to 13 May 2023 so their brother can come.",
            ),
        ] {
            let (legacy, _) = finish_dated(
                TemporalRewrite::Legacy,
                said,
                content,
                Some(model),
                expression,
            );
            assert_eq!(legacy.content, legacy_text, "legacy {said:?}");
            let (kept, _) = finish_dated(
                TemporalRewrite::Preserving,
                said,
                content,
                Some(model),
                expression,
            );
            assert_eq!(kept.content, preserving_text, "preserving {said:?}");
            let time = kept.structure.event_time.as_ref().expect("event time kept");
            assert_eq!(time.start.date_naive().to_string(), model.0, "{said:?}");
            assert_eq!(kept.valid_at, Some(time.start));
        }
    }

    /// Where the legacy rewrite leaves a complete sentence (a relative or written time that is
    /// not the sentence's value) the preserving rewrite produces exactly the same text.
    #[test]
    fn the_preserving_rewrite_keeps_the_legacy_text_where_nothing_is_lost() {
        use crate::TemporalGranularity::{Day, Week, Year};
        for (said, model, expression) in [
            (
                "Tomas went hiking in the Alps two weeks ago.",
                ("2023-04-24", "2023-04-24", Day),
                "two weeks ago",
            ),
            (
                "Last Saturday Priya adopted a dog.",
                ("2023-05-06", "2023-05-06", Day),
                "Last Saturday",
            ),
            (
                "Priya moved to Lisbon in 2019.",
                ("2019-01-01", "2019-12-31", Year),
                "2019",
            ),
            (
                "Omar scored twice in last week's match.",
                ("2023-05-01", "2023-05-07", Week),
                "last week's",
            ),
        ] {
            let (legacy, _) =
                finish_dated(TemporalRewrite::Legacy, said, said, Some(model), expression);
            let (kept, _) = finish_dated(
                TemporalRewrite::Preserving,
                said,
                said,
                Some(model),
                expression,
            );
            assert_eq!(kept.content, legacy.content, "{said:?}");
            assert_eq!(kept.structure.event_time, legacy.structure.event_time);
            // The expression is gone from the sentence itself (the prefix states the date).
            let (_, rest) = kept.content.split_once(", ").expect("date prefix");
            assert!(
                !rest.to_lowercase().contains(&expression.to_lowercase()),
                "{said:?}"
            );
        }
    }

    /// Numbers that are not dates, and expressions that cannot be dated, get no event time and
    /// keep their sentence, in both modes.
    #[test]
    fn numbers_and_undatable_expressions_keep_their_sentence() {
        use crate::TemporalGranularity::Day;
        let model = Some(("2023-05-08", "2023-05-08", Day));
        for (said, expression) in [
            ("The team has 12 members.", "12"),
            ("Tomas has lived in Lisbon for 4 years.", "for 4 years"),
        ] {
            for rewrite in [TemporalRewrite::Legacy, TemporalRewrite::Preserving] {
                let (kept, _) = finish_dated(rewrite, said, said, model, expression);
                assert_eq!(kept.content, said, "{rewrite:?} {said:?}");
            }
        }
        // A weekday recurrence: the legacy rewrite dates it to the message day and deletes it.
        let said = "Priya goes climbing every Sunday.";
        let (legacy, _) = finish_dated(TemporalRewrite::Legacy, said, said, model, "every Sunday");
        assert_eq!(legacy.content, "On 8 May 2023, Priya goes climbing.");
        let (kept, _) = finish_dated(
            TemporalRewrite::Preserving,
            said,
            said,
            model,
            "every Sunday",
        );
        assert_eq!(kept.content, said);
        assert_eq!(kept.structure.event_time, None);
    }

    #[test]
    fn time_vocabulary_and_relative_expressions_are_recognised() {
        use crate::temporal::{is_relative_expression, names_time};
        for expression in [
            "yesterday",
            "14 March",
            "March",
            "2019",
            "two weeks ago",
            "the 3rd",
            "9:30",
            "12/05/2023",
            "5pm",
            "this summer",
            "last Sunday of each month",
            "every week",
            "when I was 10",
            "the week after Easter",
        ] {
            assert!(names_time(expression), "{expression:?}");
        }
        for expression in ["in Room 4B", "BA2490", "gate 12", "the blue one", "Level 4"] {
            assert!(!names_time(expression), "{expression:?}");
        }
        for expression in [
            "next Friday",
            "two weeks ago",
            "on Saturday",
            "this May",
            "tomorrow",
        ] {
            assert!(is_relative_expression(expression), "{expression:?}");
        }
        for expression in ["14 March", "May 2023", "2019", "Saturday 13 May", "30 May"] {
            assert!(!is_relative_expression(expression), "{expression:?}");
        }
    }
}
