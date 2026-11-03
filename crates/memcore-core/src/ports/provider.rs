use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CandidateFact, Fact, FactOperationDecision, TenantContext};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    #[default]
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryMessage {
    pub role: MessageRole,
    pub content: String,
    /// Display name of the speaker. Lets extraction attribute a fact to a person
    /// rather than to a role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When the message was sent. Relative time expressions are resolved against this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    /// The client's own ID for the message, echoed back in a fact's `source_message_ids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

impl MemoryMessage {
    /// A plain role-and-content message, as every caller built before Part B.
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactExtractionInput {
    pub tenant: TenantContext,
    pub messages: Vec<MemoryMessage>,
    pub metadata: Value,
    /// Which extraction contract to use. `Legacy` sends exactly the pre-Part-B request.
    #[serde(default)]
    pub mode: ExtractionMode,
}

/// The extraction contract a provider is asked to follow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionMode {
    /// The pre-Part-B prompt and response shape. Used whenever
    /// `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` is off.
    #[default]
    Legacy,
    /// Speaker-attributed, time-resolved facts that cite their source messages.
    Structured,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactClassificationInput {
    pub tenant: TenantContext,
    pub candidate_fact: CandidateFact,
    pub existing_facts: Vec<Fact>,
}

/// One new candidate fact and the related stored facts a
/// model decides it against. The field names match the legacy classification input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolidationInput {
    pub tenant: TenantContext,
    pub candidate_fact: CandidateFact,
    pub existing_facts: Vec<Fact>,
}

/// The operation a consolidation v2 decision chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationOperation {
    /// Store the candidate as a new fact.
    Add,
    /// Rewrite the target fact with `merged_content` (enrichment, correction).
    Update,
    /// Mark the target fact as no longer true from `invalid_at` (kept as history) and store the
    /// candidate as the new current fact.
    Invalidate,
    /// The candidate adds nothing new.
    NoOp,
}

/// A model's consolidation decision, before memcore validates it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolidationDecision {
    pub operation: ConsolidationOperation,
    pub target_fact_id: Option<uuid::Uuid>,
    pub merged_content: Option<String>,
    pub invalid_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The model's `invalid_at` exactly as given when it could not be read as a date (it is
    /// rejected and recorded, never replaced by another time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_at_unparsed: Option<String>,
    pub reason: Option<String>,
}

/// One request per extraction window, separate from
/// fact extraction. The model names the entities each stored fact is about and the relations it
/// states. Keys differ from the consolidation payload, so request traces tell the calls apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityAnnotationInput {
    pub tenant: TenantContext,
    /// The window's messages (as sent to fact extraction).
    pub messages: Vec<MemoryMessage>,
    /// The facts stored from this window, in order; the model refers to them by index.
    pub facts: Vec<AnnotationFact>,
    /// Speaker names in the window (authoritative person names).
    pub speakers: Vec<String>,
    /// First request, or the one bounded retry after an unusable output. The retry sends the
    /// same messages and facts; only the instructions ask again for one valid result.
    #[serde(default)]
    pub attempt: AnnotationAttempt,
}

/// Which request of a window's annotation this is (at most one retry).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationAttempt {
    #[default]
    First,
    Retry,
}

/// A deterministic syntax repair applied to an annotation output before it parsed. Only
/// unambiguous cases are repaired; anything else stays unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonRepair {
    /// Material after a complete top-level JSON object was dropped.
    TrailingMaterial,
    /// A comma directly before a closing `}` or `]` was dropped.
    TrailingComma,
    /// Missing closing delimiters were appended after a complete final value.
    MissingClosers,
}

impl JsonRepair {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrailingMaterial => "trailing_material",
            Self::TrailingComma => "trailing_comma",
            Self::MissingClosers => "missing_closers",
        }
    }
}

/// The message prefix of an annotation output that cannot be used (malformed or not matching the
/// schema, after any allowed repair). Kept as a provider error so the router never retries it; the
/// engine's single retry recognises it by this prefix.
pub const UNUSABLE_ANNOTATION_OUTPUT: &str = "invalid entity annotation JSON from model";

/// Builds the error for an unusable annotation output. `class` is `syntax`, `data` or `eof`.
pub fn unusable_annotation_output(class: &str, detail: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::ProviderError(format!("{UNUSABLE_ANNOTATION_OUTPUT} ({class}): {detail}"))
}

