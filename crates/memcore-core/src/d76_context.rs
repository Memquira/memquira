//! Server-side context: the context-assembly contract of the reference configuration, built by the
//! server.
//!
//! - Candidates: semantic facts @100 and episodes @100 merged by raw `vector_score` with the frozen
//!   tie order (`candidates::merge_by_vector_score`, the documented merge order).
//!   With episodes off the context is facts only, in search order (the facts-only form).
//! - Line: `"<timestamp>: <text>"`. A fact's text is its content; an episode's is
//!   `"<speaker>: <content>"` (an empty speaker when none). The timestamp is the session time in the
//!   conversational display form, e.g. `4:15 pm on 12 March, 2024`.
//! - Budget: the rank-order prefix. Lines are kept in order while the newline-joined text of the
//!   kept lines still fits `token_budget` cl100k_base tokens; the first line that does not fit ends
//!   the context. Nothing is skipped, truncated or refilled.
//!
//! Everything here is a byte-for-byte port of `eval_episode_context.py`, `context_budget.py` and
//! the memcore adapter's timestamp rendering.

use std::sync::OnceLock;

use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiktoken_rs::CoreBPE;
use uuid::Uuid;

use crate::candidates::{Candidate, CandidateKind};

/// Default per-user context budget in cl100k_base tokens.
pub const D76_DEFAULT_TOKEN_BUDGET: usize = 1_500;
/// Largest accepted `token_budget`.
pub const D76_MAX_TOKEN_BUDGET: usize = 4_000;
/// Retrieval depth for each leg (facts and episodes).
pub const D76_CANDIDATE_DEPTH: usize = 100;

/// What the harness renders when an item has no timestamp (`f"{None}: ..."` in Python).
const MISSING_TIMESTAMP: &str = "None";

fn tokenizer() -> &'static CoreBPE {
    static BPE: OnceLock<CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::cl100k_base().expect("embedded cl100k_base ranks load"))
}

/// cl100k_base token count, as `tiktoken.get_encoding("cl100k_base").encode(text)` counts it
/// (special-token text is counted as ordinary text).
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    tokenizer().encode_ordinary(text).len()
}

/// The conversational session-time display, e.g. `4:15 pm on 12 March, 2024`: 12-hour clock without hour
/// padding, two-digit minutes, lower-case am / pm, unpadded day, English month name. Rendered in
/// the timestamp's own offset.
pub fn display_timestamp<Tz: TimeZone>(time: &DateTime<Tz>) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let hour = match time.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    let meridiem = if time.hour() < 12 { "am" } else { "pm" };
    format!(
        "{hour}:{:02} {meridiem} on {} {}, {}",
        time.minute(),
        time.day(),
        MONTHS[time.month0() as usize],
        time.year()
    )
}

/// The timestamp text of one candidate, as the harness rendered it:
/// - fact: `metadata.timestamp` (the session time the client sent) in display form; a value that is
///   not an RFC 3339 time is used as given; none at all renders `None`;
/// - episode: the message time (`observed_at`) in display form; none renders `None`.
pub fn timestamp_text(candidate: &Candidate) -> String {
    match candidate.kind {
        CandidateKind::Fact => match candidate.metadata.get("timestamp") {
            Some(Value::String(raw)) => DateTime::<FixedOffset>::parse_from_rfc3339(raw)
                .map(|time| display_timestamp(&time))
                .unwrap_or_else(|_| raw.clone()),
            Some(Value::Null) | None => MISSING_TIMESTAMP.to_string(),
            Some(other) => other.to_string(),
        },
        CandidateKind::Episode => candidate
            .observed_at
            .map(|time| display_timestamp(&time))
            .unwrap_or_else(|| MISSING_TIMESTAMP.to_string()),
    }
}

/// The text after the timestamp: a fact's content, or `"<speaker>: <content>"` for an episode.
pub fn item_text(candidate: &Candidate) -> String {
    match candidate.kind {
        CandidateKind::Fact => candidate.content.clone(),
        CandidateKind::Episode => format!(
            "{}: {}",
            candidate.speaker.as_deref().unwrap_or(""),
            candidate.content
        ),
    }
}

/// One line of the context, ready to render.
#[derive(Debug, Clone, PartialEq)]
pub struct D76Line {
    pub kind: CandidateKind,
    pub id: Uuid,
    pub vector_score: f32,
    pub timestamp: String,
    pub text: String,
}

impl D76Line {
    pub fn from_candidate(candidate: &Candidate) -> Self {
        Self {
            kind: candidate.kind,
            id: candidate.id,
            vector_score: candidate.vector_score,
            timestamp: timestamp_text(candidate),
            text: item_text(candidate),
        }
    }

    pub fn line(&self) -> String {
        format!("{}: {}", self.timestamp, self.text)
    }
}

/// One selected item of a rendered context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct D76ContextItem {
    pub kind: CandidateKind,
    pub id: Uuid,
    pub vector_score: f32,
    pub timestamp: String,
    pub text: String,
    pub line: String,
    /// cl100k_base tokens of this line on its own (the context total counts the joined text).
    pub tokens: usize,
}

/// A rendered server-side context for one user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct D76Context {
    /// The kept lines joined with `\n`.
    pub text: String,
    /// cl100k_base tokens of `text` (the joined text, as the harness counts the budget).
    pub token_count: usize,
    pub items: Vec<D76ContextItem>,
    /// True when a candidate did not fit, i.e. the budget, not the candidate list, ended the context.
    pub budget_bound: bool,
    pub candidates_considered: usize,
}

