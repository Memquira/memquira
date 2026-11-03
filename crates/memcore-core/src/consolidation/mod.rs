//! Consolidation v2 (`MEMCORE_CONSOLIDATION_MODE=llm_v2`).
//!
//! For each new candidate fact:
//! 1. **Candidates.** The user's related active facts: vector neighbours of the candidate (same
//!    subject first, then others, each by similarity), then full-text hits not already included.
//!    At most `ConsolidationConfig::candidates`. Invalidated facts are never candidates.
//! 2. **Fast paths, no model call.**
//!    - An active fact with the same normalised text, the same subject and the same event time
//!      means NOOP.
//!    - No candidate at or above the similarity floor means ADD.
//! 3. **Model decision** (`LlmProvider::decide_consolidation`): ADD, UPDATE, INVALIDATE or NOOP,
//!    validated here.
//!    - The target must be one of the offered candidates.
//!    - A fact about a different subject is never updated or invalidated by the candidate.
//!    - A NOOP must name an offered target.
//!    - A decision that fails validation is stored as an ADD, so invalid model output never loses
//!      or changes information. The rejection is audited.
//!
//! The constants below are fixed before any measurement and are never tuned
//! on the consolidation eval set.

use std::cmp::Ordering;
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use memcore_common::MemcoreResult;
use uuid::Uuid;

use crate::dedup::normalize_content;
use crate::ports::{
    ConsolidationDecision, ConsolidationOperation, FactFulltextQuery, FactStore, VectorSearchQuery,
    VectorStore,
};
use crate::search::FulltextQuery;
use crate::{CandidateFact, Fact, TenantContext};

/// Related-fact candidates a decision sees (`MEMCORE_CONSOLIDATION_CANDIDATES`); the original
/// `RELATED_FACTS_SEARCH_LIMIT`.
pub const DEFAULT_CONSOLIDATION_CANDIDATES: usize = 10;

/// Below this vector similarity no stored fact is considered related enough to ask the model
/// (`MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR`). Deliberately low: it only skips the model for
/// clearly unrelated facts, and the model, not the floor, decides everything else.
pub const DEFAULT_CONSOLIDATION_SIMILARITY_FLOOR: f32 = 0.5;

/// Consolidation v2 settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsolidationConfig {
    pub candidates: usize,
    pub similarity_floor: f32,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            candidates: DEFAULT_CONSOLIDATION_CANDIDATES,
            similarity_floor: DEFAULT_CONSOLIDATION_SIMILARITY_FLOOR,
        }
    }
}

/// A stored fact offered to a decision.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedFact {
    pub fact: Fact,
    /// Vector similarity to the candidate; `None` for a full-text-only hit.
    pub similarity: Option<f32>,
    pub same_subject: bool,
}

