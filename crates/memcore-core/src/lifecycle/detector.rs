use memcore_common::MemcoreResult;
use uuid::Uuid;

use crate::consolidation::{DEFAULT_CONSOLIDATION_SIMILARITY_FLOOR, normalized_subject};
use crate::dedup::vector_hit_rejection;
use crate::ports::{FactSearchQuery, FactStore, VectorSearchQuery, VectorStore};
use crate::{CandidateFact, Fact, FactOperation, FactOperationDecision, MemoryType, TenantContext};

use super::types::RELATED_FACTS_SEARCH_LIMIT;

/// Finds existing facts that may conflict with a candidate using simple fact-store search.
pub async fn find_related_facts(
    fact_store: &dyn FactStore,
    tenant: &TenantContext,
    candidate: &CandidateFact,
) -> MemcoreResult<Vec<Fact>> {
    fact_store
        .search_facts(FactSearchQuery {
            tenant: tenant.clone(),
            memory_types: Some(vec![candidate.memory_type]),
            query_text: Some(candidate.content.clone()),
            limit: RELATED_FACTS_SEARCH_LIMIT,
            cursor: None,
            include_deleted: false,
        })
        .await
}

/// How the legacy ingest path finds the stored facts a candidate is
/// classified against. `Legacy` (the default) is [`find_related_facts`], unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelatedFactMatcher {
    #[default]
    Legacy,
    /// Opt-in: [`find_related_facts_semantic`], and decisions applied through
    /// [`guard_related_decision`].
    Semantic,
    /// opt-in candidate: [`find_related_facts_semantic_v23`] (no memory-type filter,
    /// canonical subjects), decisions applied through [`guard_related_decision_v23`]. Not
    /// validated with a real classifier.
    SemanticV23,
}

impl RelatedFactMatcher {
    /// Both semantic matchers: decisions go through the history-keeping guard, and search leaves
    /// superseded facts out unless a request asks for history.
    pub fn is_semantic(self) -> bool {
        matches!(self, Self::Semantic | Self::SemanticV23)
    }

    /// The label used in configuration and audit metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Semantic => "semantic",
            Self::SemanticV23 => "semantic_v23",
        }
    }
}

/// Self-references a model writes as the subject of the user's own facts.
const SELF_SUBJECTS: [&str; 5] = ["user", "the user", "i", "me", "myself"];

/// A subject name for comparison: case- and whitespace-insensitive (as
/// [`normalized_subject`]), without surrounding punctuation, a trailing possessive or a leading
/// article ("The team's" and "team" are the same subject), and with the user's self-references
/// ("the user", "I") as one name. Distinct names are never merged: "user" and the user's own name,
/// or "Mum" and "user", stay different, because nothing here proves they are one person.
pub fn canonical_subject(subject: Option<&str>) -> Option<String> {
    let normalized = normalized_subject(subject)?;
    let mut value = normalized.trim_matches(|c: char| " .,;:!?\"'".contains(c));
    for suffix in ["'s", "\u{2019}s"] {
        if let Some(stripped) = value.strip_suffix(suffix) {
            value = stripped;
        }
    }
    for article in ["the ", "a ", "an "] {
        if let Some(stripped) = value.strip_prefix(article)
            && !stripped.is_empty()
        {
            value = stripped;
            break;
        }
    }
    let value = value.trim();
    if SELF_SUBJECTS.contains(&value) {
        return Some("user".to_string());
    }
    (!value.is_empty()).then(|| value.to_string())
}

/// Both facts name a subject and the canonical names differ.
fn different_canonical_subjects(candidate: &CandidateFact, fact: &Fact) -> bool {
    match (
        canonical_subject(candidate.structure.subject.as_deref()),
        canonical_subject(fact.structure.subject.as_deref()),
    ) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    }
}

