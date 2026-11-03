use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    AddMemoryInput, BuildContextInput, CandidateFact, ContextBudget, ContextCompressionOptions,
    ContextFormatOptions, EmbeddingProvider, FactExtractionOutput, FactOperation,
    FactOperationDecision, FactStore, LlmProvider, MemoryEngine, MemoryMessage, MemoryType,
    MessageRole, SearchMemoryInput, TenantContext,
};
use memcore_providers::{
    FactClassificationInput, FactExtractionInput, MockEmbeddingProvider, MockLlmProvider,
    ProviderExecutionPolicy, deterministic_embedding, wrap_embedding_provider, wrap_llm_provider,
};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;
use uuid::Uuid;

fn tenant() -> TenantContext {
    TenantContext::new("org_policy", "user_policy").expect("tenant")
}

fn test_policy() -> ProviderExecutionPolicy {
    ProviderExecutionPolicy::for_tests()
}

fn placeholder_messages() -> Vec<MemoryMessage> {
    vec![MemoryMessage {
        role: MessageRole::User,
        content: "User enjoys hiking on weekends.".to_string(),
        ..Default::default()
    }]
}

struct FlakyEmbeddingProvider {
    inner: MockEmbeddingProvider,
    calls: Arc<AtomicUsize>,
    fail_until: usize,
}

