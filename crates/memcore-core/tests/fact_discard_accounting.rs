//! Every extracted candidate is accounted for, with the reason it was not stored.
//!
//! Before this, all four rejection paths incremented a single `noop` counter, so
//! a rejected memory was invisible: the request succeeded, the fact was absent,
//! and nothing said whether it was a duplicate, below the importance threshold,
//! or declined by the classifier. These tests pin both the per-reason counts and
//! the invariant that makes them trustworthy:
//!
//! `extracted == added + updated + deleted + discarded.total()`

use std::sync::Arc;

use chrono::Utc;
use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, Fact, FactOperation, FactOperationDecision,
    FactStore, MemoryEngine, MemoryMessage, MemorySource, MemoryType, MessageRole, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant() -> TenantContext {
    TenantContext::new("org_discard", "user_discard").expect("tenant")
}

fn message() -> Vec<MemoryMessage> {
    vec![MemoryMessage {
        role: MessageRole::User,
        content: "placeholder extraction message".to_string(),
        ..Default::default()
    }]
}

fn candidate(content: &str, importance: f32) -> CandidateFact {
    CandidateFact::new(
        content,
        MemoryType::Conversation,
        0.9,
        importance,
        None,
        json!({}),
    )
    .expect("candidate")
}

fn engine(fact_store: Arc<MockFactStore>, llm: MockLlmProvider) -> MemoryEngine {
    MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
}

/// The whole point of the breakdown: nothing disappears unexplained.
fn assert_reconciles(output: &AddMemoryOutput) {
    assert_eq!(
        output.extracted,
        output.added + output.updated + output.deleted + output.discarded.total(),
        "every extracted candidate must end in exactly one outcome: {output:?}"
    );
    assert_eq!(
        output.noop,
        output.discarded.total(),
        "`noop` must stay equal to the total discarded for backward compatibility"
    );
    assert_eq!(output.stored(), output.added + output.updated);
}

async fn add(engine: &MemoryEngine) -> AddMemoryOutput {
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: message(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add_memory")
}

#[tokio::test]
async fn stored_candidates_are_counted_and_reconcile() {
    let llm = MockLlmProvider::new()
        .with_extraction_candidates(vec![candidate("user ships on fridays", 0.9)]);
    let output = add(&engine(Arc::new(MockFactStore::new()), llm)).await;

    assert_eq!(output.extracted, 1);
    assert_eq!(output.stored(), 1);
    assert_eq!(output.discarded.total(), 0);
    assert_reconciles(&output);
}

#[tokio::test]
async fn low_importance_candidates_are_reported_as_such() {
    // Below DEFAULT_MIN_IMPORTANCE (0.55): dropped before any store lookup.
    let llm = MockLlmProvider::new().with_extraction_candidates(vec![
        candidate("trivial aside", 0.05),
        candidate("another trivial aside", 0.05),
    ]);
    let output = add(&engine(Arc::new(MockFactStore::new()), llm)).await;

    assert_eq!(output.extracted, 2);
    assert_eq!(output.stored(), 0);
    assert_eq!(output.discarded.low_importance, 2);
    assert_eq!(output.discarded.total(), 2);
    assert_reconciles(&output);
}

#[tokio::test]
async fn duplicate_content_is_distinguished_from_other_rejections() {
    let store = Arc::new(MockFactStore::new());
    let existing = Fact::new(
        Uuid::new_v4(),
        "org_discard",
        "user_discard",
        MemoryType::Conversation,
        "user prefers dark mode in the editor",
        None,
        MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        Utc::now(),
        Utc::now(),
        json!({}),
    )
    .expect("fact");
    store
        .insert_fact(&tenant(), existing)
        .await
        .expect("seed existing fact");

    // Same wording: the token-overlap gate rejects it before embeddings.
    let llm = MockLlmProvider::new()
        .with_extraction_candidates(vec![candidate("user prefers dark mode in the editor", 0.9)]);
    let output = add(&engine(store, llm)).await;

    assert_eq!(output.extracted, 1);
    assert_eq!(output.stored(), 0);
    assert_eq!(
        output.discarded.duplicate_content, 1,
        "a duplicate must be reported as a duplicate, not as a generic no-op"
    );
    assert_eq!(output.discarded.low_importance, 0);
    assert_reconciles(&output);
}

#[tokio::test]
async fn classifier_no_op_is_distinguished_from_duplicates() {
    let llm = MockLlmProvider::new()
        .with_extraction_candidates(vec![candidate("a genuinely new fact about shipping", 0.9)])
        .with_classification_decision(FactOperationDecision {
            operation: FactOperation::NoOp,
            target_fact_id: None,
            reason: Some("nothing to do".to_string()),
            confidence: 0.9,
        });
    let output = add(&engine(Arc::new(MockFactStore::new()), llm)).await;

    assert_eq!(output.extracted, 1);
    assert_eq!(output.stored(), 0);
    assert_eq!(output.discarded.no_op_decision, 1);
    assert_eq!(output.discarded.duplicate_content, 0);
    assert_reconciles(&output);
}

#[tokio::test]
async fn mixed_outcomes_are_split_by_reason() {
    let llm = MockLlmProvider::new().with_extraction_candidates(vec![
        candidate("stored fact one", 0.9),
        candidate("trivial aside", 0.05),
        candidate("another trivial aside", 0.02),
    ]);
    let output = add(&engine(Arc::new(MockFactStore::new()), llm)).await;

    assert_eq!(output.extracted, 3);
    assert_eq!(output.stored(), 1);
    assert_eq!(output.discarded.low_importance, 2);
    assert_eq!(output.discarded.total(), 2);
    assert_reconciles(&output);
}
