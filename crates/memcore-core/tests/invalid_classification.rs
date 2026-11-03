//! One invalid classification output (a model returning
//! `"operation": "None"`) used to fail the whole write after the request's earlier candidates were already
//! stored, and reached the client as a 400. A `ProviderOutputInvalid` classification now rejects only
//! that candidate - not stored, not reinterpreted as an Add - and counts it as
//! `discarded.invalid_classification`; transport and provider failures still fail the request.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    FactClassificationInput, FactExtractionInput, LlmProvider, SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, MemoryEngine, MemoryMessage,
    MemoryType, MessageRole, TenantContext,
};
use memcore_providers::MockEmbeddingProvider;
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

type Decide = fn(&FactClassificationInput) -> MemcoreResult<FactOperationDecision>;

struct ScriptedLlm {
    extraction: Mutex<Option<Vec<CandidateFact>>>,
    decide: Decide,
    classified: Mutex<Vec<String>>,
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn extract_facts(
        &self,
        _input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        Ok(FactExtractionOutput::from_candidates(
            self.extraction.lock().unwrap().take().unwrap_or_default(),
        ))
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.classified
            .lock()
            .unwrap()
            .push(input.candidate_fact.content.clone());
        (self.decide)(&input)
    }

    async fn summarize_memory(&self, _input: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

fn add() -> FactOperationDecision {
    FactOperationDecision {
        operation: FactOperation::Add,
        target_fact_id: None,
        reason: None,
        confidence: 0.9,
    }
}

fn invalid_for_second(input: &FactClassificationInput) -> MemcoreResult<FactOperationDecision> {
    if input.candidate_fact.content.starts_with("Second") {
        Err(MemcoreError::provider_output_invalid(
            "classify_fact_operation",
            "invalid_operation",
        ))
    } else {
        Ok(add())
    }
}

fn transport_failure_for_second(
    input: &FactClassificationInput,
) -> MemcoreResult<FactOperationDecision> {
    if input.candidate_fact.content.starts_with("Second") {
        Err(MemcoreError::ProviderError("connection reset".to_string()))
    } else {
        Ok(add())
    }
}

fn candidate(content: &str) -> CandidateFact {
    CandidateFact::new(content, MemoryType::Event, 0.9, 0.8, None, json!({})).unwrap()
}

async fn run(
    decide: Decide,
) -> (
    MemcoreResult<memcore_core::AddMemoryOutput>,
    Vec<String>,
    Vec<String>,
) {
    let facts = Arc::new(MockFactStore::new());
    let llm = Arc::new(ScriptedLlm {
        extraction: Mutex::new(Some(vec![
            candidate("First fact: Maya adopted a cat named Pixel."),
            candidate("Second fact: Maya moved to Lisbon in March."),
            candidate("Third fact: Maya plays the cello on Sundays."),
        ])),
        decide,
        classified: Mutex::new(Vec::new()),
    });
    let engine = MemoryEngine::new(
        facts.clone(),
        Arc::new(MockVectorStore::new()),
        llm.clone(),
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
        enabled: false,
        ..Default::default()
    });
    let tenant = TenantContext::new("org_oafs", "user_maya").unwrap();
    let out = engine
        .add_memory(AddMemoryInput {
            tenant: tenant.clone(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Maya told me about her week.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await;
    let mut stored: Vec<String> = facts
        .search_facts(FactSearchQuery {
            tenant,
            memory_types: None,
            query_text: None,
            limit: 100,
            cursor: None,
            include_deleted: false,
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|f| f.is_active())
        .map(|f| f.content)
        .collect();
    stored.sort();
    let classified = llm.classified.lock().unwrap().clone();
    (out, stored, classified)
}

#[tokio::test]
async fn an_invalid_classification_rejects_only_its_candidate() {
    let (out, stored, classified) = run(invalid_for_second).await;
    let out = out.expect("the write succeeds; only the invalid candidate is rejected");
    assert_eq!(classified.len(), 3, "every candidate is still classified");
    assert_eq!(out.added, 2);
    assert_eq!(out.discarded.invalid_classification, 1);
    assert_eq!(out.discarded.total(), 1);
    assert_eq!(
        out.extracted,
        out.added + out.updated + out.deleted + out.discarded.total() + out.skipped.total(),
        "every extracted fact is accounted for"
    );
    assert_eq!(
        stored,
        vec![
            "First fact: Maya adopted a cat named Pixel.".to_string(),
            "Third fact: Maya plays the cello on Sundays.".to_string(),
        ],
        "the rejected candidate is not stored and nothing is reinterpreted"
    );
}

#[tokio::test]
async fn a_transport_failure_still_fails_the_write() {
    let (out, stored, classified) = run(transport_failure_for_second).await;
    let err = out.expect_err("a provider/transport failure is not swallowed");
    assert!(matches!(err, MemcoreError::ProviderError(_)), "{err:?}");
    assert_eq!(
        classified.len(),
        2,
        "processing stops at the failing candidate"
    );
    // The facts this request added before the failure are compensated (they
    // used to stay stored - a partial write), so the store holds no active fact.
    assert!(stored.is_empty(), "compensated: {stored:?}");
}