/// How [`render_packed`] fits the ranked lines into the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum D76Packing {
    /// The rank-order prefix: the first line that does not fit ends the context.
    #[default]
    Prefix,
    /// Opt-in: a line that does not fit is skipped and the later lines are still
    /// tried, in rank order, against the same budget. Nothing is truncated or reordered.
    Fill,
    /// Opt-in (Retrieval Quality): `Fill` over a fact-anchored order ([`render_evidence`]): a fact's
    /// own source turns, when retrieved and short, follow the fact; a fact whose source turns are
    /// all already in the context is skipped. Needs the candidates' provenance, so
    /// [`render_packed`] alone treats it as `Fill`.
    Evidence,
}

/// Input of `MemoryEngine::d76_context`.
#[derive(Debug, Clone, PartialEq)]
pub struct D76ContextInput {
    pub tenant: crate::TenantContext,
    pub query: String,
    pub token_budget: usize,
}

/// Output of `MemoryEngine::d76_context`.
#[derive(Debug, Clone, PartialEq)]
pub struct D76ContextOutput {
    pub context: D76Context,
    /// False when episodes are disabled: the context is the facts-only (C0) form.
    pub episodes_included: bool,
    pub token_budget: usize,
}

/// Output of `MemoryEngine::d76_context_with_packing`: the context as `d76_context` returns it,
/// the packing used and the lines `Fill` skipped because they did not fit (always 0 for `Prefix`).
#[derive(Debug, Clone, PartialEq)]
pub struct D76PackedContextOutput {
    pub output: D76ContextOutput,
    pub packing: D76Packing,
    pub lines_skipped: usize,
    /// Episode candidates removed by `D76ContextOptions::exclude_message_ids` before
    /// packing (0 without exclusions).
    pub excluded_candidates: usize,
    /// Provenance of each selected item, aligned with `output.context.items`.
    pub item_provenance: Vec<D76ItemProvenance>,
}

/// Where a selected context item came from - the client message ids a fact was
/// extracted from, or the client message id an episode stores. Taken from the candidate as
/// retrieved; never inferred.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct D76ItemProvenance {
    pub message_id: Option<String>,
    pub source_message_ids: Vec<String>,
}

/// Per-request options of the server-side context beyond packing. The default is the validated
/// reference behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct D76ContextOptions {
    /// Client message ids whose **episodes** are left out of this context, e.g. messages already
    /// present in another memory user's context of the same answer prompt (a conversation stored
    /// for both participants keeps each message as an episode of both). Only episodes whose
    /// `message_id` matches exactly are removed; facts are never removed. The prefix (or fill)
    /// rule then continues over the remaining candidates, so the freed budget holds other memories.
    pub exclude_message_ids: Vec<String>,
}

/// Largest accepted `exclude_message_ids` list, and the longest id (the API's message-id limit).
pub const D76_MAX_EXCLUDED_MESSAGE_IDS: usize = 2_000;
pub const D76_MAX_MESSAGE_ID_CHARS: usize = 200;

/// Removes the episode candidates whose `message_id` is in `exclude`, keeping every
/// fact and the order of everything kept; returns the kept candidates and how many were removed.
/// Exact identity only: no similarity, no normalisation.
pub fn exclude_episodes_by_message_id(
    candidates: Vec<Candidate>,
    exclude: &[String],
) -> (Vec<Candidate>, usize) {
    if exclude.is_empty() {
        return (candidates, 0);
    }
    let set: std::collections::HashSet<&str> = exclude.iter().map(String::as_str).collect();
    let before = candidates.len();
    let kept: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| {
            !(c.kind == CandidateKind::Episode
                && c.message_id.as_deref().is_some_and(|m| set.contains(m)))
        })
        .collect();
    let removed = before - kept.len();
    (kept, removed)
}

/// The provenance of each rendered item, looked up by candidate id.
pub fn item_provenance(context: &D76Context, candidates: &[Candidate]) -> Vec<D76ItemProvenance> {
    let by_id: std::collections::HashMap<Uuid, &Candidate> =
        candidates.iter().map(|c| (c.id, c)).collect();
    context
        .items
        .iter()
        .map(|item| match by_id.get(&item.id) {
            Some(c) => D76ItemProvenance {
                message_id: c.message_id.clone(),
                source_message_ids: c.source_message_ids.clone(),
            },
            None => D76ItemProvenance::default(),
        })
        .collect()
}

/// True when no cl100k pre-tokenizer chunk can span from the preceding `\n` into this line, so
/// the joined text's token count splits at the line start (see `render`).
fn starts_independently(line: &str) -> bool {
    line.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
}

/// cl100k tokens of `text` followed by `\n`, given `tokens` = the tokens of `text`. When the text ends with an
/// ASCII letter or digit, no cl100k pre-tokenizer chunk can take the `\n` along (the letter, digit and contraction
/// patterns never match a newline, and the punctuation pattern needs a non-alphanumeric run before it), so the `\n`
/// is a chunk of its own, one token. Otherwise it is counted.
fn tokens_with_newline(text: &str, tokens: usize) -> usize {
    if text
        .as_bytes()
        .last()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        tokens + 1
    } else {
        count_tokens(&format!("{text}\n"))
    }
}

