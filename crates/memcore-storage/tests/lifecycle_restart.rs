//! Restart / recovery on the real SQLite fact store. Invented data; no model
//! calls. The vector store is the in-memory mock, kept across the restart to stand in for a durable
//! Qdrant (only the SQL side is restarted here).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EmbeddingProvider, FactClassificationInput, FactExtractionInput, LlmProvider,
    SummarizationInput,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, MemoryEngine, MemoryMessage,
    MemoryType, MessageRole, TenantContext,
};
use memcore_storage::{MockVectorStore, SqliteFactStore};
use serde_json::json;

static FAIL_EMBED: Mutex<Option<String>> = Mutex::new(None);

struct Embed;

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let mut fail = FAIL_EMBED.lock().unwrap();
        if fail.as_deref() == Some(text) {
            *fail = None;
            return Err(MemcoreError::ProviderError(
                "connection reset (injected)".to_string(),
            ));
        }
        let mut v = [0.0_f32; 8];
        for (i, b) in text.bytes().enumerate() {
            v[i % 8] += f32::from(b);
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        Ok(v.iter().map(|x| x / n).collect())
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        let mut out = Vec::new();
        for t in texts {
            out.push(self.embed_text(&t).await?);
        }
        Ok(out)
    }
    fn dimensions(&self) -> usize {
        8
    }
}

struct Llm {
    queue: Mutex<Vec<Vec<&'static str>>>,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let next = self.queue.lock().unwrap().remove(0);
        Ok(FactExtractionOutput::from_candidates(
            next.into_iter()
                .map(|t| {
                    CandidateFact::new(t, MemoryType::Profile, 0.9, 0.9, None, json!({})).unwrap()
                })
                .collect(),
        ))
    }
    async fn classify_fact_operation(
        &self,
        _: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        Ok(FactOperationDecision {
            operation: FactOperation::Add,
            target_fact_id: None,
            reason: None,
            confidence: 0.9,
        })
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

fn tenant() -> TenantContext {
    TenantContext::new("org_v2_restart", "user_r").unwrap()
}

async fn open(
    url: &str,
    vectors: Arc<MockVectorStore>,
    llm: Arc<Llm>,
) -> (Arc<SqliteFactStore>, MemoryEngine) {
    let store = Arc::new(SqliteFactStore::connect(url).await.expect("open sqlite"));
    let engine = MemoryEngine::new(store.clone(), vectors, llm, Arc::new(Embed))
        .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
            enabled: false,
            ..Default::default()
        });
    (store, engine)
}

async fn add(engine: &MemoryEngine, llm: &Llm, texts: Vec<&'static str>) -> MemcoreResult<()> {
    llm.queue.lock().unwrap().push(texts);
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "note".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
        .map(|_| ())
}

async fn active(store: &SqliteFactStore) -> Vec<String> {
    let mut v: Vec<String> = store
        .search_facts(FactSearchQuery {
            tenant: tenant(),
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
    v.sort();
    v
}

#[tokio::test]
async fn memory_state_survives_a_restart_and_compensation_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path()
            .join("memcore.db")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let vectors = Arc::new(MockVectorStore::new());
    let llm = Arc::new(Llm {
        queue: Mutex::new(Vec::new()),
    });

    // before the restart: two facts, then a failed write whose first add is compensated
    let (store, engine) = open(&url, vectors.clone(), llm.clone()).await;
    add(
        &engine,
        &llm,
        vec!["Maya lives in Paris.", "Maya works at Acme Labs."],
    )
    .await
    .unwrap();
    *FAIL_EMBED.lock().unwrap() = Some("Maya repairs old canoes.".to_string());
    assert!(
        add(
            &engine,
            &llm,
            vec!["Maya keeps a pottery wheel.", "Maya repairs old canoes."]
        )
        .await
        .is_err()
    );
    let before = active(&store).await;
    assert_eq!(
        before,
        vec![
            "Maya lives in Paris.".to_string(),
            "Maya works at Acme Labs.".to_string()
        ]
    );
    drop(engine);
    drop(store);

    // restart: a new pool on the same file
    let (store, engine) = open(&url, vectors.clone(), llm.clone()).await;
    assert_eq!(
        active(&store).await,
        before,
        "the active state is unchanged by the restart"
    );
    // the exact-dedup key survives: a replayed fact is not stored twice
    add(&engine, &llm, vec!["Maya lives in Paris."])
        .await
        .unwrap();
    assert_eq!(active(&store).await, before);
    // the compensated fact's key was released: the retry stores it once
    add(
        &engine,
        &llm,
        vec!["Maya keeps a pottery wheel.", "Maya repairs old canoes."],
    )
    .await
    .unwrap();
    let after = active(&store).await;
    assert_eq!(after.len(), 4);
    assert_eq!(after.iter().filter(|t| t.contains("pottery")).count(), 1);
}
