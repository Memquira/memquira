mod detector;
mod executor;
mod types;

pub use detector::{
    EventUpdatePolicy, RelatedDecision, RelatedFactMatcher, canonical_subject, find_related_facts,
    find_related_facts_semantic, find_related_facts_semantic_v23,
    find_related_facts_semantic_v23_with_floor, find_related_facts_semantic_with_floor,
    guard_related_decision, guard_related_decision_v23, guard_related_decision_v23_with_policy,
    states_a_change,
};
pub use executor::{
    LifecycleApplyResult, LifecycleContext, apply_fact_operation, apply_fact_operation_with_id,
    apply_fact_operation_with_rule, apply_invalidate, apply_update_with_content,
    link_superseded_by, normalize_operation,
};
pub(crate) use executor::{checked_embedding_from, restore_fact, vector_record_from_fact};
pub use types::RELATED_FACTS_SEARCH_LIMIT;
