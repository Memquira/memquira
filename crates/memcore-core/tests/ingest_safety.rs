//! Is S1 (`MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED`) semantically
//! equivalent to the classifier path? Invented data; a scripted classifier stands in for a model
//! (it is not a measurement of a real model's decisions).
//!
//! S1 skips the classification call when the related-fact lookup returns nothing and stores the
//! candidate as an ADD. That is equivalent only if the classifier would also have answered ADD
//! there. A classifier that declines a low-value statement (NOOP) with nothing related is the
//! counterexample: S1 changes the retention policy for exactly those candidates.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider,
    SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, MemoryEngine, MemoryMessage,
    MemoryType, MessageRole, RelatedFactMatcher, TenantContext,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

fn vector(text: &str) -> Vec<f32> {
    let mut v = [0.0_f32; 16];
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let h = word
            .bytes()
            .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
        v[(h % 16) as usize] += 1.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}

struct Embed;

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        Ok(vector(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        16
    }
}

/// Scripted classifier: declines `LOW_VALUE` (NOOP), updates the first offered fact for a
/// correction, adds everything else.
#[derive(Default)]
struct Llm {
    queue: Mutex<Vec<Vec<&'static str>>>,
    calls: AtomicUsize,
}

const LOW_VALUE: &str = "Rin said the weather was okay today.";
const LISBON: &str = "Rin lives in Lisbon near the river.";
const PORTO: &str = "Rin lives in Porto near the river.";
const KAYAK: &str = "Rin owns a red kayak.";

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            return Err(MemcoreError::ProviderError("nothing queued".to_string()));
        }
        Ok(FactExtractionOutput::from_candidates(
            q.remove(0)
                .into_iter()
                .map(|t| {
                    CandidateFact::new(t, MemoryType::Profile, 0.9, 0.9, None, json!({})).unwrap()
                })
                .collect(),
        ))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = input.candidate_fact.content.as_str();
        let (operation, target) = if text == LOW_VALUE {
            (FactOperation::NoOp, None)
        } else if text == PORTO && !input.existing_facts.is_empty() {
            (FactOperation::Update, Some(input.existing_facts[0].id))
        } else {
            (FactOperation::Add, None)
        };
        Ok(FactOperationDecision {
            operation,
            target_fact_id: target,
            reason: None,
            confidence: 0.9,
        })
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

/// Runs `script` and returns (active texts, classification calls).
async fn run(
    matcher: RelatedFactMatcher,
    s1: bool,
    script: &[&[&'static str]],
) -> (Vec<String>, usize) {
    let facts = Arc::new(MockFactStore::new());
    let llm = Arc::new(Llm::default());
    let engine = MemoryEngine::new(
        facts.clone(),
        Arc::new(MockVectorStore::new()),
        llm.clone(),
        Arc::new(Embed),
    )
    .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
        enabled: false,
        ..Default::default()
    })
    .with_related_fact_matcher(matcher)
    .with_classification_skip_when_no_related(s1);
    let t = TenantContext::new("org_v21_s1", "user_rin").unwrap();
    for texts in script {
        llm.queue.lock().unwrap().push(texts.to_vec());
        engine
            .add_memory(AddMemoryInput {
                tenant: t.clone(),
                messages: vec![MemoryMessage {
                    role: MessageRole::User,
                    content: "x".to_string(),
                    ..Default::default()
                }],
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .unwrap();
    }
    let mut active: Vec<String> = facts
        .search_facts(FactSearchQuery {
            tenant: t,
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
    active.sort();
    (active, llm.calls.load(Ordering::SeqCst))
}

#[tokio::test]
async fn s1_stores_a_statement_the_classifier_would_decline() {
    let script: &[&[&str]] = &[&[LOW_VALUE]];
    for matcher in [RelatedFactMatcher::Legacy, RelatedFactMatcher::Semantic] {
        let (off, off_calls) = run(matcher, false, script).await;
        let (on, on_calls) = run(matcher, true, script).await;
        assert!(
            off.is_empty(),
            "{matcher:?}: the classifier declines it (NOOP)"
        );
        assert_eq!(
            on,
            vec![LOW_VALUE.to_string()],
            "{matcher:?}: S1 stores it without asking"
        );
        assert_eq!((off_calls, on_calls), (1, 0));
    }
}

#[tokio::test]
async fn s1_matches_the_classifier_path_when_the_answer_could_only_be_add() {
    // A correction the legacy lookup misses (it offers nothing): both paths can only add, so both
    // keep the stale and the new value. The matcher, not S1, is the cause.
    let script: &[&[&str]] = &[&[LISBON], &[PORTO], &[KAYAK]];
    let (off, off_calls) = run(RelatedFactMatcher::Legacy, false, script).await;
    let (on, on_calls) = run(RelatedFactMatcher::Legacy, true, script).await;
    assert_eq!(off, on);
    assert_eq!(off.len(), 3, "legacy: Lisbon and Porto both stay current");
    assert_eq!((off_calls, on_calls), (3, 0));

    // The semantic lookup offers Lisbon for Porto: S1 does not apply there (a related fact exists),
    // the classifier runs on both paths, and the supersession is identical.
    let (off, off_calls) = run(RelatedFactMatcher::Semantic, false, script).await;
    let (on, on_calls) = run(RelatedFactMatcher::Semantic, true, script).await;
    assert_eq!(off, on);
    assert_eq!(off, vec![PORTO.to_string(), KAYAK.to_string()]);
    assert!(
        on_calls >= 1 && on_calls < off_calls,
        "S1 skips only the unrelated candidates: {on_calls} vs {off_calls}"
    );
}

#[tokio::test]
async fn s1_never_bypasses_exact_duplicate_detection() {
    let script: &[&[&str]] = &[&[KAYAK], &[KAYAK]];
    let (off, _) = run(RelatedFactMatcher::Legacy, false, script).await;
    let (on, _) = run(RelatedFactMatcher::Legacy, true, script).await;
    assert_eq!(off, vec![KAYAK.to_string()]);
    assert_eq!(
        on, off,
        "the exact duplicate is caught before S1 is considered"
    );
}
