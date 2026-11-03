use memcore_common::MemcoreResult;

use crate::ports::{FactSearchQuery, FactStore};
use crate::{CandidateFact, Fact, TenantContext};

use super::types::{
    DEDUP_SEARCH_LIMIT, DeduplicationDecision, EXACT_DUPLICATE_THRESHOLD,
    HIGH_SIMILARITY_DUPLICATE_THRESHOLD, MODERATE_SIMILARITY_THRESHOLD,
};

/// Normalizes fact content for duplicate comparison.
///
/// Rules: trim, lowercase, collapse internal whitespace, strip simple trailing punctuation.
pub fn normalize_content(content: &str) -> String {
    let trimmed = content.trim().to_ascii_lowercase();
    let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    strip_trailing_punctuation(&collapsed)
}

/// Which exact-duplicate rule a store-enforced key encodes. Each ingestion mode
/// writes the key of its own rule, so the store enforces exactly what that mode's sequential
/// check calls an exact duplicate, no more and no less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactDedupRule {
    /// The default path ([`detect_duplicate`]'s first test): same memory type and
    /// [`normalize_content`], whatever the subject or event time.
    Legacy,
    /// Consolidation v2's fast path (`consolidation::fast_path` over related facts of every type):
    /// same [`normalize_content`], normalized subject and event-time range, whatever the memory
    /// type.
    ConsolidationV2,
}

/// The store-enforced key of `fact` under `rule`, SHA-256 over a rule tag and the
/// rule's fields. Keys of different rules never collide, so a key written by one mode never makes
/// the other mode merge anything. The token-overlap, embedding and LLM rules are not encoded (they
/// stay check-then-insert).
pub fn exact_dedup_key(rule: ExactDedupRule, fact: &crate::Fact) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut part = |value: Option<&str>| {
        match value {
            Some(value) => {
                hasher.update(b"1");
                hasher.update(value.as_bytes());
            }
            None => hasher.update(b"0"),
        }
        hasher.update(b"\0");
    };
    match rule {
        ExactDedupRule::Legacy => {
            part(Some("memcore.exact-dedup.legacy.v1"));
            part(Some(fact.memory_type.as_str()));
            part(Some(&normalize_content(&fact.content)));
        }
        ExactDedupRule::ConsolidationV2 => {
            let subject =
                crate::consolidation::normalized_subject(fact.structure.subject.as_deref());
            let event_time = fact.structure.event_time.as_ref().map(|time| {
                let format = |at: chrono::DateTime<chrono::Utc>| {
                    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
                };
                format!("{}/{}", format(time.start), format(time.end))
            });
            part(Some("memcore.exact-dedup.consolidation-v2.v1"));
            part(Some(&normalize_content(&fact.content)));
            part(subject.as_deref());
            part(event_time.as_deref());
        }
    }
    hex::encode(hasher.finalize())
}

fn strip_trailing_punctuation(content: &str) -> String {
    content
        .trim_end_matches(['.', ',', '!', '?', ';', ':'])
        .to_string()
}

fn token_set(content: &str) -> std::collections::HashSet<String> {
    normalize_content(content)
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// Jaccard similarity between token sets of two strings (0.0–1.0).
pub fn token_overlap_ratio(left: &str, right: &str) -> f32 {
    let left_tokens = token_set(left);
    let right_tokens = token_set(right);

    if left_tokens.is_empty() && right_tokens.is_empty() {
        return EXACT_DUPLICATE_THRESHOLD;
    }

    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }

    let intersection = left_tokens.intersection(&right_tokens).count();
    let union = left_tokens.union(&right_tokens).count();
    intersection as f32 / union as f32
}

/// Loads active facts for the tenant and candidate memory type for deduplication checks.
pub async fn find_existing_facts_for_dedup(
    fact_store: &dyn FactStore,
    tenant: &TenantContext,
    memory_type: crate::MemoryType,
) -> MemcoreResult<Vec<Fact>> {
    fact_store
        .search_facts(FactSearchQuery {
            tenant: tenant.clone(),
            memory_types: Some(vec![memory_type]),
            query_text: None,
            limit: DEDUP_SEARCH_LIMIT,
            cursor: None,
            include_deleted: false,
        })
        .await
}