/// Both facts name a subject and the names differ (case- and whitespace-insensitive).
fn different_subjects(candidate: &CandidateFact, fact: &Fact) -> bool {
    match (
        normalized_subject(candidate.structure.subject.as_deref()),
        normalized_subject(fact.structure.subject.as_deref()),
    ) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    }
}

/// Both facts carry an event time and the spans do not overlap: two events, not one fact restated.
fn distinct_event_times(candidate: &CandidateFact, fact: &Fact) -> bool {
    match (&candidate.structure.event_time, &fact.structure.event_time) {
        (Some(left), Some(right)) => left.end < right.start || right.end < left.start,
        _ => false,
    }
}

/// The opt-in related-fact lookup (`MEMCORE_RELATED_FACT_MATCHER=semantic`).
///
/// The legacy lookup returns only a stored fact containing the whole candidate text, which
/// exact, overlap and embedding dedup have already ruled out, so updates, corrections and
/// contradictions reach the classifier with nothing to compare (in measurements, very few related facts in
/// calls). This offers the user's facts of the candidate's memory type whose vectors are nearest the
/// candidate's own embedding (already computed for dedup), at or above the consolidation similarity
/// floor, best first, at most [`RELATED_FACTS_SEARCH_LIMIT`]; a hit that fails the dedup vector-hit
/// checks (another tenant, a stale or inactive row) or names a different subject is never offered.
/// Full-text hits are not offered: on recovered evaluation stores they were the large majority of
/// offers and mostly unrelated (generic subjects such as "the user" match any shared word). It only
/// offers facts: the classifier decides, and [`guard_related_decision`] limits what it may change.
pub async fn find_related_facts_semantic(
    fact_store: &dyn FactStore,
    vector_store: &dyn VectorStore,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    embedding: &[f32],
) -> MemcoreResult<Vec<Fact>> {
    let hits = vector_store
        .search_vectors(VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: embedding.to_vec(),
            limit: RELATED_FACTS_SEARCH_LIMIT.saturating_mul(2),
            memory_types: Some(vec![candidate.memory_type]),
            metadata_filter: None,
        })
        .await?;
    let mut related: Vec<Fact> = Vec::new();
    for hit in hits {
        if related.len() == RELATED_FACTS_SEARCH_LIMIT {
            break;
        }
        if hit.score < DEFAULT_CONSOLIDATION_SIMILARITY_FLOOR
            || related.iter().any(|fact| fact.id == hit.fact_id)
        {
            continue;
        }
        let fact = fact_store.get_fact(tenant, hit.fact_id).await?;
        if vector_hit_rejection(&hit, fact.as_ref(), tenant).is_some() {
            continue;
        }
        let Some(fact) = fact else {
            continue;
        };
        if fact.is_active()
            && fact.memory_type == candidate.memory_type
            && !different_subjects(candidate, &fact)
        {
            related.push(fact);
        }
    }
    Ok(related)
}

/// The candidate lookup (`MEMCORE_RELATED_FACT_MATCHER=semantic_v23`).
///
/// V2.2 (real gpt-4o-mini extraction and text-embedding-3-small) found the memory type to be the
/// largest cause of missed corrections: the model often files a restated value under another type
/// ("lives in Karen" as `profile`, "relocated to Mombasa" as `event`; 30 of 42 misses). This lookup
/// is [`find_related_facts_semantic`] without the memory-type filter: the user's nearest facts of
/// any type at or above the same similarity floor, best first, at most
/// [`RELATED_FACTS_SEARCH_LIMIT`]. Two checks remain:
/// - an `event` fact is offered only to an `event` candidate: an event is history and the guard
///   never lets another type update it, so offering it would only add a fact the classifier may
///   not change;
/// - subjects are compared by [`canonical_subject`] ("team" = "the team"); a different subject is
///   never offered.
pub async fn find_related_facts_semantic_v23(
    fact_store: &dyn FactStore,
    vector_store: &dyn VectorStore,
    tenant: &TenantContext,
    candidate: &CandidateFact,
    embedding: &[f32],
) -> MemcoreResult<Vec<Fact>> {
    let hits = vector_store
        .search_vectors(VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: embedding.to_vec(),
            limit: RELATED_FACTS_SEARCH_LIMIT.saturating_mul(2),
            memory_types: None,
            metadata_filter: None,
        })
        .await?;
    let mut related: Vec<Fact> = Vec::new();
    for hit in hits {
        if related.len() == RELATED_FACTS_SEARCH_LIMIT {
            break;
        }
        if hit.score < DEFAULT_CONSOLIDATION_SIMILARITY_FLOOR
            || related.iter().any(|fact| fact.id == hit.fact_id)
        {
            continue;
        }
        let fact = fact_store.get_fact(tenant, hit.fact_id).await?;
        if vector_hit_rejection(&hit, fact.as_ref(), tenant).is_some() {
            continue;
        }
        let Some(fact) = fact else {
            continue;
        };
        let cross_type_event =
            fact.memory_type == MemoryType::Event && candidate.memory_type != MemoryType::Event;
        if fact.is_active() && !cross_type_event && !different_canonical_subjects(candidate, &fact)
        {
            related.push(fact);
        }
    }
    Ok(related)
}