/// The class of an unusable annotation output error, or `None` for any other error.
pub fn unusable_annotation_class(error: &MemcoreError) -> Option<&str> {
    match error {
        MemcoreError::ProviderError(message) => message
            .strip_prefix(UNUSABLE_ANNOTATION_OUTPUT)
            .map(|rest| {
                rest.trim_start()
                    .strip_prefix('(')
                    .and_then(|rest| rest.split_once(')'))
                    .map_or("unknown", |(class, _)| class)
            }),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationFact {
    pub index: usize,
    pub content: String,
    pub subject: Option<String>,
}

/// One entity a fact is about, as the model named it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityMention {
    pub name: String,
    pub entity_type: crate::EntityType,
    pub role: crate::EntityRole,
    /// Other names the window uses for the same entity.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A relation a fact states between two named entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationMention {
    pub subject: String,
    pub relation: String,
    pub object: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactAnnotation {
    pub fact_index: usize,
    pub entities: Vec<EntityMention>,
    #[serde(default)]
    pub relations: Vec<RelationMention>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityAnnotationOutput {
    pub facts: Vec<FactAnnotation>,
    /// Syntax repairs applied before the output parsed; empty when it parsed strictly.
    #[serde(default)]
    pub repairs: Vec<JsonRepair>,
}

/// One model call that sends exactly this system and
/// user text with these sampling settings. The raw model text is returned; the caller validates it
/// against the frozen contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryDecompositionInput {
    pub system: String,
    pub user: String,
    pub max_output_tokens: u32,
    pub temperature: f32,
    pub top_p: f32,
}

/// Whether an ambiguous mention names a stored
/// entity. Asked at most once per new entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityMatchInput {
    pub tenant: TenantContext,
    pub mention: String,
    pub mention_type: crate::EntityType,
    /// The fact the mention comes from.
    pub context: String,
    pub candidate_name: String,
    pub candidate_type: crate::EntityType,
    pub candidate_aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummarizationInput {
    pub tenant: TenantContext,
    pub facts: Vec<Fact>,
    pub max_tokens: Option<usize>,
}

/// Why a fact a model produced never became a candidate.
///
/// Extraction used to swallow both of these with a bare `Err(_) => continue`, so a model could
/// return twenty facts, memcore could keep two, and nothing anywhere said so. Every skip now has a
/// reason, a log line and a counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSkipReason {
    /// The `memory_type` label was not in the vocabulary.
    UnknownMemoryType,
    /// The fact failed validation — empty content, or a score outside 0.0..=1.0.
    InvalidCandidate,
}

impl CandidateSkipReason {
    pub const ALL: [Self; 2] = [Self::UnknownMemoryType, Self::InvalidCandidate];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMemoryType => "unknown_memory_type",
            Self::InvalidCandidate => "invalid_candidate",
        }
    }
}

/// Per-reason tally of facts lost during extraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSkipCounts {
    pub unknown_memory_type: usize,
    pub invalid_candidate: usize,
}

impl CandidateSkipCounts {
    pub fn record(&mut self, reason: CandidateSkipReason) {
        match reason {
            CandidateSkipReason::UnknownMemoryType => self.unknown_memory_type += 1,
            CandidateSkipReason::InvalidCandidate => self.invalid_candidate += 1,
        }
    }

    pub fn get(&self, reason: CandidateSkipReason) -> usize {
        match reason {
            CandidateSkipReason::UnknownMemoryType => self.unknown_memory_type,
            CandidateSkipReason::InvalidCandidate => self.invalid_candidate,
        }
    }

    pub fn total(&self) -> usize {
        self.unknown_memory_type + self.invalid_candidate
    }

    pub fn merge(&mut self, other: Self) {
        self.unknown_memory_type += other.unknown_memory_type;
        self.invalid_candidate += other.invalid_candidate;
    }
}

/// What one extraction call produced, including what it could not use.
///
/// `candidates.len() + skipped.total()` is the number of facts the model returned, so a caller can
/// always tell the difference between "the model found little" and "memcore threw most of it away".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FactExtractionOutput {
    pub candidates: Vec<CandidateFact>,
    pub skipped: CandidateSkipCounts,
    /// Facts kept although their `valid_at` could not be read. These are not skips — the fact is
    /// stored without an event time — but a rising count means the accepted date formats have
    /// fallen behind what the model writes.
    pub event_time_unparsed: usize,
    /// Source message IDs a structured fact cited that were not in its input window. They are
    /// dropped from the fact's provenance, never stored.
    #[serde(default)]
    pub invalid_source_message_ids: usize,
}

impl FactExtractionOutput {
    /// An output with no skips, for providers that cannot lose a fact (mocks, canned responses).
    pub fn from_candidates(candidates: Vec<CandidateFact>) -> Self {
        Self {
            candidates,
            skipped: CandidateSkipCounts::default(),
            event_time_unparsed: 0,
            invalid_source_message_ids: 0,
        }
    }

    /// How many facts the model returned in total.
    pub fn returned_by_model(&self) -> usize {
        self.candidates.len() + self.skipped.total()
    }