/// Evaluates whether a candidate duplicates an existing fact.
pub fn detect_duplicate(
    candidate: &CandidateFact,
    existing_facts: &[Fact],
) -> DeduplicationDecision {
    let normalized_candidate = normalize_content(&candidate.content);
    let mut best_moderate: Option<(f32, String)> = None;

    for existing in existing_facts {
        if existing.memory_type != candidate.memory_type {
            continue;
        }

        let normalized_existing = normalize_content(&existing.content);
        if normalized_candidate == normalized_existing {
            return DeduplicationDecision::Duplicate {
                existing_fact_id: existing.id,
                reason: "exact normalized content match".to_string(),
            };
        }

        let overlap = token_overlap_ratio(&candidate.content, &existing.content);
        if overlap >= HIGH_SIMILARITY_DUPLICATE_THRESHOLD {
            return DeduplicationDecision::Duplicate {
                existing_fact_id: existing.id,
                reason: format!("high token overlap ({overlap:.2})"),
            };
        }

        if overlap >= MODERATE_SIMILARITY_THRESHOLD {
            let reason = format!("moderate token overlap ({overlap:.2}) with existing fact");
            match &best_moderate {
                Some((best, _)) if *best >= overlap => {}
                _ => best_moderate = Some((overlap, reason)),
            }
        }
    }

    if let Some((_, reason)) = best_moderate {
        return DeduplicationDecision::SimilarButDistinct { reason };
    }

    DeduplicationDecision::New
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_whitespace() {
        assert_eq!(normalize_content("  hello  "), "hello");
    }

    #[test]
    fn normalize_lowercases() {
        assert_eq!(normalize_content("Rust"), "rust");
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(
            normalize_content("user   is   learning"),
            "user is learning"
        );
    }

    #[test]
    fn normalize_strips_trailing_punctuation() {
        assert_eq!(
            normalize_content("User is learning Rust."),
            "user is learning rust"
        );
        assert_eq!(
            normalize_content("User is learning Rust!!!"),
            "user is learning rust"
        );
    }

    #[test]
    fn normalize_preserves_meaningful_content() {
        assert_eq!(
            normalize_content("User is learning Rust async"),
            "user is learning rust async"
        );
    }

    #[test]
    fn duplicate_examples_normalize_equally() {
        let a = normalize_content("User is learning Rust.");
        let b = normalize_content("user is learning rust");
        let c = normalize_content("User is learning Rust");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn token_overlap_is_one_for_equivalent_content() {
        let overlap = token_overlap_ratio("User is learning Rust.", "user is learning rust");
        assert!((overlap - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn token_overlap_is_low_for_distinct_content() {
        let overlap = token_overlap_ratio(
            "User is learning Rust async",
            "User enjoys hiking on weekends",
        );
        assert!(overlap < MODERATE_SIMILARITY_THRESHOLD);
    }

    #[test]
    fn detect_exact_duplicate() {
        use chrono::Utc;
        use serde_json::json;
        use uuid::Uuid;

        let existing = Fact::new(
            Uuid::new_v4(),
            "org_a",
            "user_a",
            crate::MemoryType::Skill,
            "User is learning Rust.",
            None,
            crate::MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            Utc::now(),
            Utc::now(),
            json!({}),
        )
        .expect("fact");

        let candidate = CandidateFact::new(
            "user is learning rust",
            crate::MemoryType::Skill,
            0.9,
            0.8,
            None,
            json!({}),
        )
        .expect("candidate");

        match detect_duplicate(&candidate, &[existing]) {
            DeduplicationDecision::Duplicate { .. } => {}
            other => panic!("expected duplicate, got {other:?}"),
        }
    }

    #[test]
    fn detect_high_overlap_duplicate() {
        use chrono::Utc;
        use serde_json::json;
        use uuid::Uuid;

        let existing = Fact::new(
            Uuid::new_v4(),
            "org_a",
            "user_a",
            crate::MemoryType::Skill,
            "alpha beta gamma delta epsilon zeta eta theta iota",
            None,
            crate::MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            Utc::now(),
            Utc::now(),
            json!({}),
        )
        .expect("fact");

        let candidate = CandidateFact::new(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa",
            crate::MemoryType::Skill,
            0.9,
            0.8,
            None,
            json!({}),
        )
        .expect("candidate");

        let overlap = token_overlap_ratio(&candidate.content, &existing.content);
        assert!(overlap >= HIGH_SIMILARITY_DUPLICATE_THRESHOLD);

        match detect_duplicate(&candidate, &[existing]) {
            DeduplicationDecision::Duplicate { .. } => {}
            other => panic!("expected duplicate, got {other:?}"),
        }
    }

    #[test]
    fn detect_distinct_memory() {
        use chrono::Utc;
        use serde_json::json;
        use uuid::Uuid;

        let existing = Fact::new(
            Uuid::new_v4(),
            "org_a",
            "user_a",
            crate::MemoryType::Skill,
            "User is learning Rust",
            None,
            crate::MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            Utc::now(),
            Utc::now(),
            json!({}),
        )
        .expect("fact");

        let candidate = CandidateFact::new(
            "User enjoys hiking on weekends",
            crate::MemoryType::Skill,
            0.9,
            0.8,
            None,
            json!({}),
        )
        .expect("candidate");

        assert_eq!(
            detect_duplicate(&candidate, &[existing]),
            DeduplicationDecision::New
        );
    }
}