/// How a classifier decision is applied under the semantic matcher.
#[derive(Debug, Clone, PartialEq)]
pub enum RelatedDecision {
    /// Apply as the legacy path does: an Add, a NoOp, or an Update / Delete that was not allowed and
    /// is stored as an Add instead (`downgraded` names why). Nothing is ever dropped.
    Apply {
        decision: FactOperationDecision,
        downgraded: Option<&'static str>,
    },
    /// Store the candidate as a new fact and mark `target` superseded by it: the previous statement
    /// stays as history instead of being overwritten (legacy Update) or deleted.
    Supersede { target: Uuid },
}

/// Limits what an Update or Delete may change: only a fact that was offered, about the same subject
/// (or one without a subject), not an event, and not a separate event at another time ("worked at A
/// in 2022" and "works at B in 2026" are both kept). An event is history by nature: another event of
/// the same kind ("ran the Berlin marathon", then "ran the Paris marathon") never makes it outdated,
/// even without event times to compare. Anything else is stored as an Add.
pub fn guard_related_decision(
    decision: &FactOperationDecision,
    offered: &[Fact],
    candidate: &CandidateFact,
) -> RelatedDecision {
    guard_with(decision, offered, candidate, different_subjects)
}

/// [`guard_related_decision`] for the V2.3 candidate matcher: the same rules, with subjects
/// compared by [`canonical_subject`] as the lookup compares them.
pub fn guard_related_decision_v23(
    decision: &FactOperationDecision,
    offered: &[Fact],
    candidate: &CandidateFact,
) -> RelatedDecision {
    guard_with(decision, offered, candidate, different_canonical_subjects)
}

fn guard_with(
    decision: &FactOperationDecision,
    offered: &[Fact],
    candidate: &CandidateFact,
    different_subjects: fn(&CandidateFact, &Fact) -> bool,
) -> RelatedDecision {
    if !matches!(
        decision.operation,
        FactOperation::Update | FactOperation::Delete
    ) {
        return RelatedDecision::Apply {
            decision: decision.clone(),
            downgraded: None,
        };
    }
    let target = decision
        .target_fact_id
        .and_then(|id| offered.iter().find(|fact| fact.id == id));
    let refusal = match target {
        None => "target_not_offered",
        Some(fact) if different_subjects(candidate, fact) => "different_subject",
        Some(fact) if fact.memory_type == MemoryType::Event => "event_is_history",
        Some(fact) if distinct_event_times(candidate, fact) => "distinct_event_times",
        Some(fact) => return RelatedDecision::Supersede { target: fact.id },
    };
    RelatedDecision::Apply {
        decision: FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: Some(format!("{:?} not applied: {refusal}", decision.operation)),
            confidence: decision.confidence,
        },
        downgraded: Some(refusal),
    }
}