impl FlakyEmbeddingProvider {
    fn new(dimensions: usize, fail_until: usize) -> Self {
        Self {
            inner: MockEmbeddingProvider::new(dimensions),
            calls: Arc::new(AtomicUsize::new(0)),
            fail_until,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EmbeddingProvider for FlakyEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_until {
            return Err(MemcoreError::ProviderError(
                "OpenAI API error (503): unavailable".to_string(),
            ));
        }
        self.inner.embed_text(text).await
    }

    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_until {
            return Err(MemcoreError::ProviderError(
                "OpenAI API error (503): unavailable".to_string(),
            ));
        }
        self.inner.embed_batch(texts).await
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

struct SlowThenFastEmbeddingProvider {
    calls: Arc<AtomicUsize>,
    dimensions: usize,
}

impl SlowThenFastEmbeddingProvider {
    fn new(dimensions: usize) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            dimensions,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EmbeddingProvider for SlowThenFastEmbeddingProvider {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == 1 {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
        deterministic_embedding(text, self.dimensions)
    }

    async fn embed_batch(&self, _texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Err(MemcoreError::Internal("not used".to_string()))
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

struct FlakyLlmProvider {
    inner: MockLlmProvider,
    extraction_calls: Arc<AtomicUsize>,
    fail_extraction_until: usize,
}

impl FlakyLlmProvider {
    fn new(fail_extraction_until: usize) -> Self {
        Self {
            inner: MockLlmProvider::new(),
            extraction_calls: Arc::new(AtomicUsize::new(0)),
            fail_extraction_until,
        }
    }

    fn extraction_call_count(&self) -> usize {
        self.extraction_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for FlakyLlmProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        let attempt = self.extraction_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_extraction_until {
            return Err(MemcoreError::ProviderError(
                "OpenAI API error (500): internal".to_string(),
            ));
        }
        self.inner.extract_facts(input).await
    }

    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        self.inner.classify_fact_operation(input).await
    }

    async fn summarize_memory(
        &self,
        input: memcore_providers::SummarizationInput,
    ) -> MemcoreResult<String> {
        self.inner.summarize_memory(input).await
    }
}

struct NonRetryableLlmProvider;

#[async_trait]
impl LlmProvider for NonRetryableLlmProvider {
    async fn extract_facts(
        &self,
        _input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        Err(MemcoreError::ValidationError(
            "invalid extraction payload".to_string(),
        ))
    }

    async fn classify_fact_operation(
        &self,
        _input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        Err(MemcoreError::ValidationError("invalid".to_string()))
    }

    async fn summarize_memory(
        &self,
        _input: memcore_providers::SummarizationInput,
    ) -> MemcoreResult<String> {
        Err(MemcoreError::ValidationError("invalid".to_string()))
    }
}

fn engine_with_providers(
    llm: Arc<dyn LlmProvider>,
    embedding: Arc<dyn EmbeddingProvider>,
) -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        llm,
        embedding,
    )
}

#[tokio::test]
async fn retryable_embedding_failure_succeeds_on_retry_for_search() {
    let flaky = Arc::new(FlakyEmbeddingProvider::new(4, 1));
    let flaky_for_count = flaky.clone();
    let embedding = wrap_embedding_provider(flaky, test_policy()).expect("wrap");
    let engine = engine_with_providers(Arc::new(MockLlmProvider::new()), embedding);

    let output = engine
        .search_memory(SearchMemoryInput {
            tenant: tenant(),
            query: "hiking".to_string(),
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search should succeed after retry");

    assert!(output.results.is_empty());
    assert_eq!(flaky_for_count.call_count(), 2);
}

#[tokio::test]
async fn retryable_llm_failure_succeeds_on_retry_for_add_memory() {
    let flaky = Arc::new(FlakyLlmProvider::new(1));
    let flaky_for_count = flaky.clone();
    let llm = wrap_llm_provider(flaky, test_policy());
    let engine = engine_with_providers(llm, Arc::new(MockEmbeddingProvider::new(4)));

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: placeholder_messages(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("add should succeed after retry");

    assert_eq!(output.added, 1);
    assert_eq!(flaky_for_count.extraction_call_count(), 2);
}

#[tokio::test]
async fn non_retryable_provider_error_is_not_retried_for_add_memory() {
    let llm = wrap_llm_provider(Arc::new(NonRetryableLlmProvider), test_policy());
    let engine = engine_with_providers(llm, Arc::new(MockEmbeddingProvider::new(4)));

    let error = engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: placeholder_messages(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect_err("should fail");

    assert!(matches!(error, MemcoreError::ValidationError(_)));
}

#[tokio::test]
async fn embedding_provider_timeout_is_retried_when_policy_allows() {
    let slow = Arc::new(SlowThenFastEmbeddingProvider::new(4));
    let slow_for_count = slow.clone();
    let policy = ProviderExecutionPolicy {
        timeout: std::time::Duration::from_millis(50),
        max_retries: 1,
        initial_backoff: std::time::Duration::from_millis(1),
        max_backoff: std::time::Duration::from_millis(1),
        jitter_enabled: false,
        backoff_multiplier: 2.0,
    };
    let embedding = wrap_embedding_provider(slow, policy).expect("wrap");
    let engine = engine_with_providers(Arc::new(MockLlmProvider::new()), embedding);

    let _ = engine
        .search_memory(SearchMemoryInput {
            tenant: tenant(),
            query: "timeout retry".to_string(),
            limit: 5,
            memory_types: None,
            metadata_filter: None,
        })
        .await
        .expect("search should succeed on retry after timeout");

    assert_eq!(slow_for_count.call_count(), 2);
}

#[tokio::test]
async fn context_build_uses_embedding_policy_for_search_path() {
    let now = Utc::now();
    let fact_store = Arc::new(MockFactStore::new());
    fact_store
        .insert_fact(
            &tenant(),
            memcore_core::Fact::new(
                Uuid::new_v4(),
                "org_policy",
                "user_policy",
                MemoryType::Preference,
                "User likes trail running.",
                None,
                memcore_core::MemorySource::UserMessage,
                0.9,
                0.8,
                None,
                None,
                now,
                now,
                json!({}),
            )
            .expect("fact"),
        )
        .await
        .expect("insert");

    let flaky = Arc::new(FlakyEmbeddingProvider::new(4, 1));
    let flaky_for_count = flaky.clone();
    let embedding = wrap_embedding_provider(flaky, test_policy()).expect("wrap");
    let engine = MemoryEngine::new(
        fact_store,
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        embedding,
    );

    let _ = engine
        .build_context(BuildContextInput {
            tenant: tenant(),
            query: "running".to_string(),
            max_memories: 5,
            memory_types: None,
            include_metadata: false,
            include_history: false,
            budget: ContextBudget::default(),
            format_options: ContextFormatOptions::default(),
            compression_options: ContextCompressionOptions::default(),
        })
        .await
        .expect("context build should succeed");

    assert_eq!(flaky_for_count.call_count(), 2);
}

#[tokio::test]
async fn lifecycle_classification_uses_embedding_policy_on_update() {
    let fact_store = Arc::new(MockFactStore::new());
    let vector_store = Arc::new(MockVectorStore::new());
    let now = Utc::now();
    let existing = memcore_core::Fact::new(
        Uuid::new_v4(),
        "org_policy",
        "user_policy",
        MemoryType::Preference,
        "User likes tea.",
        None,
        memcore_core::MemorySource::UserMessage,
        0.9,
        0.8,
        None,
        None,
        now,
        now,
        json!({}),
    )
    .expect("fact");
    fact_store
        .insert_fact(&tenant(), existing.clone())
        .await
        .expect("insert");

    let candidate = CandidateFact::new(
        "User prefers coffee.",
        MemoryType::Preference,
        0.9,
        0.8,
        None,
        json!({}),
    )
    .expect("candidate");

    let llm = MockLlmProvider::new()
        .with_extraction_candidates(vec![candidate])
        .with_classification_decision(FactOperationDecision {
            operation: FactOperation::Update,
            target_fact_id: Some(existing.id),
            reason: Some("update preference".to_string()),
            confidence: 0.9,
        });

    let flaky = Arc::new(FlakyEmbeddingProvider::new(4, 1));
    let flaky_for_count = flaky.clone();
    let embedding = wrap_embedding_provider(flaky, test_policy()).expect("wrap");
    let engine = MemoryEngine::new(fact_store, vector_store, Arc::new(llm), embedding);

    let output = engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: placeholder_messages(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .expect("update path should succeed");

    assert_eq!(output.updated, 1);
    assert!(flaky_for_count.call_count() >= 2);
}
