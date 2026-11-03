use crate::{CandidateFact, MemoryType};

/// Boost applied to stable long-term memory types after provider scoring.
const STABLE_TYPE_BOOST: f32 = 0.08;

/// Penalty for very short fact content.
const SHORT_CONTENT_PENALTY: f32 = 0.18;

/// Penalty for vague or temporary-sounding facts.
const VAGUE_CONTENT_PENALTY: f32 = 0.22;

/// Content shorter than this (after trim) is considered low value.
const SHORT_CONTENT_MAX_LEN: usize = 12;

/// Which adjusted importances pass the ingest threshold (`MEMCORE_IMPORTANCE_POLICY`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImportancePolicy {
    /// A candidate is stored when its adjusted importance reaches `min_importance`. Default.
    #[default]
    Legacy,
    /// A fact the extractor itself rated at least medium
    /// ([`DURABLE_FACT_IMPORTANCE_FLOOR`]) is also stored, unless it is conversational or system
    /// content. V2.2 found plain durable facts ("The cloud provider is AWS.", "The user's desk is on
    /// the 3rd floor.") extracted at 0.5 and then discarded below the 0.55 floor because their type
    /// (entity, other, event) gets no stable-type boost. The short and vague penalties still apply
    /// first, so filler stays out.
    Durable,
}

/// The medium importance a durable fact needs under [`ImportancePolicy::Durable`].
pub const DURABLE_FACT_IMPORTANCE_FLOOR: f32 = 0.5;

impl ImportancePolicy {
    /// Whether an adjusted candidate passes the ingest threshold.
    pub fn passes(self, candidate: &CandidateFact, min_importance: f32) -> bool {
        if candidate.importance >= min_importance {
            return true;
        }
        self == Self::Durable
            && !matches!(
                candidate.memory_type,
                MemoryType::Conversation | MemoryType::System
            )
            && candidate.importance >= DURABLE_FACT_IMPORTANCE_FLOOR
    }
}

/// Rule-based importance adjustment on top of provider scores.
///
/// Future work: drive thresholds from `Settings` / config.
pub struct ImportanceScorer;

impl ImportanceScorer {
    pub fn adjust(candidate: &CandidateFact) -> CandidateFact {
        let mut importance = candidate.importance.clamp(0.0, 1.0);

        if is_stable_memory_type(candidate.memory_type) {
            importance += STABLE_TYPE_BOOST;
        }

        if is_short_content(&candidate.content) {
            importance -= SHORT_CONTENT_PENALTY;
        }

        if is_vague_or_temporary(&candidate.content) {
            importance -= VAGUE_CONTENT_PENALTY;
        }

        CandidateFact {
            importance: importance.clamp(0.0, 1.0),
            ..candidate.clone()
        }
    }
}

fn is_stable_memory_type(memory_type: MemoryType) -> bool {
    matches!(
        memory_type,
        MemoryType::Profile | MemoryType::Preference | MemoryType::Project | MemoryType::Skill
    )
}

fn is_short_content(content: &str) -> bool {
    content.trim().len() < SHORT_CONTENT_MAX_LEN
}

fn is_vague_or_temporary(content: &str) -> bool {
    let normalized = content.trim().to_ascii_lowercase();
    const VAGUE_PATTERNS: &[&str] = &[
        "user said okay",
        "user asked a question",
        "user is here today",
        "okay",
        "ok",
        "user said ok",
        "user said yes",
        "user said no",
    ];

    VAGUE_PATTERNS
        .iter()
        .any(|pattern| normalized == *pattern || normalized.starts_with(&format!("{pattern}.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidate(content: &str, memory_type: MemoryType, importance: f32) -> CandidateFact {
        CandidateFact::new(content, memory_type, 0.9, importance, None, json!({}))
            .expect("candidate")
    }

    #[test]
    fn importance_is_clamped_to_unit_interval() {
        let high = candidate(
            "stable profile fact content here",
            MemoryType::Profile,
            0.95,
        );
        let adjusted_high = ImportanceScorer::adjust(&high);
        assert_eq!(adjusted_high.importance, 1.0);

        let low = candidate("ok", MemoryType::Conversation, 0.0);
        let adjusted_low = ImportanceScorer::adjust(&low);
        assert_eq!(adjusted_low.importance, 0.0);
    }

    #[test]
    fn stable_memory_type_receives_boost() {
        let base = candidate(
            "User prefers Rust for backend work",
            MemoryType::Preference,
            0.6,
        );
        let adjusted = ImportanceScorer::adjust(&base);
        assert!(adjusted.importance > base.importance);
    }

    #[test]
    fn vague_fact_receives_penalty() {
        let vague = candidate("User said okay.", MemoryType::Conversation, 0.8);
        let adjusted = ImportanceScorer::adjust(&vague);
        assert!(adjusted.importance < vague.importance);
    }

    #[test]
    fn short_content_receives_penalty() {
        let short = candidate("User said ok", MemoryType::Conversation, 0.8);
        let adjusted = ImportanceScorer::adjust(&short);
        assert!(adjusted.importance < short.importance);
    }

    #[test]
    fn low_importance_after_penalties_can_fall_below_default_threshold() {
        let vague = candidate("User is here today.", MemoryType::Conversation, 0.6);
        let adjusted = ImportanceScorer::adjust(&vague);
        assert!(adjusted.importance < crate::DEFAULT_MIN_IMPORTANCE);
    }
}
