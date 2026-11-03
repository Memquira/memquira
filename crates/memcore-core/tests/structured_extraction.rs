//! The `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` flag, windowing,
//! bounded concurrency, and how subject, provenance and event time reach the stored fact.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use memcore_common::MemcoreResult;
use memcore_core::ports::{FactSearchQuery, SummarizationInput};
use memcore_core::{
    AddMemoryInput, CandidateFact, ExtractionMode, FactClassificationInput, FactExtractionInput,
    FactExtractionOutput, FactOperation, FactOperationDecision, FactStore, FactStructure,
    LlmProvider, MemoryEngine, MemoryMessage, MemorySession, MemoryType, MessageRole,
    StructuredExtractionConfig, TemporalGranularity, TemporalValue, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockVectorStore};
use serde_json::json;

fn tenant() -> TenantContext {
    TenantContext::new("org_b", "user_b").expect("tenant")
}

fn time(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2023, 5, day, hour, 0, 0).unwrap()
}

fn message(id: &str, name: &str, text: &str, at: Option<DateTime<Utc>>) -> MemoryMessage {
    MemoryMessage {
        role: MessageRole::User,
        content: text.to_string(),
        name: Some(name.to_string()),
        timestamp: at,
        message_id: Some(id.to_string()),
    }
}

fn engine(
    llm: Arc<dyn LlmProvider>,
    facts: Arc<MockFactStore>,
    structured: Option<StructuredExtractionConfig>,
) -> MemoryEngine {
    MemoryEngine::new(
        facts,
        Arc::new(MockVectorStore::new()),
        llm,
        Arc::new(MockEmbeddingProvider::new(8)),
    )
    .with_structured_extraction(structured)
}

async fn stored(facts: &MockFactStore) -> Vec<memcore_core::Fact> {
    facts
        .search_facts(FactSearchQuery::new(tenant(), 100))
        .await
        .expect("search")
}

