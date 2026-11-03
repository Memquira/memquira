use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    CandidateFact, FactExtractionOutput, FactOperation, FactOperationDecision, MemoryType,
};

use memcore_core::ports::{
    ConsolidationDecision, ConsolidationInput, ConsolidationOperation, EmbeddingProvider,
    EntityAnnotationInput, EntityAnnotationOutput, EntityMatchInput, EntityMention, FactAnnotation,
    FactClassificationInput, FactExtractionInput, LlmProvider, MemoryMessage, MessageRole,
    SummarizationInput,
};

/// Scripts an entity annotation from its input (entity tests).
pub type EntityAnnotator =
    std::sync::Arc<dyn Fn(&EntityAnnotationInput) -> EntityAnnotationOutput + Send + Sync>;

/// Scripts an entity match confirmation from its input.
pub type EntityMatcher = std::sync::Arc<dyn Fn(&EntityMatchInput) -> bool + Send + Sync>;

struct Scripted<T>(Option<T>);

impl<T> Default for Scripted<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> std::fmt::Debug for Scripted<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.0.is_some() {
            "Some(<scripted>)"
        } else {
            "None"
        })
    }
}

/// Scripts a consolidation v2 decision from the input it is offered (tests pick targets from
/// `input.existing_facts`, whose IDs are only known at run time).
pub type ConsolidationDecider =
    std::sync::Arc<dyn Fn(&ConsolidationInput) -> ConsolidationDecision + Send + Sync>;

#[derive(Default)]
struct DeciderSlot(Option<ConsolidationDecider>);

impl std::fmt::Debug for DeciderSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.0.is_some() {
            "Some(<decider>)"
        } else {
            "None"
        })
    }
}

fn check_fail(error: &Option<MemcoreError>) -> MemcoreResult<()> {
    if let Some(err) = error {
        return Err(err.clone());
    }
    Ok(())
}

pub fn deterministic_embedding(text: &str, dimensions: usize) -> MemcoreResult<Vec<f32>> {
    if text.trim().is_empty() {
        return Err(MemcoreError::ValidationError(
            "embedding text cannot be empty".to_string(),
        ));
    }

    if dimensions == 0 {
        return Err(MemcoreError::ValidationError(
            "embedding dimensions must be greater than 0".to_string(),
        ));
    }

    let mut embedding = vec![0.0_f32; dimensions];
    for (index, byte) in text.as_bytes().iter().enumerate() {
        let slot = (index.wrapping_mul(17) ^ (*byte as usize).wrapping_mul(31)) % dimensions;
        embedding[slot] += f32::from(*byte) / 255.0;
    }

    let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut embedding {
            *value /= norm;
        }
    }

    Ok(embedding)
}

#[derive(Debug, Default)]
pub struct MockLlmProvider {
    extraction_candidates: RwLock<Option<Vec<CandidateFact>>>,
    classification_decision: RwLock<Option<FactOperationDecision>>,
    classification_decisions: RwLock<Vec<FactOperationDecision>>,
    classification_fail_with: RwLock<Option<MemcoreError>>,
    summary_prefix: RwLock<String>,
    fail_with: RwLock<Option<MemcoreError>>,
    last_extraction_messages: RwLock<Vec<MemoryMessage>>,
    consolidation_decider: RwLock<DeciderSlot>,
    consolidation_inputs: RwLock<Vec<ConsolidationInput>>,
    entity_annotator: RwLock<Scripted<EntityAnnotator>>,
    annotation_inputs: RwLock<Vec<EntityAnnotationInput>>,
    entity_matcher: RwLock<Scripted<EntityMatcher>>,
    match_inputs: RwLock<Vec<EntityMatchInput>>,
}