/// The rank-order prefix over `lines` at `token_budget` (`context_budget.truncate`).
///
/// The harness re-tokenises the joined kept lines for every candidate. The same count is computed
/// here incrementally: cl100k's pre-tokenizer never forms a chunk that crosses into a line starting
/// with an ASCII letter or digit (the `\n` before it ends a chunk), so `count(l1\n..\nlk)` equals
/// `count(l1\n) + .. + count(lk-1\n) + count(lk)`. A line starting with anything else falls back to
/// counting the full joined text. Both paths give the harness's numbers exactly.
pub fn render(lines: &[D76Line], token_budget: usize) -> D76Context {
    render_packed(lines, token_budget, D76Packing::Prefix).0
}

/// [`render`] with an explicit packing; also returns the number of lines `Fill` skipped. `Prefix`
/// is exactly [`render`].
///
/// Fill performance: each line is tokenized at most once on its own (its count is reused for the item and for the
/// running total), the joined text is extended in place only when a line is kept, and `Fill` skips a line whose
/// token lower bound already breaks the budget before tokenizing it. The decisions, the kept lines, their order,
/// the text and every count are exactly those of the frozen loop (`render_packed_reference` in the tests).
pub fn render_packed(
    lines: &[D76Line],
    token_budget: usize,
    packing: D76Packing,
) -> (D76Context, usize) {
    render_lines(lines, token_budget, packing, &[], |_| true)
}

/// What [`render_lines`] asks its gate: may line `i` be tried, and line `i` was kept.
enum Gate {
    Admit(usize),
    Kept(usize),
}

/// The packing loop of [`render_packed`]. `gate(Gate::Admit(i))` returning false skips line `i`
/// before it is counted (a skipped line, not a budget decision); `gate(Gate::Kept(i))` reports a
/// kept line. `known[i]`, when present, is line `i`'s own token count, already computed. With no
/// known counts and a gate that admits everything this is exactly `render_packed`.
fn render_lines(
    lines: &[D76Line],
    token_budget: usize,
    packing: D76Packing,
    known: &[Option<usize>],
    mut gate: impl FnMut(Gate) -> bool,
) -> (D76Context, usize) {
    let fill = packing != D76Packing::Prefix;
    let mut lines_skipped = 0usize;
    let mut items: Vec<D76ContextItem> = Vec::new();
    let mut joined = String::new();
    // Tokens of the joined kept text followed by `\n`.
    let mut sum_with_newlines = 0usize;
    let mut token_count = 0usize;
    let mut budget_bound = false;
    // `Fill` only: whitespace-separated runs of the kept text, a lower bound on its tokens.
    let mut runs_kept = 0usize;

    for (index, line) in lines.iter().enumerate() {
        if !gate(Gate::Admit(index)) {
            lines_skipped += 1;
            continue;
        }
        let text = line.line();
        // The joined text's count splits at this line's start (see `render`).
        let independent = items.is_empty() || starts_independently(&text);
        let mut runs = 0usize;
        if fill {
            // No cl100k pre-tokenizer chunk holds two whitespace-separated runs, so a text has at
            // least as many tokens as runs. A line is skipped without tokenizing it when a lower bound
            // on the context's tokens with it already breaks the budget (the same decision as
            // counting): the kept text's tokens plus this line's runs for a line that splits at its
            // start, else the runs of the whole joined text.
            runs = text.split_whitespace().count();
            let floor = if independent {
                sum_with_newlines + runs
            } else {
                runs_kept + runs
            };
            if floor > token_budget {
                budget_bound = true;
                lines_skipped += 1;
                continue;
            }
        }
        let mut line_tokens = None;
        let mut candidate_text = None;
        let candidate_count = if independent {
            let tokens = known
                .get(index)
                .copied()
                .flatten()
                .unwrap_or_else(|| count_tokens(&text));
            line_tokens = Some(tokens);
            sum_with_newlines + tokens
        } else {
            let with_line = format!("{joined}\n{text}");
            let count = count_tokens(&with_line);
            candidate_text = Some(with_line);
            count
        };
        if candidate_count > token_budget {
            budget_bound = true;
            if !fill {
                break;
            }
            lines_skipped += 1;
            continue;
        }
        let tokens = line_tokens
            .or_else(|| known.get(index).copied().flatten())
            .unwrap_or_else(|| count_tokens(&text));
        sum_with_newlines = match &candidate_text {
            None => sum_with_newlines + tokens_with_newline(&text, tokens),
            Some(with_line) => count_tokens(&format!("{with_line}\n")),
        };
        token_count = candidate_count;
        runs_kept += runs;
        match candidate_text {
            Some(with_line) => joined = with_line,
            None => {
                if !joined.is_empty() {
                    joined.push('\n');
                }
                joined.push_str(&text);
            }
        }
        items.push(D76ContextItem {
            kind: line.kind,
            id: line.id,
            vector_score: line.vector_score,
            timestamp: line.timestamp.clone(),
            text: line.text.clone(),
            line: text,
            tokens,
        });
        gate(Gate::Kept(index));
    }

    let context = D76Context {
        text: joined,
        token_count,
        items,
        budget_bound,
        candidates_considered: lines.len(),
    };
    (context, lines_skipped)
}