/// Subject names compared case- and whitespace-insensitively.
pub fn normalized_subject(subject: Option<&str>) -> Option<String> {
    subject
        .map(|value| {
            value
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .filter(|value| !value.is_empty())
}

fn same_subject(candidate: &CandidateFact, fact: &Fact) -> bool {
    match (
        normalized_subject(candidate.structure.subject.as_deref()),
        normalized_subject(fact.structure.subject.as_deref()),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Gathers the user's related active facts for `candidate`, most relevant first.
pub async fn gather_related_facts(
    fact_store: &dyn FactStore,
    vector_store: &dyn VectorStore,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    embedding: &[f32],
    config: &ConsolidationConfig,
) -> MemcoreResult<Vec<RelatedFact>> {
    let hits = vector_store
        .search_vectors(VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: embedding.to_vec(),
            limit: config.candidates.saturating_mul(2).max(1),
            memory_types: None,
            metadata_filter: None,
        })
        .await?;

    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut semantic: Vec<RelatedFact> = Vec::new();
    for hit in hits {
        if !seen.insert(hit.fact_id) {
            continue;
        }
        let Some(fact) = fact_store.get_fact(tenant, hit.fact_id).await? else {
            continue;
        };
        if !fact.is_active() {
            continue;
        }
        semantic.push(RelatedFact {
            same_subject: same_subject(candidate, &fact),
            similarity: Some(hit.score),
            fact,
        });
    }
    // Same subject first, then similarity; exact ties by stable fields.
    semantic.sort_by(|left, right| {
        right
            .same_subject
            .cmp(&left.same_subject)
            .then_with(|| {
                right
                    .similarity
                    .partial_cmp(&left.similarity)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| left.fact.content.cmp(&right.fact.content))
            .then_with(|| left.fact.id.cmp(&right.fact.id))
    });

    let subject_names: Vec<String> = candidate.structure.subject.iter().cloned().collect();
    let fulltext = fact_store
        .fulltext_search_facts(FactFulltextQuery {
            tenant: tenant.clone(),
            memory_types: None,
            query: FulltextQuery::parse_scoped(&candidate.content, &subject_names),
            limit: config.candidates,
        })
        .await?;

    let mut related = semantic;
    for scored in fulltext {
        if !scored.fact.is_active() || !seen.insert(scored.fact.id) {
            continue;
        }
        related.push(RelatedFact {
            same_subject: same_subject(candidate, &scored.fact),
            similarity: None,
            fact: scored.fact,
        });
    }
    related.truncate(config.candidates);
    Ok(related)
}

/// What happens to a candidate before any model call.
#[derive(Debug, Clone, PartialEq)]
pub enum FastPath {
    /// An active fact already says exactly this (same text, subject and event time).
    NoOp { target_fact_id: Uuid },
    /// Nothing related at or above the similarity floor.
    Add,
    /// Ask the model.
    Decide,
}

fn same_event_time(candidate: &CandidateFact, fact: &Fact) -> bool {
    match (&candidate.structure.event_time, &fact.structure.event_time) {
        (None, None) => true,
        (Some(left), Some(right)) => left.start == right.start && left.end == right.end,
        _ => false,
    }
}

fn same_subject_or_both_unknown(candidate: &CandidateFact, fact: &Fact) -> bool {
    normalized_subject(candidate.structure.subject.as_deref())
        == normalized_subject(fact.structure.subject.as_deref())
}

pub fn fast_path(
    candidate: &CandidateFact,
    related: &[RelatedFact],
    config: &ConsolidationConfig,
) -> FastPath {
    let text = normalize_content(&candidate.content);
    if let Some(duplicate) = related.iter().find(|related| {
        normalize_content(&related.fact.content) == text
            && same_subject_or_both_unknown(candidate, &related.fact)
            && same_event_time(candidate, &related.fact)
    }) {
        return FastPath::NoOp {
            target_fact_id: duplicate.fact.id,
        };
    }
    let above_floor = related.iter().any(|related| {
        related
            .similarity
            .is_some_and(|similarity| similarity >= config.similarity_floor)
    });
    if above_floor {
        FastPath::Decide
    } else {
        FastPath::Add
    }
}

/// Checks a model decision against the offered candidates. `Err` holds the rejection reason; the
/// caller then stores the candidate as a new fact.
pub fn validate_decision(
    decision: &ConsolidationDecision,
    candidate: &CandidateFact,
    related: &[RelatedFact],
) -> Result<ConsolidationDecision, String> {
    let target = decision
        .target_fact_id
        .and_then(|id| related.iter().find(|related| related.fact.id == id));
    match decision.operation {
        ConsolidationOperation::Add => Ok(ConsolidationDecision {
            target_fact_id: None,
            merged_content: None,
            invalid_at: None,
            invalid_at_unparsed: None,
            ..decision.clone()
        }),
        ConsolidationOperation::NoOp => match target {
            Some(_) => Ok(decision.clone()),
            None => Err("NOOP without a target among the offered candidates".to_string()),
        },
        ConsolidationOperation::Update | ConsolidationOperation::Invalidate => {
            let Some(target) = target else {
                return Err(format!(
                    "{:?} target is not among the offered candidates",
                    decision.operation
                ));
            };
            let candidate_subject = normalized_subject(candidate.structure.subject.as_deref());
            let target_subject = normalized_subject(target.fact.structure.subject.as_deref());
            if let (Some(left), Some(right)) = (&candidate_subject, &target_subject)
                && left != right
            {
                return Err(format!(
                    "cross-subject {:?} refused: candidate is about {left:?}, target about {right:?}",
                    decision.operation
                ));
            }
            let mut decision = decision.clone();
            if decision.operation == ConsolidationOperation::Update {
                let merged = decision
                    .merged_content
                    .as_deref()
                    .map(str::trim)
                    .filter(|content| !content.is_empty())
                    .unwrap_or(candidate.content.trim())
                    .to_string();
                decision.merged_content = Some(merged);
                decision.invalid_at = None;
                decision.invalid_at_unparsed = None;
            } else {
                decision.merged_content = None;
            }
            Ok(decision)
        }
    }
}

/// Where an invalidated fact's `invalid_at` came from. Only deterministic temporal
/// evidence from the source supports a change time; there is no model-only, observation or receipt
/// source. Without such evidence `invalid_at` stays unset and the fact is inactive through
/// `superseded_at` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidAtSource {
    /// The deterministically resolved change day (a day-precision source time expression).
    SourceEventTime,
    /// The model's date, accepted because it lies inside a coarser deterministically resolved
    /// window (a week, month, year or approximate span).
    ModelWithinSourceWindow,
    /// No source-supported change time: `invalid_at` is left unset.
    Unknown,
}

impl InvalidAtSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceEventTime => "source_event_time",
            Self::ModelWithinSourceWindow => "model_within_source_window",
            Self::Unknown => "unknown",
        }
    }
}