/// Records every extraction request and delegates to an inner provider (or returns canned facts).
struct RecordingProvider {
    inner: MockLlmProvider,
    canned: Option<Vec<CandidateFact>>,
    calls: Mutex<Vec<(ExtractionMode, usize)>>,
    delay: Duration,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl RecordingProvider {
    fn new() -> Self {
        Self {
            inner: MockLlmProvider::new(),
            canned: None,
            calls: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    async fn extract_facts(
        &self,
        input: FactExtractionInput,
    ) -> MemcoreResult<FactExtractionOutput> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.calls
            .lock()
            .unwrap()
            .push((input.mode, input.messages.len()));
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let result = match &self.canned {
            Some(candidates) => Ok(FactExtractionOutput::from_candidates(candidates.clone())),
            None => self.inner.extract_facts(input).await,
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }

    async fn classify_fact_operation(
        &self,
        _input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        Ok(FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: None,
            confidence: 0.9,
        })
    }

    async fn summarize_memory(&self, _input: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

fn input(messages: Vec<MemoryMessage>, session: MemorySession) -> AddMemoryInput {
    AddMemoryInput {
        tenant: tenant(),
        messages,
        metadata: json!({}),
        session,
    }
}

fn session() -> MemorySession {
    MemorySession {
        id: Some("session_4".to_string()),
        timestamp: Some(time(1, 9)),
    }
}

#[tokio::test]
async fn flag_off_uses_the_legacy_request_and_stores_no_structure() {
    let provider = Arc::new(RecordingProvider::new());
    let facts = Arc::new(MockFactStore::new());
    let engine = engine(provider.clone(), facts.clone(), None);

    engine
        .add_memory(input(
            vec![message(
                "D1:1",
                "Priya",
                "I adopted a dog named Biscuit.",
                Some(time(8, 10)),
            )],
            session(),
        ))
        .await
        .expect("add");

    assert_eq!(
        *provider.calls.lock().unwrap(),
        vec![(ExtractionMode::Legacy, 1)],
        "flag off: one legacy call, exactly as before Part B"
    );
    let stored = stored(&facts).await;
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].structure.is_empty(),
        "flag off: no structured fields are written even when the request carries them"
    );
}

#[tokio::test]
async fn flag_on_stores_subject_provenance_session_and_observation_time() {
    let provider = Arc::new(RecordingProvider::new());
    let facts = Arc::new(MockFactStore::new());
    let engine = engine(
        provider.clone(),
        facts.clone(),
        Some(StructuredExtractionConfig::default()),
    );

    engine
        .add_memory(input(
            vec![message(
                "D1:1",
                "Priya",
                "I adopted a dog named Biscuit.",
                Some(time(8, 10)),
            )],
            session(),
        ))
        .await
        .expect("add");

    assert_eq!(
        *provider.calls.lock().unwrap(),
        vec![(ExtractionMode::Structured, 1)]
    );
    let stored = stored(&facts).await;
    let structure = &stored[0].structure;
    assert_eq!(structure.subject.as_deref(), Some("Priya"));
    assert_eq!(structure.source_message_ids, vec!["D1:1"]);
    assert_eq!(structure.session_id.as_deref(), Some("session_4"));
    assert_eq!(
        structure.observed_at,
        Some(time(8, 10)),
        "the message's own time wins"
    );
}

#[tokio::test]
async fn messages_without_times_are_observed_at_the_session_time() {
    let provider = Arc::new(RecordingProvider::new());
    let facts = Arc::new(MockFactStore::new());
    let engine = engine(
        provider,
        facts.clone(),
        Some(StructuredExtractionConfig::default()),
    );

    engine
        .add_memory(input(
            vec![message("D1:1", "Priya", "I like hiking.", None)],
            session(),
        ))
        .await
        .expect("add");

    assert_eq!(
        stored(&facts).await[0].structure.observed_at,
        Some(time(1, 9))
    );
}

#[tokio::test]
async fn a_wrong_model_date_is_overridden_by_the_resolver_and_becomes_valid_at() {
    // The model resolved "yesterday" against the wrong day.
    let wrong = TemporalValue::new(
        Utc.with_ymd_and_hms(2023, 5, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2023, 5, 1, 23, 59, 59).unwrap(),
        TemporalGranularity::Day,
        time(8, 10),
    )
    .unwrap();
    let candidate = CandidateFact::new(
        "Priya adopted a dog.",
        MemoryType::Event,
        0.9,
        0.9,
        None,
        json!({}),
    )
    .unwrap()
    .with_structure(FactStructure {
        subject: Some("Priya".to_string()),
        event_time: Some(wrong),
        event_time_text: Some("yesterday".to_string()),
        observed_at: Some(time(8, 10)),
        source_message_ids: vec!["D1:1".to_string()],
        ..Default::default()
    });
    let mut provider = RecordingProvider::new();
    provider.canned = Some(vec![candidate]);
    let facts = Arc::new(MockFactStore::new());
    let engine = engine(
        Arc::new(provider),
        facts.clone(),
        Some(StructuredExtractionConfig::default()),
    );

    let output = engine
        .add_memory(input(
            vec![message(
                "D1:1",
                "Priya",
                "I adopted a dog yesterday.",
                Some(time(8, 10)),
            )],
            session(),
        ))
        .await
        .expect("add");

    assert_eq!(output.temporal.deterministic_override, 1);
    let fact = &stored(&facts).await[0];
    let event_time = fact.structure.event_time.as_ref().expect("event time");
    assert_eq!(event_time.start.date_naive(), time(7, 0).date_naive());
    assert_eq!(event_time.anchor, time(8, 10));
    assert_eq!(
        fact.content, "On 7 May 2023, Priya adopted a dog.",
        "the stored sentence states the settled date, not the model's"
    );
    assert_eq!(
        fact.valid_at,
        Some(event_time.start),
        "valid_at follows the settled event time"
    );
}

#[tokio::test]
async fn long_requests_are_windowed_with_overlap() {
    let provider = Arc::new(RecordingProvider::new());
    let facts = Arc::new(MockFactStore::new());
    let config = StructuredExtractionConfig {
        window_turns: 20,
        window_overlap: 2,
        concurrency: 4,
    };
    let engine = engine(provider.clone(), facts, Some(config));
    let messages = (0..45)
        .map(|i| {
            message(
                &format!("D1:{i}"),
                "Priya",
                &format!("Fact number {i} about hiking."),
                Some(time(8, 10)),
            )
        })
        .collect();

    engine
        .add_memory(input(messages, session()))
        .await
        .expect("add");

    let sizes: Vec<usize> = provider
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(_, n)| *n)
        .collect();
    assert_eq!(sizes, vec![20, 20, 9]);
}

#[tokio::test]
async fn window_concurrency_never_exceeds_the_configured_bound() {
    let mut provider = RecordingProvider::new();
    provider.delay = Duration::from_millis(30);
    let provider = Arc::new(provider);
    let facts = Arc::new(MockFactStore::new());
    let config = StructuredExtractionConfig {
        window_turns: 2,
        window_overlap: 0,
        concurrency: 2,
    };
    let engine = engine(provider.clone(), facts, Some(config));
    let messages = (0..10)
        .map(|i| {
            message(
                &format!("D1:{i}"),
                "Priya",
                &format!("Distinct fact {i}."),
                Some(time(8, 10)),
            )
        })
        .collect();

    engine
        .add_memory(input(messages, session()))
        .await
        .expect("add");

    assert_eq!(provider.calls.lock().unwrap().len(), 5);
    let max = provider.max_in_flight.load(Ordering::SeqCst);
    assert!(max <= 2, "at most 2 windows in flight, saw {max}");
    assert_eq!(
        max, 2,
        "windows should actually run concurrently up to the bound"
    );
}

/// With the flag on, the request's metadata reaches the stored fact exactly as with it off (where
/// the legacy prompt has the model copy it). Clients read it back from search results.
#[tokio::test]
async fn flag_on_keeps_the_request_metadata_on_every_fact() {
    let provider = Arc::new(RecordingProvider::new());
    let facts = Arc::new(MockFactStore::new());
    let engine = engine(
        provider.clone(),
        facts.clone(),
        Some(StructuredExtractionConfig::default()),
    );
    let metadata = json!({ "timestamp": "2023-05-08T13:56:00Z", "session_id": "session_1" });

    engine
        .add_memory(AddMemoryInput {
            metadata: metadata.clone(),
            ..input(
                vec![message(
                    "D1:1",
                    "Priya",
                    "I adopted a dog named Biscuit.",
                    Some(time(8, 10)),
                )],
                session(),
            )
        })
        .await
        .expect("add");

    let stored = stored(&facts).await;
    assert!(!stored.is_empty());
    for fact in stored {
        assert_eq!(fact.metadata, metadata);
    }
}