/// Opt-in `Evidence` packing over the merged candidates (Retrieval Quality sprint).
///
/// Facts are short and match questions well; the turn a fact was extracted from often ranks far
/// lower on its own vector (a long turn's vector is diluted), and only that turn is the evidence.
/// So, walking the merged order, each fact is followed by the retrieved episodes it cites (same
/// session, `message_id` in `source_message_ids`, not already placed) whose line has at most
/// `token_budget / 5` tokens, so a promoted turn cannot crowd the context. The order is then packed
/// as `Fill`, skipping a fact once every one of its source turns is kept (the turn holds the
/// fact). Nothing is retrieved, truncated or rewritten: the context is a subset of the candidates
/// with their exact lines; only order and redundant facts change.
pub fn render_evidence(candidates: &[Candidate], token_budget: usize) -> (D76Context, usize) {
    use std::collections::{HashMap, HashSet};

    fn turn(c: &Candidate) -> Option<(&str, &str)> {
        match (c.kind, &c.session_id, &c.message_id) {
            (CandidateKind::Episode, Some(session), Some(message)) => {
                Some((session.as_str(), message.as_str()))
            }
            _ => None,
        }
    }

    let guard = token_budget / 5;
    let episode_at: HashMap<(&str, &str), usize> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, c)| turn(c).map(|key| (key, index)))
        .collect();
    let lines: Vec<D76Line> = candidates.iter().map(D76Line::from_candidate).collect();
    let mut tokens: Vec<Option<usize>> = vec![None; candidates.len()];
    // `tokens <= guard`, deciding by cheap bounds first: a text has at least as many tokens as
    // whitespace-separated runs and at most as many as bytes.
    let mut fits_guard = |index: usize| {
        let text = lines[index].line();
        if text.split_whitespace().count() > guard {
            return false;
        }
        if text.len() <= guard {
            return true;
        }
        let count = count_tokens(&text);
        tokens[index] = Some(count);
        count <= guard
    };
    let mut placed = vec![false; candidates.len()];
    let mut order = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        if placed[index] {
            continue;
        }
        placed[index] = true;
        order.push(index);
        if candidate.kind != CandidateKind::Fact {
            continue;
        }
        let Some(session) = candidate.session_id.as_deref() else {
            continue;
        };
        for message in &candidate.source_message_ids {
            let Some(&cited) = episode_at.get(&(session, message.as_str())) else {
                continue;
            };
            if !placed[cited] && fits_guard(cited) {
                placed[cited] = true;
                order.push(cited);
            }
        }
    }
    let ordered: Vec<D76Line> = order.iter().map(|&index| lines[index].clone()).collect();
    let known: Vec<Option<usize>> = order.iter().map(|&index| tokens[index]).collect();
    let sources: Vec<Option<Vec<(&str, &str)>>> = order
        .iter()
        .map(|&index| {
            let c = &candidates[index];
            match (c.kind, c.session_id.as_deref()) {
                (CandidateKind::Fact, Some(session)) if !c.source_message_ids.is_empty() => Some(
                    c.source_message_ids
                        .iter()
                        .map(|message| (session, message.as_str()))
                        .collect(),
                ),
                _ => None,
            }
        })
        .collect();
    let mut kept_turns: HashSet<(&str, &str)> = HashSet::new();
    render_lines(
        &ordered,
        token_budget,
        D76Packing::Evidence,
        &known,
        |event| match event {
            Gate::Admit(position) => !sources[position]
                .as_ref()
                .is_some_and(|turns| turns.iter().all(|key| kept_turns.contains(key))),
            Gate::Kept(position) => {
                if let Some(key) = turn(&candidates[order[position]]) {
                    kept_turns.insert(key);
                }
                true
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn display_matches_the_harness_session_form() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap();
        assert_eq!(
            display_timestamp(&at("2024-03-12T16:15:00Z")),
            "4:15 pm on 12 March, 2024"
        );
        assert_eq!(
            display_timestamp(&at("2031-03-03T09:05:00Z")),
            "9:05 am on 3 March, 2031"
        );
        assert_eq!(
            display_timestamp(&at("2031-03-03T00:00:00Z")),
            "12:00 am on 3 March, 2031"
        );
        assert_eq!(
            display_timestamp(&at("2031-12-31T12:30:00Z")),
            "12:30 pm on 31 December, 2031"
        );
        assert_eq!(
            display_timestamp(&Utc.with_ymd_and_hms(2031, 6, 9, 19, 55, 0).unwrap()),
            "7:55 pm on 9 June, 2031"
        );
    }

    #[test]
    fn token_counts_match_known_cl100k_values() {
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("hello world"), 2);
    }

    fn line(text: &str) -> D76Line {
        D76Line {
            kind: CandidateKind::Fact,
            id: Uuid::nil(),
            vector_score: 0.5,
            timestamp: "9:15 am on 3 March, 2031".to_string(),
            text: text.to_string(),
        }
    }

    /// The incremental count equals counting the joined text, including line ends that merge with
    /// the following newline (punctuation, spaces, unicode).
    #[test]
    fn incremental_count_equals_joined_count() {
        let texts = [
            "Avery keeps a pottery wheel.",
            "Ends with a question?",
            "Trailing spaces   ",
            "Unicode — café crème 抹茶 😀",
            "Line with 'quotes' and \"doubles\"",
            "Numbers 12345 and 3.14159!!!",
            "  leading spaces inside text",
        ];
        let lines: Vec<D76Line> = texts.iter().map(|t| line(t)).collect();
        let rendered = render(&lines, 10_000);
        assert_eq!(rendered.items.len(), lines.len());
        assert_eq!(rendered.token_count, count_tokens(&rendered.text));
        for k in 1..=lines.len() {
            let partial = render(&lines[..k], 10_000);
            assert_eq!(
                partial.token_count,
                count_tokens(&partial.text),
                "prefix {k}"
            );
        }
    }

    #[test]
    fn prefix_stops_at_the_first_line_that_does_not_fit() {
        let lines = vec![line("short"), line(&"long ".repeat(200)), line("tiny")];
        let rendered = render(&lines, 40);
        assert_eq!(
            rendered.items.len(),
            1,
            "never skips ahead to the tiny third line"
        );
        assert!(rendered.budget_bound);
        let empty = render(&[], 1_500);
        assert_eq!(
            (empty.items.len(), empty.token_count, empty.budget_bound),
            (0, 0, false)
        );
        let none_fit = render(&[line(&"long ".repeat(200))], 5);
        assert_eq!(
            (
                none_fit.items.len(),
                none_fit.text.as_str(),
                none_fit.budget_bound
            ),
            (0, "", true)
        );
    }
}