/// A model `invalid_at` that memcore did not accept, kept for the audit event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedInvalidAt {
    /// The model's value as given (RFC 3339 when it was readable).
    pub value: String,
    pub reason: String,
}

/// The outcome of [`resolve_invalid_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAtResolution {
    pub invalid_at: Option<DateTime<Utc>>,
    pub source: InvalidAtSource,
    pub rejected: Option<RejectedInvalidAt>,
}

/// Why a time cannot be the real-world end of the superseded fact, if it cannot.
///
/// The change cannot have happened after the message that reports it: the candidate's source time
/// (`observed_at`) or, when the client sent none, when memcore received it. That upper bound is a
/// check only; it never becomes `invalid_at`. The lower bound (year 1000) also applies.
fn change_time_problem(time: DateTime<Utc>, source_time: DateTime<Utc>) -> Option<String> {
    use chrono::Datelike;
    if time.year() < MIN_CHANGE_YEAR {
        return Some(format!("before year {MIN_CHANGE_YEAR}"));
    }
    (time > source_time).then(|| {
        format!(
            "later than the source message time {}",
            source_time.to_rfc3339()
        )
    })
}

/// The lower bound on event times (`temporal::resolver`), applied to change times too.
const MIN_CHANGE_YEAR: i32 = 1000;

/// The deterministic temporal evidence for the change the candidate reports: the
/// resolver applied to the candidate's own time expression, with no model input, against the
/// source message time (absolute expressions only when the client sent none). A candidate whose
/// event time did not survive extraction (ungrounded, non-datable, unanchored) has none, and neither
/// does an event time only the model supplied.
fn source_change_window(candidate: &CandidateFact) -> Option<crate::TemporalValue> {
    candidate.structure.event_time.as_ref()?;
    let text = candidate.structure.event_time_text.as_deref()?;
    crate::temporal::settle_event_time(None, Some(text), candidate.structure.observed_at).0
}