impl MockLlmProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_extraction_candidates(self, candidates: Vec<CandidateFact>) -> Self {
        *self
            .extraction_candidates
            .write()
            .expect("extraction lock poisoned") = Some(candidates);
        self
    }

    pub fn with_classification_decision(self, decision: FactOperationDecision) -> Self {
        *self
            .classification_decision
            .write()
            .expect("classification lock poisoned") = Some(decision);
        self
    }

    /// Returns one decision per `classify_fact_operation` call, in order.
    pub fn with_classification_decisions(self, decisions: Vec<FactOperationDecision>) -> Self {
        *self
            .classification_decisions
            .write()
            .expect("classification queue lock poisoned") = decisions;
        self
    }

    pub fn with_classification_fail_error(self, error: MemcoreError) -> Self {
        *self
            .classification_fail_with
            .write()
            .expect("classification fail lock poisoned") = Some(error);
        self
    }

    /// Consolidation v2 decisions come from `decider` (default: ADD).
    pub fn with_consolidation_decider(self, decider: ConsolidationDecider) -> Self {
        *self
            .consolidation_decider
            .write()
            .expect("consolidation decider lock poisoned") = DeciderSlot(Some(decider));
        self
    }

    /// Entity annotations come from `annotator`. Default: each fact with a subject names that
    /// subject as a person and the fact's subject.
    pub fn with_entity_annotator(self, annotator: EntityAnnotator) -> Self {
        *self
            .entity_annotator
            .write()
            .expect("entity annotator lock poisoned") = Scripted(Some(annotator));
        self
    }

    /// Entity match confirmations come from `matcher` (default: not the same entity).
    pub fn with_entity_matcher(self, matcher: EntityMatcher) -> Self {
        *self
            .entity_matcher
            .write()
            .expect("entity matcher lock poisoned") = Scripted(Some(matcher));
        self
    }

    /// Every input passed to [`LlmProvider::annotate_entities`], in order (for tests).
    pub fn annotation_inputs(&self) -> Vec<EntityAnnotationInput> {
        self.annotation_inputs
            .read()
            .expect("annotation inputs lock poisoned")
            .clone()
    }

    /// Every input passed to [`LlmProvider::confirm_entity_match`], in order (for tests).
    pub fn match_inputs(&self) -> Vec<EntityMatchInput> {
        self.match_inputs
            .read()
            .expect("match inputs lock poisoned")
            .clone()
    }

    /// Every input passed to [`LlmProvider::decide_consolidation`], in order (for tests).
    pub fn consolidation_inputs(&self) -> Vec<ConsolidationInput> {
        self.consolidation_inputs
            .read()
            .expect("consolidation inputs lock poisoned")
            .clone()
    }

    pub fn with_summary_prefix(self, prefix: impl Into<String>) -> Self {
        *self.summary_prefix.write().expect("summary lock poisoned") = prefix.into();
        self
    }

    pub fn with_fail_error(self, error: MemcoreError) -> Self {
        *self.fail_with.write().expect("fail lock poisoned") = Some(error);
        self
    }

    /// Messages most recently passed to [`LlmProvider::extract_facts`] (for tests).
    pub fn last_extraction_messages(&self) -> Vec<MemoryMessage> {
        self.last_extraction_messages
            .read()
            .expect("extraction messages lock poisoned")
            .clone()
    }

    fn default_candidates(messages: &[MemoryMessage]) -> MemcoreResult<Vec<CandidateFact>> {
        let mut candidates = Vec::new();

        for message in messages {
            if message.role != MessageRole::User {
                continue;
            }

            let content = message.content.trim();
            if content.is_empty() {
                continue;
            }

            candidates.push(CandidateFact::new(
                content,
                MemoryType::Conversation,
                0.85,
                0.7,
                None,
                serde_json::json!({ "source": "mock_llm" }),
            )?);
        }

        Ok(candidates)
    }

    fn default_classification() -> FactOperationDecision {
        FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: Some("mock default add operation".to_string()),
            confidence: 0.9,
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;

        *self
            .last_extraction_messages
            .write()
            .expect("extraction messages lock poisoned") = input.messages.clone();

        if let Some(candidates) = self
            .extraction_candidates
            .read()
            .expect("extraction lock poisoned")
            .clone()
        {
            return Ok(FactExtractionOutput::from_candidates(candidates));
        }

        // A mock never rejects its own output, so the skip counts stay zero here. That is what
        // makes `memcore_fact_candidates_skipped_total == 0` a meaningful assertion in tests.
        let candidates = Self::default_candidates(&input.messages)?;
        if input.mode == memcore_core::ExtractionMode::Structured {
            // Structured mode attributes each fact to its speaker and message, as a real model is
            // asked to, so engine tests exercise the full structured path without a network call.
            let attributed = input
                .messages
                .iter()
                .filter(|message| {
                    message.role == MessageRole::User && !message.content.trim().is_empty()
                })
                .zip(candidates)
                .map(|(message, candidate)| {
                    candidate.with_structure(memcore_core::FactStructure {
                        subject: message.name.clone(),
                        observed_at: message.timestamp,
                        source_message_ids: message.message_id.iter().cloned().collect(),
                        ..Default::default()
                    })
                })
                .collect();
            return Ok(FactExtractionOutput::from_candidates(attributed));
        }
        Ok(FactExtractionOutput::from_candidates(candidates))
    }

    async fn decide_consolidation(
        &self,
        input: ConsolidationInput,
    ) -> MemcoreResult<ConsolidationDecision> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        let decider = self
            .consolidation_decider
            .read()
            .expect("consolidation decider lock poisoned")
            .0
            .clone();
        let decision = match &decider {
            Some(decide) => decide(&input),
            None => ConsolidationDecision {
                operation: ConsolidationOperation::Add,
                target_fact_id: None,
                merged_content: None,
                invalid_at: None,
                invalid_at_unparsed: None,
                reason: Some("mock default add".to_string()),
            },
        };
        self.consolidation_inputs
            .write()
            .expect("consolidation inputs lock poisoned")
            .push(input);
        Ok(decision)
    }

    /// Deterministic: never decomposes (`{"sub_queries": []}`), so mock-backed servers make no
    /// model call and change nothing.
    async fn decompose_query(
        &self,
        _input: memcore_core::ports::QueryDecompositionInput,
    ) -> MemcoreResult<String> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        Ok(r#"{"sub_queries": []}"#.to_string())
    }

    async fn annotate_entities(
        &self,
        input: EntityAnnotationInput,
    ) -> MemcoreResult<EntityAnnotationOutput> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        let annotator = self
            .entity_annotator
            .read()
            .expect("entity annotator lock poisoned")
            .0
            .clone();
        let output = match &annotator {
            Some(annotate) => annotate(&input),
            None => EntityAnnotationOutput {
                facts: input
                    .facts
                    .iter()
                    .map(|fact| FactAnnotation {
                        fact_index: fact.index,
                        entities: fact
                            .subject
                            .iter()
                            .map(|subject| EntityMention {
                                name: subject.clone(),
                                entity_type: memcore_core::EntityType::Person,
                                role: memcore_core::EntityRole::Subject,
                                aliases: Vec::new(),
                            })
                            .collect(),
                        relations: Vec::new(),
                    })
                    .collect(),
                repairs: Vec::new(),
            },
        };
        self.annotation_inputs
            .write()
            .expect("annotation inputs lock poisoned")
            .push(input);
        Ok(output)
    }

    async fn confirm_entity_match(&self, input: EntityMatchInput) -> MemcoreResult<bool> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        let matcher = self
            .entity_matcher
            .read()
            .expect("entity matcher lock poisoned")
            .0
            .clone();
        let same = matcher.is_some_and(|matches| matches(&input));
        self.match_inputs
            .write()
            .expect("match inputs lock poisoned")
            .push(input);
        Ok(same)
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        check_fail(
            &self
                .classification_fail_with
                .read()
                .expect("classification fail lock poisoned"),
        )?;
        let _ = input;

        let mut queue = self
            .classification_decisions
            .write()
            .expect("classification queue lock poisoned");
        if !queue.is_empty() {
            return Ok(queue.remove(0));
        }

        if let Some(decision) = self
            .classification_decision
            .read()
            .expect("classification lock poisoned")
            .clone()
        {
            return Ok(decision);
        }

        Ok(Self::default_classification())
    }

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;

        let prefix = self
            .summary_prefix
            .read()
            .expect("summary lock poisoned")
            .clone();

        if input.facts.is_empty() {
            return Ok(format!("{prefix}no facts to summarize"));
        }

        let summary = input
            .facts
            .iter()
            .map(|fact| fact.content.as_str())
            .collect::<Vec<_>>()
            .join("; ");

        if let Some(max_tokens) = input.max_tokens {
            let max_chars = max_tokens.saturating_mul(4);
            let truncated = if summary.len() > max_chars {
                summary[..max_chars].to_string()
            } else {
                summary
            };
            return Ok(format!("{prefix}{truncated}"));
        }

        Ok(format!("{prefix}{summary}"))
    }
}