#[cfg(test)]
mod packing_tests {
    use super::*;

    fn line(kind: CandidateKind, words: usize, n: u128) -> D76Line {
        D76Line {
            kind,
            id: Uuid::from_u128(n),
            vector_score: 1.0 - n as f32 / 1_000.0,
            timestamp: "9:15 am on 3 March, 2031".to_string(),
            text: format!("item{n} {}", "alpha ".repeat(words))
                .trim()
                .to_string(),
        }
    }

    /// Ranked lines of mixed lengths: short facts and short or very long episodes.
    fn ranked() -> Vec<D76Line> {
        let words = [12, 300, 9, 15, 800, 7, 40, 11, 600, 20, 5, 3];
        words
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                let kind = if w > 30 {
                    CandidateKind::Episode
                } else {
                    CandidateKind::Fact
                };
                line(kind, w, i as u128)
            })
            .collect()
    }

    #[test]
    fn prefix_packing_is_exactly_the_reference_render() {
        let lines = ranked();
        for budget in [1, 5, 40, 120, 400, 1_500, 10_000] {
            assert_eq!(
                render_packed(&lines, budget, D76Packing::Prefix),
                (render(&lines, budget), 0),
                "budget {budget}"
            );
        }
        assert_eq!(D76Packing::default(), D76Packing::Prefix);
    }

    #[test]
    fn fill_skips_a_line_that_does_not_fit_and_keeps_later_ones_in_rank_order() {
        let lines = ranked();
        let prefix = render(&lines, 120);
        let (fill, skipped) = render_packed(&lines, 120, D76Packing::Fill);
        // The prefix stops at the 300-word episode; fill keeps every later short line that fits.
        assert_eq!(prefix.items.len(), 1);
        assert!(fill.items.len() > prefix.items.len());
        assert!(skipped >= 1);
        assert!(fill.budget_bound && prefix.budget_bound);
        let fill_ids: Vec<Uuid> = fill.items.iter().map(|item| item.id).collect();
        let prefix_ids: Vec<Uuid> = prefix.items.iter().map(|item| item.id).collect();
        assert_eq!(&fill_ids[..prefix_ids.len()], prefix_ids.as_slice());
        let mut sorted = fill_ids.clone();
        sorted.sort();
        assert_eq!(fill_ids, sorted, "rank order (ids follow rank here)");
    }

    #[test]
    fn fill_never_exceeds_the_budget_and_counts_the_joined_text_exactly() {
        let lines = ranked();
        for budget in [1, 5, 40, 120, 400, 1_500, 10_000] {
            let (fill, skipped) = render_packed(&lines, budget, D76Packing::Fill);
            assert!(fill.token_count <= budget, "budget {budget}");
            assert_eq!(
                fill.token_count,
                count_tokens(&fill.text),
                "budget {budget}"
            );
            assert_eq!(fill.items.len() + skipped, lines.len(), "budget {budget}");
            assert_eq!(fill.candidates_considered, lines.len());
            assert_eq!(
                render_packed(&lines, budget, D76Packing::Fill),
                (fill, skipped)
            );
        }
        let (unbound, skipped) = render_packed(&lines, 10_000, D76Packing::Fill);
        assert_eq!(unbound.items.len(), lines.len());
        assert_eq!((unbound.budget_bound, skipped), (false, 0));
        assert_eq!(unbound, render(&lines, 10_000));
    }
}

#[cfg(test)]
mod fill_shortcut_tests {
    use super::*;

    /// Fill without the runs shortcut: every line is counted against the joined kept text.
    fn naive_fill(lines: &[D76Line], budget: usize) -> Vec<Uuid> {
        let mut joined = String::new();
        let mut kept = Vec::new();
        for line in lines {
            let text = line.line();
            let candidate = if joined.is_empty() {
                text
            } else {
                format!("{joined}\n{text}")
            };
            if count_tokens(&candidate) <= budget {
                joined = candidate;
                kept.push(line.id);
            }
        }
        kept
    }

    #[test]
    fn the_runs_shortcut_never_changes_what_fill_keeps() {
        // Punctuation, digits, contractions, unicode and repeated whitespace, short and long.
        let texts = [
            "x",
            "user: I'd   like 3,000,000 ü-ä—ö items!!",
            "assistant: ```rust\nfn main() { println!(\"hi\"); }\n```",
            "12 34 56 78 90",
            "...   ???   !!!",
            "naïve café — résumé (über)",
        ];
        let mut lines = Vec::new();
        for n in 0..60u128 {
            let base = texts[(n % texts.len() as u128) as usize];
            let repeat = 1 + (n * 37 % 23) as usize * (if n % 5 == 0 { 40 } else { 1 });
            lines.push(D76Line {
                kind: CandidateKind::Episode,
                id: Uuid::from_u128(n),
                vector_score: 0.5,
                timestamp: if n % 7 == 0 {
                    MISSING_TIMESTAMP.to_string()
                } else {
                    "9:15 am on 3 March, 2031".to_string()
                },
                text: vec![base; repeat].join(" "),
            });
        }
        for budget in [1, 3, 10, 50, 200, 800, 1_500, 4_000] {
            let (fill, _) = render_packed(&lines, budget, D76Packing::Fill);
            let ids: Vec<Uuid> = fill.items.iter().map(|item| item.id).collect();
            assert_eq!(ids, naive_fill(&lines, budget), "budget {budget}");
            assert_eq!(
                fill.token_count,
                count_tokens(&fill.text),
                "budget {budget}"
            );
        }
    }
}