/// The time a superseded fact stopped being true, only when deterministic source temporal evidence
/// supports one:
///
/// - a day-precision resolved change time is used as is, and a model date on another day is
///   rejected as conflicting;
/// - with a coarser resolved window, the model's date is used only when it lies inside the window;
///   a date outside it is rejected, and without a model date `invalid_at` stays unset;
/// - with no evidence, any model date is rejected and `invalid_at` stays unset.
///
/// Evidence or dates later than the source message (`observed_at`, else `received_at`, a bound
/// only) or before year 1000 never qualify. Never the candidate's `observed_at`, `recorded_at` or
/// the receipt time.
pub fn resolve_invalid_at(
    decision: &ConsolidationDecision,
    candidate: &CandidateFact,
    received_at: DateTime<Utc>,
) -> InvalidAtResolution {
    let source_time = candidate.structure.observed_at.unwrap_or(received_at);
    let mut rejected = decision
        .invalid_at_unparsed
        .as_ref()
        .map(|raw| RejectedInvalidAt {
            value: raw.clone(),
            reason: "not a readable date".to_string(),
        });
    let mut reject = |time: DateTime<Utc>, reason: String| {
        rejected = Some(RejectedInvalidAt {
            value: time.to_rfc3339(),
            reason,
        });
    };
    let window = source_change_window(candidate)
        .filter(|window| change_time_problem(window.start, source_time).is_none());
    let within = |window: &crate::TemporalValue, time: DateTime<Utc>| {
        window.start <= time && time <= window.end
    };
    let outcome = match (window, decision.invalid_at) {
        (Some(window), model) if window.granularity == crate::TemporalGranularity::Day => {
            if let Some(time) = model.filter(|time| !within(&window, *time)) {
                reject(
                    time,
                    format!(
                        "conflicts with the source-resolved change day {}",
                        window.start.date_naive()
                    ),
                );
            }
            (Some(window.start), InvalidAtSource::SourceEventTime)
        }
        (Some(window), Some(time)) => match change_time_problem(time, source_time) {
            Some(problem) => {
                reject(time, problem);
                (None, InvalidAtSource::Unknown)
            }
            None if within(&window, time) => (Some(time), InvalidAtSource::ModelWithinSourceWindow),
            None => {
                reject(
                    time,
                    format!(
                        "outside the source-resolved window {} to {}",
                        window.start.to_rfc3339(),
                        window.end.to_rfc3339()
                    ),
                );
                (None, InvalidAtSource::Unknown)
            }
        },
        (Some(_), None) => (None, InvalidAtSource::Unknown),
        (None, Some(time)) => {
            let reason = change_time_problem(time, source_time).unwrap_or_else(|| {
                "no deterministic source temporal evidence supports a change time".to_string()
            });
            reject(time, reason);
            (None, InvalidAtSource::Unknown)
        }
        (None, None) => (None, InvalidAtSource::Unknown),
    };
    InvalidAtResolution {
        invalid_at: outcome.0,
        source: outcome.1,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FactStructure, MemorySource, MemoryType};
    use serde_json::json;

    fn candidate(content: &str, subject: Option<&str>) -> CandidateFact {
        CandidateFact::new(content, MemoryType::Preference, 0.9, 0.9, None, json!({}))
            .unwrap()
            .with_structure(FactStructure {
                subject: subject.map(str::to_string),
                ..FactStructure::default()
            })
    }

    fn fact(content: &str, subject: Option<&str>) -> Fact {
        let now = Utc::now();
        Fact::new(
            Uuid::new_v4(),
            "org",
            "user",
            MemoryType::Preference,
            content,
            None,
            MemorySource::UserMessage,
            0.9,
            0.9,
            None,
            None,
            now,
            now,
            json!({}),
        )
        .unwrap()
        .with_structure(FactStructure {
            subject: subject.map(str::to_string),
            ..FactStructure::default()
        })
    }

    fn related(fact: Fact, similarity: Option<f32>, candidate: &CandidateFact) -> RelatedFact {
        RelatedFact {
            same_subject: same_subject(candidate, &fact),
            similarity,
            fact,
        }
    }

    fn decision(operation: ConsolidationOperation, target: Option<Uuid>) -> ConsolidationDecision {
        ConsolidationDecision {
            operation,
            target_fact_id: target,
            merged_content: None,
            invalid_at: None,
            invalid_at_unparsed: None,
            reason: None,
        }
    }

    #[test]
    fn exact_duplicate_with_same_subject_and_time_is_a_fast_noop() {
        let c = candidate("Orla keeps bees.", Some("Orla"));
        let existing = fact("orla keeps bees", Some("ORLA"));
        let id = existing.id;
        let path = fast_path(
            &c,
            &[related(existing, Some(0.99), &c)],
            &Default::default(),
        );
        assert_eq!(path, FastPath::NoOp { target_fact_id: id });
    }

    #[test]
    fn same_text_about_another_subject_is_not_a_fast_noop() {
        let c = candidate("Keeps bees.", Some("Orla"));
        let existing = fact("Keeps bees.", Some("Piet"));
        let path = fast_path(
            &c,
            &[related(existing, Some(0.99), &c)],
            &Default::default(),
        );
        assert_eq!(path, FastPath::Decide);
    }

    #[test]
    fn nothing_above_the_floor_is_a_fast_add() {
        let c = candidate("Orla keeps bees.", Some("Orla"));
        let related_facts = vec![
            related(fact("Orla drives a van.", Some("Orla")), Some(0.49), &c),
            related(fact("Orla likes jazz.", Some("Orla")), None, &c),
        ];
        assert_eq!(
            fast_path(&c, &related_facts, &Default::default()),
            FastPath::Add
        );
        assert_eq!(fast_path(&c, &[], &Default::default()), FastPath::Add);
    }

    #[test]
    fn a_candidate_at_the_floor_goes_to_the_model() {
        let c = candidate("Orla keeps bees.", Some("Orla"));
        let related_facts = vec![related(
            fact("Orla has hives.", Some("Orla")),
            Some(0.5),
            &c,
        )];
        assert_eq!(
            fast_path(&c, &related_facts, &Default::default()),
            FastPath::Decide
        );
    }

    #[test]
    fn a_target_outside_the_candidates_is_rejected() {
        let c = candidate("Orla moved to Cork.", Some("Orla"));
        let offered = vec![related(
            fact("Orla lives in Galway.", Some("Orla")),
            Some(0.8),
            &c,
        )];
        for op in [
            ConsolidationOperation::Update,
            ConsolidationOperation::Invalidate,
            ConsolidationOperation::NoOp,
        ] {
            assert!(validate_decision(&decision(op, Some(Uuid::new_v4())), &c, &offered).is_err());
            assert!(validate_decision(&decision(op, None), &c, &offered).is_err());
        }
    }

    #[test]
    fn a_cross_subject_update_or_invalidation_is_rejected() {
        let c = candidate("Piet adopted a cat.", Some("Piet"));
        let other = fact("Orla adopted a cat.", Some("Orla"));
        let id = other.id;
        let offered = vec![related(other, Some(0.9), &c)];
        for op in [
            ConsolidationOperation::Update,
            ConsolidationOperation::Invalidate,
        ] {
            let error = validate_decision(&decision(op, Some(id)), &c, &offered).unwrap_err();
            assert!(error.contains("cross-subject"), "{error}");
        }
    }

    #[test]
    fn update_without_merged_content_uses_the_candidate_text() {
        let c = candidate("Orla keeps three hives.", Some("Orla"));
        let target = fact("Orla keeps bees.", Some("Orla"));
        let id = target.id;
        let offered = vec![related(target, Some(0.9), &c)];
        let validated = validate_decision(
            &decision(ConsolidationOperation::Update, Some(id)),
            &c,
            &offered,
        )
        .unwrap();
        assert_eq!(
            validated.merged_content.as_deref(),
            Some("Orla keeps three hives.")
        );
    }

    #[test]
    fn add_drops_any_target() {
        let c = candidate("Orla keeps bees.", Some("Orla"));
        let validated = validate_decision(
            &decision(ConsolidationOperation::Add, Some(Uuid::new_v4())),
            &c,
            &[],
        )
        .unwrap();
        assert_eq!(validated.target_fact_id, None);
    }

    fn resolve(
        decided: Option<DateTime<Utc>>,
        unparsed: Option<&str>,
        c: &CandidateFact,
        received_at: DateTime<Utc>,
    ) -> InvalidAtResolution {
        let mut d = decision(ConsolidationOperation::Invalidate, Some(Uuid::new_v4()));
        d.invalid_at = decided;
        d.invalid_at_unparsed = unparsed.map(str::to_string);
        resolve_invalid_at(&d, c, received_at)
    }

    fn utc(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    /// A candidate stated at `observed` whose time expression was settled the way extraction does.
    fn stated(expression: Option<&str>, observed: Option<DateTime<Utc>>) -> CandidateFact {
        let mut c = candidate("Orla moved to Cork.", Some("Orla"));
        c.structure.observed_at = observed;
        c.structure.event_time_text = expression.map(str::to_string);
        c.structure.event_time = expression
            .and_then(|text| crate::temporal::settle_event_time(None, Some(text), observed).0);
        c
    }

    #[test]
    fn an_exact_resolved_change_day_is_used_and_a_matching_model_date_passes() {
        let now = Utc::now();
        let c = stated(Some("8 May 2025"), Some(utc(2025, 5, 20)));
        for model in [None, Some(utc(2025, 5, 8))] {
            let resolved = resolve(model, None, &c, now);
            assert_eq!(resolved.invalid_at, Some(utc(2025, 5, 8)));
            assert_eq!(resolved.source, InvalidAtSource::SourceEventTime);
            assert_eq!(resolved.rejected, None);
        }
        // A relative expression resolved against the message time is evidence too.
        let c = stated(
            Some("yesterday"),
            Some(utc(2025, 5, 20) + chrono::Duration::hours(9)),
        );
        assert_eq!(
            resolve(None, None, &c, now).invalid_at,
            Some(utc(2025, 5, 19))
        );
    }

    #[test]
    fn a_model_date_conflicting_with_the_resolved_day_is_rejected() {
        let c = stated(Some("8 May 2025"), Some(utc(2025, 5, 20)));
        let resolved = resolve(Some(utc(2025, 5, 1)), None, &c, Utc::now());
        assert_eq!(resolved.invalid_at, Some(utc(2025, 5, 8)));
        assert!(resolved.rejected.unwrap().reason.contains("conflicts"));
    }

    #[test]
    fn a_model_date_inside_a_resolved_window_passes_and_outside_is_rejected() {
        let now = Utc::now();
        let c = stated(Some("April 2025"), Some(utc(2025, 5, 20)));
        let inside = resolve(Some(utc(2025, 4, 15)), None, &c, now);
        assert_eq!(inside.invalid_at, Some(utc(2025, 4, 15)));
        assert_eq!(inside.source, InvalidAtSource::ModelWithinSourceWindow);
        assert_eq!(inside.rejected, None);

        let outside = resolve(Some(utc(2025, 3, 30)), None, &c, now);
        assert_eq!(outside.invalid_at, None);
        assert_eq!(outside.source, InvalidAtSource::Unknown);
        assert!(outside.rejected.unwrap().reason.contains("outside"));

        // A window alone is not a change time: no precision is manufactured.
        let alone = resolve(None, None, &c, now);
        assert_eq!(
            (alone.invalid_at, alone.source),
            (None, InvalidAtSource::Unknown)
        );
    }

    #[test]
    fn a_plausible_model_date_without_source_evidence_is_rejected() {
        let now = Utc::now();
        // "No longer" with no change date: nothing resolves.
        let c = stated(None, Some(utc(2025, 5, 20)));
        let resolved = resolve(Some(utc(2025, 5, 10)), None, &c, now);
        assert_eq!(resolved.invalid_at, None);
        assert_eq!(resolved.source, InvalidAtSource::Unknown);
        assert!(
            resolved
                .rejected
                .unwrap()
                .reason
                .contains("no deterministic")
        );

        // An event time only the model supplied (the expression does not resolve) is not evidence.
        let mut c = stated(Some("around the holidays"), Some(utc(2025, 5, 20)));
        c.structure.event_time = Some(
            crate::TemporalValue::new(
                utc(2024, 12, 24),
                utc(2024, 12, 24),
                crate::TemporalGranularity::Day,
                utc(2025, 5, 20),
            )
            .unwrap(),
        );
        let resolved = resolve(Some(utc(2024, 12, 24)), None, &c, now);
        assert_eq!(resolved.invalid_at, None);
        assert!(resolved.rejected.is_some());
    }

    #[test]
    fn message_and_receipt_times_are_bounds_never_values() {
        let now = Utc::now();
        let observed = utc(2025, 5, 20);
        for c in [stated(None, Some(observed)), stated(None, None)] {
            let resolved = resolve(None, None, &c, now);
            assert_eq!(
                (resolved.invalid_at, resolved.source),
                (None, InvalidAtSource::Unknown)
            );
            // Even a model date equal to the message or receipt time is not promoted.
            let bound = c.structure.observed_at.unwrap_or(now);
            assert_eq!(resolve(Some(bound), None, &c, now).invalid_at, None);
        }
        // A model date after the source message is rejected with that reason.
        let c = stated(Some("April 2025"), Some(observed));
        let after = resolve(Some(utc(2025, 6, 1)), None, &c, now);
        assert_eq!(after.invalid_at, None);
        assert!(
            after
                .rejected
                .unwrap()
                .reason
                .contains("later than the source message")
        );
        // Resolved evidence after the message (a planned change) is not a known end.
        let c = stated(Some("next Saturday"), Some(observed));
        assert_eq!(resolve(None, None, &c, now).invalid_at, None);
    }

    #[test]
    fn unreadable_and_impossible_model_dates_are_rejected_and_recorded() {
        let now = Utc::now();
        let c = stated(None, Some(utc(2025, 5, 20)));
        let resolved = resolve(None, Some("sometime"), &c, now);
        assert_eq!(resolved.invalid_at, None);
        assert_eq!(resolved.rejected.unwrap().value, "sometime");
        let ancient = DateTime::parse_from_rfc3339("0999-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let resolved = resolve(Some(ancient), None, &c, now);
        assert_eq!(resolved.invalid_at, None);
        assert!(resolved.rejected.unwrap().reason.contains("before year"));
    }
}