    /// Folds another window's output into this one, keeping candidates in window order.
    pub fn merge(&mut self, other: Self) {
        self.candidates.extend(other.candidates);
        self.skipped.merge(other.skipped);
        self.event_time_unparsed += other.event_time_unparsed;
        self.invalid_source_message_ids += other.invalid_source_message_ids;
    }
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput>;

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision>;

    async fn summarize_memory(&self, input: SummarizationInput) -> MemcoreResult<String>;

    /// Consolidation v2 decision (`MEMCORE_CONSOLIDATION_MODE=llm_v2`). Providers that do
    /// not implement it fail, so v2 is never silently degraded to a different behaviour.
    async fn decide_consolidation(
        &self,
        _input: ConsolidationInput,
    ) -> MemcoreResult<ConsolidationDecision> {
        Err(memcore_common::MemcoreError::ProviderError(
            "consolidation v2 decisions are not supported by this LLM provider".to_string(),
        ))
    }

    /// Entity annotation (`MEMCORE_ENTITIES_ENABLED`). Providers that do not implement it
    /// fail rather than silently storing no entities.
    async fn annotate_entities(
        &self,
        _input: EntityAnnotationInput,
    ) -> MemcoreResult<EntityAnnotationOutput> {
        Err(memcore_common::MemcoreError::ProviderError(
            "entity annotation is not supported by this LLM provider".to_string(),
        ))
    }

    /// Query decomposition. Providers that do not implement it fail, and the caller
    /// falls back to the original query.
    async fn decompose_query(&self, _input: QueryDecompositionInput) -> MemcoreResult<String> {
        Err(memcore_common::MemcoreError::ProviderError(
            "query decomposition is not supported by this LLM provider".to_string(),
        ))
    }

    /// Entity resolution confirmation for an ambiguous match.
    async fn confirm_entity_match(&self, _input: EntityMatchInput) -> MemcoreResult<bool> {
        Err(memcore_common::MemcoreError::ProviderError(
            "entity match confirmation is not supported by this LLM provider".to_string(),
        ))
    }
}

/// What produces a provider's vectors: the semantic identity. Two providers with
/// equal identities produce interchangeable vectors; anything that can change a vector belongs
/// here, while runtime provenance that cannot (e.g. an inference-runtime patch version) does not.
/// `None` means not applicable to, or not known for, this provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingIdentity {
    pub provider: String,
    pub model_id: String,
    pub revision: Option<String>,
    pub dimensions: usize,
    pub pooling: Option<String>,
    pub normalisation: Option<String>,
    pub query_instruction_sha256: Option<String>,
    pub document_prefix: Option<String>,
    pub max_sequence_length: Option<usize>,
    pub truncation: Option<String>,
    pub tokenizer_sha256: Option<String>,
    pub model_artifact_sha256: Option<String>,
    /// Numeric precision of the computation (e.g. `f32`), when it is part of the provider's
    /// contract: a different precision can change the vectors.
    pub precision: Option<String>,
    /// How the model is served when that can change vectors (quantisation, runtime build,
    /// endpoint flavour), as declared by the operator. Absent from the hash when
    /// unset, so existing identities keep their hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving_variant: Option<String>,
    /// The embedding-dedup policy of this store (D-L3: dedup policy is part of identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_policy: Option<String>,
}

impl EmbeddingIdentity {
    /// A remote model known only by provider, model id and dimensions.
    pub fn remote(provider: &str, model_id: &str, dimensions: usize) -> Self {
        Self {
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            revision: None,
            dimensions,
            pooling: None,
            normalisation: None,
            query_instruction_sha256: None,
            document_prefix: None,
            max_sequence_length: None,
            truncation: None,
            tokenizer_sha256: None,
            model_artifact_sha256: None,
            precision: None,
            serving_variant: None,
            dedup_policy: None,
        }
    }

    /// Whether vectors come from a model run by memcore itself (`local_*` providers). Local stores
    /// must record their identity.
    pub fn is_local(&self) -> bool {
        self.provider.starts_with("local_")
    }

    /// SHA-256 of the canonical (key-sorted, compact) JSON form.
    pub fn semantic_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::to_value(self)
            .expect("an embedding identity always serialises")
            .to_string();
        hex::encode(Sha256::digest(canonical))
    }
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>>;

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>>;

    fn dimensions(&self) -> usize;

    /// Embeds a search query. Providers whose model encodes queries differently from
    /// documents (an instruction, a prefix) override this; the default is the document path, so
    /// every other provider keeps its exact behaviour.
    async fn embed_query(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        self.embed_text(text).await
    }

    /// Provider-specific runtime diagnostics (e.g. a local model's queue), if any.
    fn diagnostics(&self) -> Option<serde_json::Value> {
        None
    }

    /// The semantic identity of this provider's vectors. The default, `None`, means unknown:
    /// nothing keyed on identity (such as the query-embedding cache) is used with this provider.
    fn embedding_identity(&self) -> Option<EmbeddingIdentity> {
        None
    }
}