#[cfg(test)]
mod fill_performance_parity_tests {
    use super::*;

    /// The frozen `render_packed` of `1771a924`, verbatim: the reference every output must equal.
    pub(crate) fn render_packed_reference(
        lines: &[D76Line],
        token_budget: usize,
        packing: D76Packing,
    ) -> (D76Context, usize) {
        let mut lines_skipped = 0usize;
        let mut items: Vec<D76ContextItem> = Vec::new();
        let mut joined = String::new();
        let mut sum_with_newlines = 0usize;
        let mut token_count = 0usize;
        let mut budget_bound = false;
        let mut runs_kept = 0usize;
        for line in lines {
            let text = line.line();
            if packing == D76Packing::Fill {
                let runs = text.split_whitespace().count();
                if runs_kept + runs > token_budget {
                    budget_bound = true;
                    lines_skipped += 1;
                    continue;
                }
            }
            let candidate_text = if joined.is_empty() {
                text.clone()
            } else {
                format!("{joined}\n{text}")
            };
            let candidate_count = if items.is_empty() || starts_independently(&text) {
                sum_with_newlines + count_tokens(&text)
            } else {
                count_tokens(&candidate_text)
            };
            if candidate_count > token_budget {
                budget_bound = true;
                match packing {
                    D76Packing::Prefix => break,
                    // `Evidence` did not exist in the frozen loop; it is never passed here.
                    D76Packing::Fill | D76Packing::Evidence => {
                        lines_skipped += 1;
                        continue;
                    }
                }
            }
            let tokens = count_tokens(&text);
            sum_with_newlines = if starts_independently(&text) || items.is_empty() {
                sum_with_newlines + count_tokens(&format!("{text}\n"))
            } else {
                count_tokens(&format!("{candidate_text}\n"))
            };
            token_count = candidate_count;
            runs_kept += text.split_whitespace().count();
            joined = candidate_text;
            items.push(D76ContextItem {
                kind: line.kind,
                id: line.id,
                vector_score: line.vector_score,
                timestamp: line.timestamp.clone(),
                text: line.text.clone(),
                line: text,
                tokens,
            });
        }
        let context = D76Context {
            text: joined,
            token_count,
            items,
            budget_bound,
            candidates_considered: lines.len(),
        };
        (context, lines_skipped)
    }

    const FRAGMENTS: [&str; 20] = [
        "x",
        "Avery keeps a pottery wheel",
        "user: I'd   like 3,000,000 ü-ä—ö items!!",
        "assistant: ```rust\nfn main() { println!(\"hi\"); }\n```",
        "12 34 56 78 90",
        "...   ???   !!!",
        "naïve café — résumé (über)",
        "Ends with a question?",
        "Trailing spaces   ",
        "Unicode 抹茶 😀",
        "it's they're we'll",
        "tab\tseparated\tvalues",
        "  leading spaces inside text",
        "line\nbreaks\r\ninside",
        "2031",
        "end.",
        "😀 emoji first",
        "über first",
        "\u{301}combining mark first",
        "抹茶ends without space抹茶",
    ];

    /// Ranked lines of every shape: short and very long, alphanumeric and other starts (the joined-count
    /// fallback, including non-ASCII starts), punctuation / whitespace / unicode ends, missing timestamps.
    fn corpus() -> Vec<D76Line> {
        let mut lines = Vec::new();
        for n in 0..90u128 {
            let base = FRAGMENTS[(n % FRAGMENTS.len() as u128) as usize];
            let repeat = 1 + (n * 37 % 23) as usize * (if n % 5 == 0 { 40 } else { 1 });
            let timestamp = match n % 9 {
                0 => MISSING_TIMESTAMP.to_string(),
                3 => "  raw, not a time".to_string(),
                6 => "(bracketed)".to_string(),
                4 => "über-early, 3 March 2031".to_string(),
                7 => "抹茶 o'clock".to_string(),
                _ => "9:15 am on 3 March, 2031".to_string(),
            };
            lines.push(D76Line {
                kind: if n % 2 == 0 {
                    CandidateKind::Episode
                } else {
                    CandidateKind::Fact
                },
                id: Uuid::from_u128(n),
                vector_score: 1.0 - n as f32 / 1_000.0,
                timestamp,
                text: vec![base; repeat].join(if n % 3 == 0 { " " } else { "\n" }),
            });
        }
        lines
    }

    #[test]
    fn tokens_with_newline_equals_counting_the_newline() {
        let mut texts: Vec<String> = FRAGMENTS.iter().map(|s| s.to_string()).collect();
        for a in FRAGMENTS {
            for b in FRAGMENTS {
                texts.push(format!("{a} {b}"));
                texts.push(format!("{a}{b}"));
            }
        }
        texts.extend(
            [
                "", "a", "Z9", "abc123", "x 1", "über", "end!", "end ", "end\n", "'s",
            ]
            .map(String::from),
        );
        for text in &texts {
            assert_eq!(
                tokens_with_newline(text, count_tokens(text)),
                count_tokens(&format!("{text}\n")),
                "{text:?}"
            );
        }
    }

