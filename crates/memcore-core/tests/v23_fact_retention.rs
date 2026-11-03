//! Plain durable facts must not be discarded only because the extractor rated
//! them medium (0.5) and their type gets no stable-type boost (V2.2 defect D2: "The cloud provider
//! is AWS." extracted as `entity` / `other` at 0.5, then dropped below the 0.55 floor).
//!
//! `MEMCORE_IMPORTANCE_POLICY=durable` keeps them; filler, low ratings, conversational and system
//! content stay out, and the default (`legacy`) is unchanged.

use std::sync::Arc;

use memcore_core::{
    AddMemoryInput, AddMemoryOutput, CandidateFact, ImportancePolicy, MemoryEngine, MemoryMessage,
    MemoryType, MessageRole, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

fn candidate(content: &str, memory_type: MemoryType, importance: f32) -> CandidateFact {
    CandidateFact::new(content, memory_type, 0.9, importance, None, json!({})).expect("candidate")
}

/// (content, memory type, model importance, stored under `durable`)
fn candidates() -> Vec<(&'static str, MemoryType, f32, bool)> {
    vec![
        // The V2.2 losses: durable facts at the extractor's medium rating.
        ("The cloud provider is AWS.", MemoryType::Entity, 0.5, true),
        ("The cloud provider is GCP.", MemoryType::Other, 0.5, true),
        (
            "The user's desk is on the 3rd floor.",
            MemoryType::Other,
            0.5,
            true,
        ),
        (
            "The user was moved to the 5th floor.",
            MemoryType::Event,
            0.5,
            true,
        ),
        (
            "The user is reading a novel about forests.",
            MemoryType::Knowledge,
            0.5,
            true,
        ),
        // Already stored by the legacy policy (stable-type boost or a high rating).
        (
            "The user drinks three cups of coffee a day.",
            MemoryType::Preference,
            0.5,
            true,
        ),
        (
            "The user works as a dispatcher.",
            MemoryType::Profile,
            0.8,
            true,
        ),
        // Never stored: conversational or system content, a low rating, filler.
        (
            "The user said they would think about it.",
            MemoryType::Conversation,
            0.5,
            false,
        ),
        (
            "The assistant reported a timeout.",
            MemoryType::System,
            0.5,
            false,
        ),
        (
            "The user mentioned the weather in passing.",
            MemoryType::Other,
            0.4,
            false,
        ),
        ("User said okay.", MemoryType::Other, 0.6, false),
        ("ok", MemoryType::Entity, 0.7, false),
    ]
}

async fn run(policy: Option<ImportancePolicy>) -> (AddMemoryOutput, Vec<String>) {
    let llm = MockLlmProvider::new().with_extraction_candidates(
        candidates()
            .into_iter()
            .map(|(content, memory_type, importance, _)| {
                candidate(content, memory_type, importance)
            })
            .collect(),
    );
    let facts = Arc::new(MockFactStore::new());
    let mut engine = MemoryEngine::new(
        facts,
        Arc::new(MockVectorStore::new()),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(16)),
    );
    if let Some(policy) = policy {
        engine = engine.with_importance_policy(policy);
    }
    let output = engine
        .add_memory(AddMemoryInput {
            tenant: TenantContext::new("org_v23", "user_retention").expect("tenant"),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "placeholder conversation".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add_memory");
    let stored = output
        .memories
        .iter()
        .map(|fact| fact.content.clone())
        .collect();
    (output, stored)
}

#[tokio::test]
async fn durable_policy_keeps_plain_facts_and_nothing_else() {
    let (output, stored) = run(Some(ImportancePolicy::Durable)).await;
    for (content, _, _, expected) in candidates() {
        assert_eq!(
            stored.iter().any(|fact| fact == content),
            expected,
            "{content:?} stored = {stored:?}"
        );
    }
    let expected_stored = candidates().iter().filter(|c| c.3).count();
    assert_eq!(output.stored(), expected_stored);
    assert_eq!(
        output.discarded.low_importance,
        candidates().len() - expected_stored
    );
    assert_eq!(
        output.extracted,
        output.added + output.updated + output.deleted + output.discarded.total()
    );
}

#[tokio::test]
async fn the_default_policy_is_unchanged() {
    let (default, stored) = run(None).await;
    let (legacy, _) = run(Some(ImportancePolicy::Legacy)).await;
    assert_eq!(default.stored(), legacy.stored());
    // Exactly the two that the legacy floor already admitted.
    assert_eq!(
        stored,
        vec![
            "The user drinks three cups of coffee a day.".to_string(),
            "The user works as a dispatcher.".to_string()
        ]
    );
    assert_eq!(default.discarded.low_importance, candidates().len() - 2);
}

/// The durable policy adds at most the facts the legacy floor dropped, never a duplicate.
#[tokio::test]
async fn durable_policy_does_not_duplicate_on_retry() {
    let llm = MockLlmProvider::new().with_extraction_candidates(vec![candidate(
        "The cloud provider is AWS.",
        MemoryType::Entity,
        0.5,
    )]);
    let engine = MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(llm),
        Arc::new(MockEmbeddingProvider::new(16)),
    )
    .with_importance_policy(ImportancePolicy::Durable);
    let input = || AddMemoryInput {
        tenant: TenantContext::new("org_v23", "user_retry").expect("tenant"),
        messages: vec![MemoryMessage {
            role: MessageRole::User,
            content: "placeholder conversation".to_string(),
            ..Default::default()
        }],
        metadata: json!({}),
        session: Default::default(),
    };
    let first = engine.add_memory(input()).await.expect("first");
    let second = engine.add_memory(input()).await.expect("second");
    assert_eq!(first.stored(), 1);
    assert_eq!(second.stored(), 0);
    assert_eq!(
        second.discarded.total(),
        1,
        "the repeat is a duplicate: {second:?}"
    );
}