#[derive(Debug)]
pub struct MockEmbeddingProvider {
    dimensions: usize,
    fail_with: RwLock<Option<MemcoreError>>,
}

impl MockEmbeddingProvider {
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            fail_with: RwLock::new(None),
        }
    }

    pub fn with_fail_error(self, error: MemcoreError) -> Self {
        *self.fail_with.write().expect("fail lock poisoned") = Some(error);
        self
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        deterministic_embedding(text, self.dimensions)
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        check_fail(&self.fail_with.read().expect("fail lock poisoned"))?;
        texts
            .iter()
            .map(|text| deterministic_embedding(text, self.dimensions))
            .collect()
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embedding_identity(&self) -> Option<memcore_core::ports::EmbeddingIdentity> {
        Some(memcore_core::ports::EmbeddingIdentity::remote(
            "mock",
            "deterministic",
            self.dimensions,
        ))
    }
}

/// Lightweight hash helper for tests asserting embedding differences.
pub fn embedding_signature(text: &str, dimensions: usize) -> u64 {
    let embedding = deterministic_embedding(text, dimensions).expect("embedding should succeed");
    let mut hasher = DefaultHasher::new();
    for value in embedding {
        value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use memcore_common::MemcoreError;
    use memcore_core::{CandidateFact, Fact, MemorySource, MemoryType, TenantContext};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        MockEmbeddingProvider, MockLlmProvider, deterministic_embedding, embedding_signature,
    };
    use crate::inputs::{
        FactClassificationInput, FactExtractionInput, MemoryMessage, MessageRole,
        SummarizationInput,
    };
    use crate::traits::{EmbeddingProvider, LlmProvider};

    fn tenant() -> TenantContext {
        TenantContext::new("org_test", "user_test").expect("tenant should be valid")
    }

    fn sample_fact(content: &str) -> Fact {
        let now = Utc::now();
        Fact::new(
            Uuid::new_v4(),
            "org_test",
            "user_test",
            MemoryType::Profile,
            content,
            None,
            MemorySource::UserMessage,
            0.9,
            0.8,
            None,
            None,
            now,
            now,
            json!({}),
        )
        .expect("fact should be valid")
    }

    #[tokio::test]
    async fn mock_fact_extraction_returns_predictable_candidates() {
        let provider = MockLlmProvider::new();
        let input = FactExtractionInput {
            tenant: tenant(),
            messages: vec![
                MemoryMessage {
                    role: MessageRole::User,
                    content: "I am learning Rust.".to_string(),
                    ..Default::default()
                },
                MemoryMessage {
                    role: MessageRole::Assistant,
                    content: "Great choice.".to_string(),
                    ..Default::default()
                },
            ],
            metadata: json!({}),
            mode: Default::default(),
        };

        let facts = provider
            .extract_facts(input)
            .await
            .expect("extraction should succeed")
            .candidates;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "I am learning Rust.");
        assert_eq!(facts[0].memory_type, MemoryType::Conversation);
    }

    #[tokio::test]
    async fn mock_fact_extraction_supports_configurable_candidates() {
        let custom = CandidateFact::new(
            "Configured fact",
            MemoryType::Skill,
            0.95,
            0.9,
            None,
            json!({}),
        )
        .expect("candidate should be valid");

        let provider = MockLlmProvider::new().with_extraction_candidates(vec![custom.clone()]);
        let input = FactExtractionInput {
            tenant: tenant(),
            messages: vec![],
            metadata: json!({}),
            mode: Default::default(),
        };

        let facts = provider
            .extract_facts(input)
            .await
            .expect("extraction should succeed")
            .candidates;
        assert_eq!(facts, vec![custom]);
    }

    #[tokio::test]
    async fn mock_fact_classification_defaults_to_add() {
        let provider = MockLlmProvider::new();
        let candidate = CandidateFact::new(
            "User prefers Rust.",
            MemoryType::Preference,
            0.8,
            0.7,
            None,
            json!({}),
        )
        .expect("candidate should be valid");

        let input = FactClassificationInput {
            tenant: tenant(),
            candidate_fact: candidate,
            existing_facts: vec![sample_fact("User prefers Python.")],
        };

        let decision = provider
            .classify_fact_operation(input)
            .await
            .expect("classification should succeed");
        assert_eq!(decision.operation, memcore_core::FactOperation::Add);
        assert_eq!(decision.confidence, 0.9);
    }

    #[tokio::test]
    async fn mock_summarization_returns_simple_summary() {
        let provider = MockLlmProvider::new().with_summary_prefix("summary: ");
        let input = SummarizationInput {
            tenant: tenant(),
            facts: vec![sample_fact("Fact one"), sample_fact("Fact two")],
            max_tokens: None,
        };

        let summary = provider
            .summarize_memory(input)
            .await
            .expect("summarization should succeed");
        assert_eq!(summary, "summary: Fact one; Fact two");
    }

    #[tokio::test]
    async fn embedding_dimensions_match_configuration() {
        let provider = MockEmbeddingProvider::new(8);
        assert_eq!(provider.dimensions(), 8);

        let embedding = provider
            .embed_text("hello")
            .await
            .expect("embedding should succeed");
        assert_eq!(embedding.len(), 8);
    }

    #[tokio::test]
    async fn deterministic_embedding_output_is_stable() {
        let first = deterministic_embedding("same text", 16).expect("embedding should succeed");
        let second = deterministic_embedding("same text", 16).expect("embedding should succeed");
        assert_eq!(first, second);

        let different =
            deterministic_embedding("different text", 16).expect("embedding should succeed");
        assert_ne!(first, different);
        assert_ne!(
            embedding_signature("same text", 16),
            embedding_signature("different text", 16)
        );
    }

    #[test]
    fn sqlite_and_semantic_pairs_work_with_mock_dimensions() {
        let dims = 8;
        let sqlite_a = "First sqlite integration memory alpha bravo charlie delta";
        let sqlite_b = "Second distinct sqlite integration memory foxtrot golf hotel india";
        let ea = deterministic_embedding(sqlite_a, dims).expect("a");
        let eb = deterministic_embedding(sqlite_b, dims).expect("b");
        let sqlite_sim: f32 = ea.iter().zip(eb.iter()).map(|(x, y)| x * y).sum();
        assert!(
            sqlite_sim < 0.92,
            "sqlite pair should be below threshold, got {sqlite_sim}"
        );

        let semantic_a =
            "The user prefers working with Rust programming language for backend services.";
        let semantic_b = "The user prefers working with Rust coding language for backend services.";
        let sa = deterministic_embedding(semantic_a, dims).expect("sa");
        let sb = deterministic_embedding(semantic_b, dims).expect("sb");
        let semantic_sim: f32 = sa.iter().zip(sb.iter()).map(|(x, y)| x * y).sum();
        assert!(
            semantic_sim >= 0.92,
            "semantic pair should meet threshold, got {semantic_sim}"
        );
    }

    #[tokio::test]
    async fn batch_embedding_output_count_matches_input() {
        let provider = MockEmbeddingProvider::new(4);
        let embeddings = provider
            .embed_batch(vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ])
            .await
            .expect("batch embedding should succeed");

        assert_eq!(embeddings.len(), 3);
        assert!(embeddings.iter().all(|embedding| embedding.len() == 4));
    }

    #[tokio::test]
    async fn empty_text_returns_validation_error() {
        let provider = MockEmbeddingProvider::new(4);
        let error = provider
            .embed_text("   ")
            .await
            .expect_err("empty text should fail");
        assert_eq!(
            error,
            MemcoreError::ValidationError("embedding text cannot be empty".to_string())
        );
    }

    #[tokio::test]
    async fn provider_error_behavior_when_configured_to_fail() {
        let llm = MockLlmProvider::new().with_fail_error(MemcoreError::ProviderError(
            "mock llm unavailable".to_string(),
        ));
        let embedding = MockEmbeddingProvider::new(4).with_fail_error(MemcoreError::ProviderError(
            "mock embedding unavailable".to_string(),
        ));

        let llm_error = llm
            .extract_facts(FactExtractionInput {
                tenant: tenant(),
                messages: vec![],
                metadata: json!({}),
                mode: Default::default(),
            })
            .await
            .expect_err("llm should fail");
        assert_eq!(
            llm_error,
            MemcoreError::ProviderError("mock llm unavailable".to_string())
        );

        let embedding_error = embedding
            .embed_text("hello")
            .await
            .expect_err("embedding should fail");
        assert_eq!(
            embedding_error,
            MemcoreError::ProviderError("mock embedding unavailable".to_string())
        );
    }
}