    #[test]
    fn render_packed_equals_the_frozen_loop_for_both_packings() {
        let lines = corpus();
        let mut budgets: Vec<usize> =
            vec![1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987];
        budgets.extend([
            1_000, 1_499, 1_500, 1_501, 2_000, 2_999, 3_000, 3_001, 4_000, 10_000,
        ]);
        for packing in [D76Packing::Prefix, D76Packing::Fill] {
            for &budget in &budgets {
                for start in [0usize, 1, 7, 30] {
                    let slice = &lines[start..];
                    assert_eq!(
                        render_packed(slice, budget, packing),
                        render_packed_reference(slice, budget, packing),
                        "{packing:?} budget {budget} start {start}"
                    );
                }
            }
        }
        assert_eq!(
            render_packed(&[], 1_500, D76Packing::Fill),
            render_packed_reference(&[], 1_500, D76Packing::Fill)
        );
    }

    /// Token-boundary cases: every budget that a context fits exactly, one token under and one over.
    #[test]
    fn render_packed_equals_the_frozen_loop_at_exact_fit_budgets() {
        let lines = corpus();
        for packing in [D76Packing::Prefix, D76Packing::Fill] {
            for start in [0usize, 7] {
                let slice = &lines[start..];
                let mut fits: Vec<usize> = [8, 55, 377, 1_000, 1_500, 2_000, 3_000, 4_000]
                    .iter()
                    .map(|&budget| {
                        render_packed_reference(slice, budget, packing)
                            .0
                            .token_count
                    })
                    .filter(|&count| count > 0)
                    .collect();
                fits.sort_unstable();
                fits.dedup();
                for count in fits {
                    for budget in [count - 1, count, count + 1] {
                        let (got, skipped) = render_packed(slice, budget, packing);
                        assert!(got.token_count <= budget, "{packing:?} budget {budget}");
                        assert_eq!(
                            (got, skipped),
                            render_packed_reference(slice, budget, packing),
                            "{packing:?} budget {budget} start {start}"
                        );
                    }
                }
            }
        }
    }

    /// Deterministic selection: the same ranked lines always give the same context.
    #[test]
    fn render_packed_is_deterministic() {
        let lines = corpus();
        for packing in [D76Packing::Prefix, D76Packing::Fill] {
            for budget in [1_500, 3_000] {
                let first = render_packed(&lines, budget, packing);
                for _ in 0..3 {
                    assert_eq!(render_packed(&lines.clone(), budget, packing), first);
                }
            }
        }
    }
}

#[cfg(test)]
mod evidence_packing_tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn fact(n: u128, session: &str, sources: &[&str], content: &str) -> Candidate {
        Candidate {
            kind: CandidateKind::Fact,
            id: Uuid::from_u128(n),
            content: content.to_string(),
            vector_score: 0.9,
            rank: 1,
            source_message_ids: sources.iter().map(|s| s.to_string()).collect(),
            message_id: None,
            session_id: Some(session.to_string()),
            speaker: None,
            metadata: serde_json::json!({"timestamp": "2031-03-03T09:15:00Z"}),
            event_time: None,
            observed_at: None,
            query_index: 0,
            rerank_score: None,
        }
    }

    fn episode(n: u128, session: &str, message: &str, content: &str) -> Candidate {
        Candidate {
            kind: CandidateKind::Episode,
            id: Uuid::from_u128(n),
            content: content.to_string(),
            vector_score: 0.5,
            rank: 1,
            source_message_ids: Vec::new(),
            message_id: Some(message.to_string()),
            session_id: Some(session.to_string()),
            speaker: Some("user".to_string()),
            metadata: Value::Null,
            event_time: None,
            observed_at: Some(Utc.with_ymd_and_hms(2031, 3, 3, 9, 15, 0).unwrap()),
            query_index: 0,
            rerank_score: None,
        }
    }

    fn ids(context: &D76Context) -> Vec<u128> {
        context.items.iter().map(|item| item.id.as_u128()).collect()
    }

    #[test]
    fn a_cited_short_episode_follows_its_fact_and_a_redundant_fact_is_skipped() {
        let candidates = vec![
            fact(1, "s1", &["b4"], "Avery keeps a pottery wheel"),
            episode(2, "s1", "b9", "unrelated chatter about the weather"),
            fact(3, "s1", &["b4"], "Avery bought the wheel in March"),
            episode(4, "s1", "b4", "I keep a pottery wheel I bought in March"),
        ];
        let (context, skipped) = render_evidence(&candidates, 1_500);
        assert_eq!(ids(&context), vec![1, 4, 2]);
        assert_eq!(skipped, 1);
        assert!(!context.budget_bound);
        assert!(context.token_count <= 1_500);
        assert_eq!(context.candidates_considered, 4);
    }

    #[test]
    fn a_cited_episode_over_a_fifth_of_the_budget_keeps_its_own_rank() {
        let long = vec!["word"; 400].join(" ");
        let candidates = vec![
            fact(1, "s1", &["b4"], "Avery keeps a pottery wheel"),
            episode(2, "s1", "b9", "unrelated chatter"),
            episode(4, "s1", "b4", &long),
        ];
        let (context, _) = render_evidence(&candidates, 1_500);
        assert_eq!(ids(&context), vec![1, 2, 4]);
        let (small, _) = render_evidence(&candidates, 2_500);
        assert_eq!(ids(&small), vec![1, 4, 2]);
    }

    #[test]
    fn citations_match_only_within_the_fact_s_session() {
        let candidates = vec![
            fact(1, "s1", &["b4"], "Avery keeps a pottery wheel"),
            episode(2, "s1", "b9", "unrelated chatter"),
            episode(4, "s2", "b4", "same message id, another session"),
        ];
        let (context, skipped) = render_evidence(&candidates, 1_500);
        assert_eq!(ids(&context), vec![1, 2, 4]);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn without_citations_evidence_equals_fill() {
        let mut candidates = Vec::new();
        for n in 0..40u128 {
            let text = vec!["Avery keeps a pottery wheel"; 1 + (n as usize * 7) % 30].join(" ");
            candidates.push(if n % 2 == 0 {
                fact(n, "s1", &[], &text)
            } else {
                episode(n, "s1", &format!("b{n}"), &text)
            });
        }
        let lines: Vec<D76Line> = candidates.iter().map(D76Line::from_candidate).collect();
        for budget in [50, 300, 1_500, 3_000] {
            assert_eq!(
                render_evidence(&candidates, budget),
                render_packed(&lines, budget, D76Packing::Fill),
                "budget {budget}"
            );
        }
    }

    /// Integration verification: on mixed multi-session candidates the output is deterministic, in budget
    /// with an exact count, made of exact candidate lines without duplicates; every episode kept out of its
    /// rank order is cited by an earlier fact of the same session and within the budget / 5 guard; and no
    /// kept fact is redundant (all its source turns already kept before it).
    #[test]
    fn evidence_is_deterministic_in_budget_provenanced_and_without_redundant_facts() {
        let mut candidates = Vec::new();
        for n in 0..80u128 {
            let session = format!("s{}", (n / 10) % 3);
            let words = 1 + (n as usize * 37) % 40;
            let text = vec!["Avery's garden: tomatoes, basil & mulch."; words].join(" ");
            if n % 3 == 0 {
                // Even facts cite turns ranked after them; odd ones a turn ranked before them
                // (redundant once that turn is kept).
                let cites = if n % 2 == 0 {
                    vec![format!("b{}", n + 1), format!("b{}", n + 4)]
                } else {
                    vec![format!("b{}", n - 2)]
                };
                let cites: Vec<&str> = cites.iter().map(String::as_str).collect();
                candidates.push(fact(n, &session, &cites, &text));
            } else {
                candidates.push(episode(n, &session, &format!("b{n}"), &text));
            }
        }
        let lines: HashMap<Uuid, String> = candidates
            .iter()
            .map(|c| (c.id, D76Line::from_candidate(c).line()))
            .collect();
        let rank: HashMap<Uuid, usize> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();
        let by_id: HashMap<Uuid, &Candidate> = candidates.iter().map(|c| (c.id, c)).collect();
        let (mut promoted, mut facts_skipped) = (0usize, 0usize);
        for budget in [50, 300, 1_500, 3_000] {
            let fill: Vec<D76Line> = candidates.iter().map(D76Line::from_candidate).collect();
            let (fill, _) = render_packed(&fill, budget, D76Packing::Fill);
            let facts = |c: &D76Context| {
                c.items
                    .iter()
                    .filter(|i| i.kind == CandidateKind::Fact)
                    .count()
            };
            let (context, skipped) = render_evidence(&candidates, budget);
            assert_eq!(
                render_evidence(&candidates, budget),
                (context.clone(), skipped)
            );
            assert!(context.token_count <= budget);
            assert_eq!(context.token_count, count_tokens(&context.text));
            assert_eq!(skipped + context.items.len(), candidates.len());
            let mut seen = std::collections::HashSet::new();
            let mut kept_turns = std::collections::HashSet::new();
            for (position, item) in context.items.iter().enumerate() {
                assert!(seen.insert(item.id), "duplicate");
                assert_eq!(&item.line, &lines[&item.id], "exact candidate line");
                let c = by_id[&item.id];
                if c.kind == CandidateKind::Episode {
                    let out_of_rank = context.items[position + 1..]
                        .iter()
                        .any(|later| rank[&later.id] < rank[&item.id]);
                    if out_of_rank {
                        promoted += 1;
                        assert!(item.tokens <= budget / 5, "guard");
                        assert!(
                            context.items[..position].iter().any(|earlier| {
                                let f = by_id[&earlier.id];
                                f.kind == CandidateKind::Fact
                                    && f.session_id == c.session_id
                                    && c.message_id
                                        .as_ref()
                                        .is_some_and(|m| f.source_message_ids.contains(m))
                            }),
                            "a promoted episode follows a fact citing it"
                        );
                    }
                    kept_turns.insert((c.session_id.clone(), c.message_id.clone()));
                } else if !c.source_message_ids.is_empty() {
                    assert!(
                        !c.source_message_ids
                            .iter()
                            .all(|m| kept_turns.contains(&(c.session_id.clone(), Some(m.clone())))),
                        "redundant fact kept"
                    );
                }
            }
            facts_skipped += facts(&fill).saturating_sub(facts(&context));
        }
        assert!(
            promoted > 0 && facts_skipped > 0,
            "{promoted} {facts_skipped}"
        );
    }

    #[test]
    fn evidence_packing_serialises_as_evidence() {
        assert_eq!(
            serde_json::to_string(&D76Packing::Evidence).unwrap(),
            "\"evidence\""
        );
        assert_eq!(D76Packing::default(), D76Packing::Prefix);
    }
}
