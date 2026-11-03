//! The ingest journal (write-ahead undo journal) under deterministic fault
//! injection. Invented data; no model calls. Every test checks the final stores, not only the
//! returned result.
//!
//! Contract under test (`memcore_core::ports::ingest_journal`):
//! - a request succeeds only after all of its fact operations completed; it leaves no journal row;
//! - an error, a cancellation (the request future dropped) or a crash rolls back every fact
//!   operation the request applied (adds removed, in-place updates restored, supersessions undone
//!   as a unit; a delete is rolled forward), by the request, by its cancellation task or by the
//!   recovery sweep; every undo is idempotent and resumable;
//! - a request never acknowledges another request's uncommitted fact: it waits for that request
//!   to resolve (or rolls an abandoned one back) and processes the candidate again;
//! - rollback and lookups stay inside the tenant.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    DedupInsert, EmbeddingProvider, FactClassificationInput, FactExtractionInput,
    FactFulltextQuery, IngestJournalStep, IngestJournalStore, IngestOperation,
    IngestOperationState, IngestReceipt, LlmProvider, MemoryEventQuery, OrgUserListQuery,
    OrgUserSummary, RetentionPruneResult, ScoredFact, StoredFactState, SummarizationInput,
    VectorPointState, VectorStore,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingDeduplicationConfig, Fact, FactExtractionOutput,
    FactOperation, FactOperationDecision, FactSearchQuery, FactStore, ForgetUserInput,
    IngestJournalComponents, MemoryEngine, MemoryEventStore, MemoryMessage, MemoryType,
    MessageRole, RelatedFactMatcher, TenantContext, VectorRecord, VectorSearchQuery,
    VectorSearchResult,
};
use memcore_storage::{
    MockFactStore, MockIngestJournalStore, MockMemoryEventStore, MockVectorStore,
};
use serde_json::json;
use tokio::sync::Notify;
use uuid::Uuid;

// ------------------------------------------------------------------ providers with fault hooks

/// Bag-of-words embedding (16 hashed buckets): paraphrases that share most words are similar.
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

#[derive(Default)]
struct Embed {
    fail_on: Mutex<Option<String>>,
}

#[async_trait]
impl EmbeddingProvider for Embed {
    async fn embed_text(&self, text: &str) -> MemcoreResult<Vec<f32>> {
        let mut fail = self.fail_on.lock().unwrap();
        if fail.as_deref() == Some(text) {
            *fail = None;
            return Err(MemcoreError::ProviderError(
                "embedding connection reset (injected)".to_string(),
            ));
        }
        Ok(vector(text))
    }
    async fn embed_batch(&self, texts: Vec<String>) -> MemcoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        16
    }
}

#[derive(Default)]
struct Llm {
    queue: Mutex<Vec<Vec<CandidateFact>>>,
    /// candidate text -> UPDATE of this fact id (the scripted classifier's decision).
    update_target: Mutex<HashMap<String, Uuid>>,
    /// With related facts offered, update the first one (the semantic matcher's offered fact).
    update_related: AtomicBool,
    /// The classification of this text waits for `release`.
    hold_on: Mutex<Option<String>>,
    reached: Notify,
    release: Notify,
    /// Delay of every classification (concurrency tests).
    delay_ms: AtomicUsize,
}

#[async_trait]
impl LlmProvider for Llm {
    async fn extract_facts(&self, _: FactExtractionInput) -> MemcoreResult<FactExtractionOutput> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            return Err(MemcoreError::ProviderError(
                "extraction unavailable (injected)".to_string(),
            ));
        }
        Ok(FactExtractionOutput::from_candidates(q.remove(0)))
    }
    async fn classify_fact_operation(
        &self,
        input: FactClassificationInput,
    ) -> MemcoreResult<FactOperationDecision> {
        let text = input.candidate_fact.content.clone();
        let hold = self.hold_on.lock().unwrap().as_deref() == Some(text.as_str());
        if hold {
            self.reached.notify_one();
            self.release.notified().await;
        }
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        }
        let target = self
            .update_target
            .lock()
            .unwrap()
            .get(&text)
            .copied()
            .or_else(|| {
                (self.update_related.load(Ordering::SeqCst) && !input.existing_facts.is_empty())
                    .then(|| input.existing_facts[0].id)
            });
        Ok(match target {
            Some(id) => FactOperationDecision {
                operation: FactOperation::Update,
                target_fact_id: Some(id),
                reason: Some("scripted update".to_string()),
                confidence: 0.9,
            },
            None => FactOperationDecision {
                operation: FactOperation::Add,
                target_fact_id: None,
                reason: None,
                confidence: 0.9,
            },
        })
    }
    async fn summarize_memory(&self, _: SummarizationInput) -> MemcoreResult<String> {
        Ok(String::new())
    }
}

/// MockVectorStore with an upsert failure for one text, and a delete that can hang or fail.
#[derive(Default)]
struct Vectors {
    inner: MockVectorStore,
    fail_upsert_text: Mutex<Option<String>>,
    hold_delete: AtomicBool,
    delete_reached: Notify,
    delete_release: Notify,
    fail_deletes: AtomicBool,
    /// (org, user, fact id) of every point the store holds.
    points: Mutex<HashSet<(String, String, Uuid)>>,
}

impl Vectors {
    /// Fact ids of the tenant's points.
    fn point_ids(&self, tenant: &TenantContext) -> HashSet<Uuid> {
        self.points
            .lock()
            .unwrap()
            .iter()
            .filter(|(org, user, _)| *org == tenant.org_id && *user == tenant.user_id)
            .map(|(_, _, id)| *id)
            .collect()
    }
}

#[async_trait]
impl VectorStore for Vectors {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        {
            let mut fail = self.fail_upsert_text.lock().unwrap();
            if fail.as_deref() == Some(record.content.as_str()) {
                *fail = None;
                return Err(MemcoreError::StorageError(
                    "vector store unavailable (injected)".to_string(),
                ));
            }
        }
        let id = record.fact_id;
        self.inner.upsert_vector(tenant, record).await?;
        self.points
            .lock()
            .unwrap()
            .insert((tenant.org_id.clone(), tenant.user_id.clone(), id));
        Ok(())
    }
    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.inner.search_vectors(query).await
    }
    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        if self.hold_delete.load(Ordering::SeqCst) {
            self.delete_reached.notify_one();
            self.delete_release.notified().await;
        }
        if self.fail_deletes.load(Ordering::SeqCst) {
            return Err(MemcoreError::StorageError(
                "vector delete failed (injected)".to_string(),
            ));
        }
        self.inner.delete_by_fact_id(tenant, fact_id).await?;
        self.points.lock().unwrap().remove(&(
            tenant.org_id.clone(),
            tenant.user_id.clone(),
            fact_id,
        ));
        Ok(())
    }
    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_by_user(tenant).await?;
        self.points
            .lock()
            .unwrap()
            .retain(|(org, user, _)| *org != tenant.org_id || *user != tenant.user_id);
        Ok(())
    }
    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.inner.fact_vector_points(tenant, fact_id).await
    }
}

/// MockFactStore whose `update_fact` fails once after `update_ok_before_fail` successful calls.
#[derive(Default)]
struct Facts {
    inner: MockFactStore,
    fail_update_in: Mutex<Option<usize>>,
}

#[async_trait]
impl FactStore for Facts {
    async fn insert_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        self.inner.insert_fact(tenant, fact).await
    }
    async fn insert_fact_deduplicated(
        &self,
        tenant: &TenantContext,
        fact: Fact,
        dedup_key: &str,
    ) -> MemcoreResult<DedupInsert> {
        self.inner
            .insert_fact_deduplicated(tenant, fact, dedup_key)
            .await
    }
    async fn update_fact(&self, tenant: &TenantContext, fact: Fact) -> MemcoreResult<Fact> {
        {
            let mut countdown = self.fail_update_in.lock().unwrap();
            if let Some(n) = countdown.as_mut() {
                if *n == 0 {
                    *countdown = None;
                    return Err(MemcoreError::StorageError(
                        "database connection lost (injected)".to_string(),
                    ));
                }
                *n -= 1;
            }
        }
        self.inner.update_fact(tenant, fact).await
    }
    async fn get_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<Option<Fact>> {
        self.inner.get_fact(tenant, fact_id).await
    }
    async fn get_fact_including_deleted(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<StoredFactState>> {
        self.inner.get_fact_including_deleted(tenant, fact_id).await
    }
    async fn list_fact_states(
        &self,
        tenant: &TenantContext,
        after: Option<Uuid>,
        limit: usize,
    ) -> MemcoreResult<Vec<StoredFactState>> {
        self.inner.list_fact_states(tenant, after, limit).await
    }
    async fn fact_id_exists_in_any_tenant(&self, fact_id: Uuid) -> MemcoreResult<bool> {
        self.inner.fact_id_exists_in_any_tenant(fact_id).await
    }
    async fn search_facts(&self, query: FactSearchQuery) -> MemcoreResult<Vec<Fact>> {
        self.inner.search_facts(query).await
    }
    async fn fulltext_search_facts(
        &self,
        query: FactFulltextQuery,
    ) -> MemcoreResult<Vec<ScoredFact>> {
        self.inner.fulltext_search_facts(query).await
    }
    async fn soft_delete_fact(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        self.inner.soft_delete_fact(tenant, fact_id).await
    }
    async fn delete_user_data(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.inner.delete_user_data(tenant).await
    }
    async fn delete_facts_older_than(
        &self,
        tenant: &TenantContext,
        cutoff: DateTime<Utc>,
        dry_run: bool,
    ) -> MemcoreResult<RetentionPruneResult> {
        self.inner
            .delete_facts_older_than(tenant, cutoff, dry_run)
            .await
    }
    async fn count_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_facts_by_org(org_id).await
    }
    async fn count_active_facts_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_active_facts_by_org(org_id).await
    }
    async fn count_facts_by_user(&self, tenant: &TenantContext) -> MemcoreResult<usize> {
        self.inner.count_facts_by_user(tenant).await
    }
    async fn count_users_by_org(&self, org_id: &str) -> MemcoreResult<usize> {
        self.inner.count_users_by_org(org_id).await
    }
    async fn list_users_by_org(
        &self,
        query: OrgUserListQuery,
    ) -> MemcoreResult<Vec<OrgUserSummary>> {
        self.inner.list_users_by_org(query).await
    }
}

/// The mock journal with a commit that can fail once.
#[derive(Default)]
struct Journal {
    inner: MockIngestJournalStore,
    fail_commit: AtomicBool,
}

#[async_trait]
impl IngestJournalStore for Journal {
    async fn begin(
        &self,
        t: &TenantContext,
        id: Uuid,
        step: &IngestJournalStep,
    ) -> MemcoreResult<()> {
        self.inner.begin(t, id, step).await
    }
    async fn append(
        &self,
        t: &TenantContext,
        id: Uuid,
        step: &IngestJournalStep,
    ) -> MemcoreResult<()> {
        self.inner.append(t, id, step).await
    }
    async fn commit(
        &self,
        t: &TenantContext,
        id: Uuid,
        receipt: Option<&IngestReceipt>,
    ) -> MemcoreResult<()> {
        if self.fail_commit.swap(false, Ordering::SeqCst) {
            return Err(MemcoreError::StorageError(
                "journal commit failed (injected)".to_string(),
            ));
        }
        self.inner.commit(t, id, receipt).await
    }
    async fn record_receipt(
        &self,
        t: &TenantContext,
        receipt: &IngestReceipt,
    ) -> MemcoreResult<()> {
        self.inner.record_receipt(t, receipt).await
    }
    async fn find_receipt(
        &self,
        t: &TenantContext,
        key: &str,
    ) -> MemcoreResult<Option<IngestReceipt>> {
        self.inner.find_receipt(t, key).await
    }
    async fn purge_receipts(&self, before: DateTime<Utc>) -> MemcoreResult<usize> {
        self.inner.purge_receipts(before).await
    }
    async fn claim(
        &self,
        id: Uuid,
        stale: Option<DateTime<Utc>>,
    ) -> MemcoreResult<Option<IngestOperation>> {
        self.inner.claim(id, stale).await
    }
    async fn finish(&self, id: Uuid) -> MemcoreResult<()> {
        self.inner.finish(id).await
    }
    async fn release(&self, id: Uuid) -> MemcoreResult<()> {
        self.inner.release(id).await
    }
    async fn list_stale(&self, stale: DateTime<Utc>, limit: usize) -> MemcoreResult<Vec<Uuid>> {
        self.inner.list_stale(stale, limit).await
    }
    async fn find_in_flight(
        &self,
        t: &TenantContext,
        fact: Uuid,
        exclude: Uuid,
    ) -> MemcoreResult<Option<Uuid>> {
        self.inner.find_in_flight(t, fact, exclude).await
    }
    async fn operation_state(&self, id: Uuid) -> MemcoreResult<Option<IngestOperationState>> {
        self.inner.operation_state(id).await
    }
    async fn count_operations(&self) -> MemcoreResult<usize> {
        self.inner.count_operations().await
    }
    async fn backlog(&self) -> MemcoreResult<memcore_core::ports::IngestJournalBacklog> {
        self.inner.backlog().await
    }
    async fn delete_user_operations(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.inner.delete_user_operations(t).await
    }
}

// ------------------------------------------------------------------ world

struct World {
    facts: Arc<Facts>,
    events: Arc<MockMemoryEventStore>,
    vectors: Arc<Vectors>,
    _embed: Arc<Embed>,
    llm: Arc<Llm>,
    journal: Arc<Journal>,
    engine: Arc<MemoryEngine>,
}

fn tenant() -> TenantContext {
    TenantContext::new("org_v21", "user_rin").unwrap()
}

fn other_tenant() -> TenantContext {
    TenantContext::new("org_v21", "user_other").unwrap()
}

fn cand(text: &str) -> CandidateFact {
    CandidateFact::new(text, MemoryType::Profile, 0.9, 0.9, None, json!({})).unwrap()
}

const A: &str = "Rin keeps a telescope on the balcony.";
const B: &str = "Rin studies Portuguese every evening.";
const C: &str = "Rin volunteers at the harbour museum.";
const LISBON: &str = "Rin lives in Lisbon near the river.";
const PORTO: &str = "Rin lives in Porto near the river.";

#[derive(Clone, Copy)]
struct Options {
    journal: bool,
    matcher: RelatedFactMatcher,
    stale_after: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            journal: true,
            matcher: RelatedFactMatcher::Legacy,
            stale_after: Duration::from_secs(300),
        }
    }
}

impl World {
    fn new() -> Self {
        Self::with(Options::default())
    }

    fn with(options: Options) -> Self {
        Self::on(
            Arc::new(Facts::default()),
            Arc::new(Vectors::default()),
            Arc::new(MockMemoryEventStore::new()),
            Arc::new(Journal::default()),
            options,
        )
    }

    /// A new engine (a restarted process) over the same stores.
    fn restarted(&self, options: Options) -> Self {
        Self::on(
            self.facts.clone(),
            self.vectors.clone(),
            self.events.clone(),
            self.journal.clone(),
            options,
        )
    }

    fn on(
        facts: Arc<Facts>,
        vectors: Arc<Vectors>,
        events: Arc<MockMemoryEventStore>,
        journal: Arc<Journal>,
        options: Options,
    ) -> Self {
        let embed = Arc::new(Embed::default());
        let llm = Arc::new(Llm::default());
        let engine = MemoryEngine::new(facts.clone(), vectors.clone(), llm.clone(), embed.clone())
            .with_embedding_dedup_config(EmbeddingDeduplicationConfig {
                enabled: false,
                ..Default::default()
            })
            .with_related_fact_matcher(options.matcher)
            .with_event_store(events.clone())
            .with_ingest_journal(options.journal.then(|| IngestJournalComponents {
                store: journal.clone(),
                stale_after: options.stale_after,
                inflight_wait: Duration::from_secs(5),
                receipt_ttl: memcore_core::DEFAULT_INGEST_RECEIPT_TTL,
            }));
        Self {
            facts,
            events,
            vectors,
            _embed: embed,
            llm,
            journal,
            engine: Arc::new(engine),
        }
    }

    fn queue(&self, texts: &[&str]) {
        self.llm
            .queue
            .lock()
            .unwrap()
            .push(texts.iter().map(|t| cand(t)).collect());
    }

    async fn add_for(&self, t: TenantContext) -> MemcoreResult<memcore_core::AddMemoryOutput> {
        add_with(self.engine.clone(), t).await
    }

    async fn add(&self) -> MemcoreResult<memcore_core::AddMemoryOutput> {
        self.add_for(tenant()).await
    }

    async fn rows(&self, t: &TenantContext, include_deleted: bool) -> Vec<Fact> {
        self.facts
            .search_facts(FactSearchQuery {
                tenant: t.clone(),
                memory_types: None,
                query_text: None,
                limit: 1000,
                cursor: None,
                include_deleted,
            })
            .await
            .unwrap()
    }

    /// Active facts of `t` (live and neither superseded nor invalid), sorted text.
    async fn active_of(&self, t: &TenantContext) -> Vec<String> {
        let mut v: Vec<String> = self
            .rows(t, false)
            .await
            .into_iter()
            .filter(|f| f.is_active())
            .map(|f| f.content)
            .collect();
        v.sort();
        v
    }

    async fn active(&self) -> Vec<String> {
        self.active_of(&tenant()).await
    }

    /// Live rows (deleted ones excluded) whose vector is missing: must always be empty.
    async fn rows_without_points(&self, t: &TenantContext) -> Vec<String> {
        let points = self.vectors.point_ids(t);
        self.rows(t, false)
            .await
            .into_iter()
            .filter(|f| !points.contains(&f.id))
            .map(|f| f.content)
            .collect()
    }

    /// Points whose fact row is not live: must always be empty.
    async fn orphan_points(&self, t: &TenantContext) -> usize {
        let live: HashSet<Uuid> = self
            .rows(t, false)
            .await
            .into_iter()
            .map(|f| f.id)
            .collect();
        self.vectors
            .point_ids(t)
            .into_iter()
            .filter(|id| !live.contains(id))
            .count()
    }

    async fn rollback_events(&self) -> usize {
        self.events
            .list_events(MemoryEventQuery::new(tenant(), 100))
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                e.metadata.get("reason").and_then(|r| r.as_str()) == Some("ingest_rollback")
            })
            .count()
    }

    fn journal_len(&self) -> usize {
        self.journal.inner.operations().len()
    }

    /// Makes every journalled operation look abandoned (its last write ten minutes ago).
    fn age_journal(&self) {
        for op in self.journal.inner.operations() {
            self.journal
                .inner
                .set_updated_at(op.id, Utc::now() - chrono::Duration::minutes(10));
        }
    }

    async fn wait_journal_empty(&self) {
        for _ in 0..200 {
            if self.journal_len() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the journal did not drain: {:?}",
            self.journal.inner.operations()
        );
    }

    /// Asserts the global invariants: no live row without its point, no point without a live row.
    async fn assert_consistent(&self, t: &TenantContext) {
        assert!(
            self.rows_without_points(t).await.is_empty(),
            "live rows without a vector: {:?}",
            self.rows_without_points(t).await
        );
        assert_eq!(self.orphan_points(t).await, 0, "points without a live row");
    }
}

async fn add_with(
    engine: Arc<MemoryEngine>,
    t: TenantContext,
) -> MemcoreResult<memcore_core::AddMemoryOutput> {
    engine
        .add_memory(AddMemoryInput {
            tenant: t,
            messages: vec![MemoryMessage {
                role: MessageRole::User,
                content: "Rin's week.".to_string(),
                ..Default::default()
            }],
            metadata: json!({}),
            session: Default::default(),
        })
        .await
}

/// Seeds `text` as a committed fact of `t` and returns it.
async fn seed(w: &World, t: TenantContext, text: &str) -> Fact {
    w.queue(&[text]);
    let out = w.add_for(t).await.expect("seed write succeeds");
    out.memories.into_iter().next().expect("seed fact stored")
}

// ------------------------------------------------------------------ B6 scenarios

#[tokio::test]
async fn t01_failure_before_persistence_stores_nothing_and_leaves_no_journal() {
    let w = World::new();
    assert!(w.add().await.is_err(), "extraction fails (nothing queued)");
    assert!(w.rows(&tenant(), true).await.is_empty());
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t02_failure_after_sql_row_before_vector_rolls_back_every_add() {
    let w = World::new();
    w.queue(&[A, B, C]);
    // A and B are fully written (row + point); C's row is written, its vector write fails.
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
    let err = w.add().await.expect_err("the failure is reported");
    assert!(matches!(err, MemcoreError::StorageError(_)), "{err:?}");
    assert!(
        w.active().await.is_empty(),
        "every add of the request is undone"
    );
    assert_eq!(
        w.rows(&tenant(), false).await.len(),
        0,
        "no live row is left, C's row without a vector included"
    );
    assert_eq!(
        w.rows(&tenant(), true).await.len(),
        3,
        "the rows stay as deleted audit records"
    );
    assert_eq!(w.rollback_events().await, 3);
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t03_v2_contrast_without_journal_leaves_the_failed_row_without_a_vector() {
    // The same fault without the journal (V2 compensation): A and B are compensated, but C's row
    // (written before its vector failed) stays live without a point until a retry heals it.
    let w = World::with(Options {
        journal: false,
        ..Default::default()
    });
    w.queue(&[A, B, C]);
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
    assert!(w.add().await.is_err());
    assert_eq!(w.active().await, vec![C.to_string()]);
    assert_eq!(w.rows_without_points(&tenant()).await, vec![C.to_string()]);
}

#[tokio::test]
async fn t04_failure_after_all_writes_at_commit_rolls_back() {
    let w = World::new();
    w.queue(&[A, B]);
    w.journal.fail_commit.store(true, Ordering::SeqCst);
    assert!(
        w.add().await.is_err(),
        "a failed commit never reports success"
    );
    assert!(w.active().await.is_empty());
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t05_cancellation_during_processing_is_rolled_back_by_its_own_task() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let engine = w.engine.clone();
    let request = tokio::spawn(add_with(engine, tenant()));
    w.llm.reached.notified().await;
    assert_eq!(
        w.active().await,
        vec![A.to_string()],
        "A is applied, B in flight"
    );
    request.abort(); // client disconnect / request deadline: the future is dropped
    let _ = request.await;
    w.wait_journal_empty().await;
    assert!(
        w.active().await.is_empty(),
        "the cancelled request's add is undone"
    );
    w.assert_consistent(&tenant()).await;
}

#[tokio::test]
async fn t06_v2_contrast_cancellation_without_journal_keeps_the_partial_write() {
    let w = World::with(Options {
        journal: false,
        ..Default::default()
    });
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let request = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    request.abort();
    let _ = request.await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        w.active().await,
        vec![A.to_string()],
        "V2: a cancelled request's add stays"
    );
}

#[tokio::test]
async fn t07_cancellation_during_rollback_is_resumed_by_the_sweep() {
    let w = World::new();
    w.queue(&[A, B, C]);
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
    // The rollback hangs on its first vector delete; the request is then cancelled mid-rollback.
    w.vectors.hold_delete.store(true, Ordering::SeqCst);
    let request = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.vectors.delete_reached.notified().await;
    request.abort();
    let _ = request.await;
    assert_eq!(
        w.journal_len(),
        1,
        "the interrupted rollback stays in the journal"
    );
    assert!(
        !w.active().await.is_empty(),
        "the rollback was interrupted before it finished"
    );
    // Later (e.g. the next sweep after a restart): the stale rollback is resumed.
    w.vectors.hold_delete.store(false, Ordering::SeqCst);
    w.age_journal();
    let report = w.engine.recover_stale_ingest_operations(100).await.unwrap();
    assert_eq!((report.rolled_back, report.failed), (1, 0), "{report:?}");
    assert!(w.active().await.is_empty());
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[test]
fn t08_process_stop_mid_request_is_rolled_back_after_restart() {
    // A real runtime shutdown with the request in flight: its task is dropped while the runtime
    // stops, so no rollback can run (the crash case). The journal keeps the operation; a new
    // engine (a restarted process) over the same stores rolls it back.
    let w = World::new();
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let first = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let engine = w.engine.clone();
    let llm = w.llm.clone();
    first.block_on(async {
        tokio::spawn(add_with(engine, tenant()));
        llm.reached.notified().await;
    });
    first.shutdown_background(); // the process stops: in-flight tasks are dropped
    std::thread::sleep(Duration::from_millis(100));

    let second = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    second.block_on(async {
        assert_eq!(
            w.active().await,
            vec![A.to_string()],
            "the partial write survived the stop"
        );
        assert_eq!(w.journal_len(), 1, "and is still journalled");
        let restarted = w.restarted(Options::default());
        let early = restarted
            .engine
            .recover_stale_ingest_operations(100)
            .await
            .unwrap();
        assert_eq!(
            early.rolled_back, 0,
            "an operation is not touched before it is stale"
        );
        w.age_journal();
        let report = restarted
            .engine
            .recover_stale_ingest_operations(100)
            .await
            .unwrap();
        assert_eq!(report.rolled_back, 1, "{report:?}");
        assert!(w.active().await.is_empty());
        w.assert_consistent(&tenant()).await;
        assert_eq!(w.journal_len(), 0);
    });
}

#[tokio::test]
async fn t09_crash_during_recovery_is_resumed_and_repeated_recovery_is_idempotent() {
    let w = World::new();
    w.queue(&[A, B, C]);
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
    // The in-request rollback fails part-way (vector deletes fail): it is released for a sweep.
    w.vectors.fail_deletes.store(true, Ordering::SeqCst);
    assert!(w.add().await.is_err());
    assert_eq!(w.journal_len(), 1);
    let op = w.journal.inner.operations().remove(0);
    assert!(op.attempts >= 1, "the failed rollback was claimed once");

    // A sweep while the store is still failing: fails again, stays journalled.
    w.age_journal();
    let failed = w.engine.recover_stale_ingest_operations(100).await.unwrap();
    assert_eq!((failed.rolled_back, failed.failed), (0, 1));
    assert_eq!(w.journal_len(), 1);

    // The store recovers: two concurrent sweeps race for the same operation.
    w.vectors.fail_deletes.store(false, Ordering::SeqCst);
    w.age_journal();
    let (one, two) = tokio::join!(
        w.engine.recover_stale_ingest_operations(100),
        w.engine.recover_stale_ingest_operations(100)
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(
        one.rolled_back + two.rolled_back,
        1,
        "claimed exactly once: {one:?} {two:?}"
    );
    assert!(w.active().await.is_empty());
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
    // Repeating after completion does nothing.
    let again = w.engine.recover_stale_ingest_operations(100).await.unwrap();
    assert_eq!(again.examined, 0);
    assert_eq!(
        w.rollback_events().await,
        3,
        "each undone add is recorded once"
    );
}

#[tokio::test]
async fn t10_sequential_retry_after_failure_stores_each_fact_once() {
    let w = World::new();
    w.queue(&[A, B, C]);
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
    assert!(w.add().await.is_err());
    w.queue(&[A, B, C]);
    w.add().await.expect("the retry succeeds");
    let mut expected = vec![A.to_string(), B.to_string(), C.to_string()];
    expected.sort();
    assert_eq!(w.active().await, expected);
    assert_eq!(
        w.rows(&tenant(), false).await.len(),
        3,
        "one live row per fact"
    );
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t11_concurrent_identical_requests_store_one_fact_and_all_succeed() {
    let w = World::new();
    w.llm.delay_ms.store(20, Ordering::SeqCst);
    for _ in 0..8 {
        w.queue(&[A]);
    }
    let mut tasks = Vec::new();
    for _ in 0..8 {
        tasks.push(tokio::spawn(add_with(w.engine.clone(), tenant())));
    }
    let mut ok = 0;
    for task in tasks {
        if task.await.unwrap().is_ok() {
            ok += 1;
        }
    }
    assert_eq!(ok, 8, "every identical request succeeds");
    assert_eq!(w.active().await, vec![A.to_string()]);
    assert_eq!(w.rows(&tenant(), false).await.len(), 1);
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t12_concurrent_retry_while_the_first_attempt_fails_keeps_the_fact() {
    // Request 1 adds A, then fails on B. Request 2 (a client retry sent while request 1 is still
    // running) sees A as a duplicate. Without the journal it would acknowledge A, which request
    // 1's compensation then deletes: a success that lost the memory.
    let w = World::new();
    w.queue(&[A, B]);
    w.queue(&[A]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(B.to_string());
    let first = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    let second = tokio::spawn(add_with(w.engine.clone(), tenant()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        !second.is_finished(),
        "the retry waits for the uncommitted A"
    );
    w.llm.release.notify_one();
    assert!(
        first.await.unwrap().is_err(),
        "request 1 fails (B's vector)"
    );
    let out = second.await.unwrap().expect("the retry succeeds");
    assert_eq!(
        out.added, 1,
        "the retry stored A itself after request 1 rolled back"
    );
    assert_eq!(w.active().await, vec![A.to_string()]);
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t13_v2_contrast_concurrent_retry_loses_the_fact_without_journal() {
    let w = World::with(Options {
        journal: false,
        ..Default::default()
    });
    w.queue(&[A, B]);
    w.queue(&[A]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(B.to_string());
    let first = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    let out = add_with(w.engine.clone(), tenant())
        .await
        .expect("V2: the retry succeeds at once");
    assert_eq!(
        out.added, 0,
        "V2: the retry acknowledged request 1's uncommitted A"
    );
    w.llm.release.notify_one();
    assert!(first.await.unwrap().is_err());
    assert_eq!(
        w.active().await,
        vec![B.to_string()],
        "V2: the acknowledged A is gone (and B's row is left without a vector, as in t03)"
    );
}

#[tokio::test]
async fn t14_partial_supersession_is_undone_as_a_unit() {
    for (label, fail_after) in [("link fails", 1usize), ("invalidate fails", 0)] {
        let w = World::with(Options {
            matcher: RelatedFactMatcher::Semantic,
            ..Default::default()
        });
        let lisbon = seed(&w, tenant(), LISBON).await;
        w.llm.update_related.store(true, Ordering::SeqCst);
        w.queue(&[PORTO]);
        // The supersession: replacement added, target invalidated (update 0), linked (update 1).
        *w.facts.fail_update_in.lock().unwrap() = Some(fail_after);
        assert!(w.add().await.is_err(), "{label}: the failure is reported");
        assert_eq!(
            w.active().await,
            vec![LISBON.to_string()],
            "{label}: the previous value is current again and the replacement is gone"
        );
        let restored = w
            .facts
            .get_fact(&tenant(), lisbon.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.superseded_at, None, "{label}");
        assert_eq!(restored.superseded_by, None, "{label}");
        w.assert_consistent(&tenant()).await;
        assert_eq!(w.journal_len(), 0, "{label}");

        // The retry supersedes cleanly and keeps history.
        w.queue(&[PORTO]);
        w.add().await.expect("retry");
        assert_eq!(w.active().await, vec![PORTO.to_string()], "{label}");
        let history = w
            .facts
            .get_fact(&tenant(), lisbon.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            history.superseded_at.is_some() && history.superseded_by.is_some(),
            "{label}"
        );
    }
}

#[tokio::test]
async fn t15_partial_in_place_update_is_restored_with_its_vector() {
    let w = World::new();
    let a = seed(&w, tenant(), A).await;
    let edited = "Rin keeps a large telescope on the roof.";
    w.llm
        .update_target
        .lock()
        .unwrap()
        .insert(edited.to_string(), a.id);
    w.queue(&[edited, C]);
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(C.to_string());
    assert!(w.add().await.is_err());
    let row = w.facts.get_fact(&tenant(), a.id).await.unwrap().unwrap();
    assert_eq!(row.content, A, "the update is undone");
    let points = w
        .vectors
        .fact_vector_points(&tenant(), a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(
        points[0].content, A,
        "and its vector carries the restored text"
    );
    assert_eq!(w.active().await, vec![A.to_string()]);
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t16_rollback_and_waits_stay_inside_the_tenant() {
    let w = World::new();
    let theirs = seed(&w, other_tenant(), A).await;
    // The other tenant writes the same text while this tenant's request holds A in flight: no
    // cross-tenant wait.
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let mine = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    w.queue(&[C]);
    tokio::time::timeout(Duration::from_secs(2), w.add_for(other_tenant()))
        .await
        .expect("another tenant never waits for this tenant's operation")
        .expect("their write succeeds");
    mine.abort();
    let _ = mine.await;
    w.wait_journal_empty().await;
    assert!(w.active().await.is_empty());
    assert_eq!(
        w.active_of(&other_tenant()).await,
        vec![A.to_string(), C.to_string()]
    );

    // A journal entry of this tenant naming the other tenant's fact id (corrupt or forged) never
    // touches it: every undo is tenant-scoped.
    w.journal
        .inner
        .begin(
            &tenant(),
            Uuid::new_v4(),
            &IngestJournalStep::Add { fact_id: theirs.id },
        )
        .await
        .unwrap();
    w.age_journal();
    let report = w.engine.recover_stale_ingest_operations(100).await.unwrap();
    assert_eq!(report.rolled_back, 1);
    assert_eq!(
        w.active_of(&other_tenant()).await,
        vec![A.to_string(), C.to_string()]
    );
    w.assert_consistent(&other_tenant()).await;
}

#[tokio::test]
async fn t17_a_request_that_recovery_claimed_never_reports_success() {
    // A request that runs past the stale window (no write deadline) is claimed by the sweep; when
    // it resumes, its next journal write is refused, it undoes its own steps and fails.
    let w = World::with(Options {
        stale_after: Duration::from_millis(1),
        ..Default::default()
    });
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let request = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let report = w.engine.recover_stale_ingest_operations(100).await.unwrap();
    assert_eq!(report.rolled_back, 1);
    w.llm.release.notify_one();
    let err = request.await.unwrap().expect_err("never a success");
    assert!(matches!(err, MemcoreError::Conflict(_)), "{err:?}");
    assert!(
        w.active().await.is_empty(),
        "B, applied after the claim, was undone too"
    );
    w.assert_consistent(&tenant()).await;
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t18_abandoned_operation_is_rolled_back_on_demand_by_a_retry() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    // An abandoned operation (its request stopped without rolling back, as after a crash): the
    // request stays parked on the held classification and its journal entry is aged past the
    // stale window.
    let _parked = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    w.age_journal();
    w.queue(&[A]);
    let out = w
        .add()
        .await
        .expect("the retry recovers the abandoned operation and succeeds");
    assert_eq!(out.added, 1);
    assert_eq!(w.active().await, vec![A.to_string()]);
    w.assert_consistent(&tenant()).await;
}

#[tokio::test]
async fn t19_forget_user_removes_the_users_journal_rows() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let request = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    assert_eq!(w.journal_len(), 1);
    w.engine
        .forget_user(ForgetUserInput { tenant: tenant() })
        .await
        .expect("forget-user");
    assert_eq!(
        w.journal_len(),
        0,
        "the before-images of the user's writes are erased"
    );
    w.llm.release.notify_one();
    assert!(
        request.await.unwrap().is_err(),
        "the interrupted write fails"
    );
    assert!(w.active().await.is_empty());
    w.assert_consistent(&tenant()).await;
}

#[tokio::test]
async fn t20_success_path_is_unchanged_and_leaves_no_journal_rows() {
    let script: [&[&str]; 3] = [&[A, B], &[A, C], &[LISBON]];
    let mut finals = Vec::new();
    for journal in [false, true] {
        let w = World::with(Options {
            journal,
            ..Default::default()
        });
        let mut outputs = Vec::new();
        for texts in script {
            w.queue(texts);
            let out = w.add().await.unwrap();
            outputs.push((out.added, out.updated, out.discarded.total()));
        }
        assert_eq!(w.journal_len(), 0);
        finals.push((outputs, w.active().await));
    }
    assert_eq!(
        finals[0], finals[1],
        "same outputs and final state with and without the journal"
    );
}

// ------------------------------------------------------------------ V2.1 closure: receipts

async fn add_identified(
    engine: Arc<MemoryEngine>,
    ids: &[&str],
) -> MemcoreResult<memcore_core::AddMemoryOutput> {
    engine
        .add_memory(AddMemoryInput {
            tenant: tenant(),
            messages: ids
                .iter()
                .map(|id| MemoryMessage {
                    role: MessageRole::User,
                    content: format!("message {id}"),
                    message_id: Some((*id).to_string()),
                    ..Default::default()
                })
                .collect(),
            metadata: json!({}),
            session: Default::default(),
        })
        .await
}

const A_PARAPHRASE: &str = "Rin has a telescope out on her balcony.";

#[tokio::test]
async fn t21_retry_after_a_lost_response_returns_the_recorded_result() {
    let w = World::new();
    w.queue(&[A, B]);
    let first = add_identified(w.engine.clone(), &["m1", "m2"])
        .await
        .unwrap();
    assert_eq!(first.added, 2);
    // The response is lost; the client retries the same request. A real model may extract a
    // paraphrase this time: the retry must not store it.
    w.queue(&[A_PARAPHRASE]);
    let retry = add_identified(w.engine.clone(), &["m1", "m2"])
        .await
        .unwrap();
    assert_eq!(retry.added, first.added, "the recorded result");
    let mut ids: Vec<_> = retry.memories.iter().map(|f| f.id).collect();
    let mut first_ids: Vec<_> = first.memories.iter().map(|f| f.id).collect();
    ids.sort();
    first_ids.sort();
    assert_eq!(ids, first_ids, "the same committed facts");
    let mut expected = vec![A.to_string(), B.to_string()];
    expected.sort();
    assert_eq!(w.active().await, expected, "nothing new stored");
    assert_eq!(
        w.llm.queue.lock().unwrap().len(),
        1,
        "no extraction ran for the replay"
    );
    assert_eq!(w.journal_len(), 0);
}

#[tokio::test]
async fn t22_without_stable_message_ids_a_retry_runs_again() {
    let w = World::new();
    w.queue(&[A]);
    w.add().await.unwrap();
    w.queue(&[A_PARAPHRASE]);
    w.add().await.unwrap();
    assert_eq!(
        w.active().await.len(),
        2,
        "no identity: documented, the paraphrase is stored"
    );
}

#[tokio::test]
async fn t23_a_failed_attempt_leaves_no_receipt_and_its_retry_runs() {
    let w = World::new();
    w.queue(&[A, B]);
    *w.vectors.fail_upsert_text.lock().unwrap() = Some(B.to_string());
    assert!(
        add_identified(w.engine.clone(), &["m1", "m2"])
            .await
            .is_err()
    );
    w.queue(&[A, B]);
    let retry = add_identified(w.engine.clone(), &["m1", "m2"])
        .await
        .unwrap();
    assert_eq!(
        retry.added, 2,
        "the retry ran (no receipt for a failed request)"
    );
    assert_eq!(w.active().await.len(), 2);
    w.assert_consistent(&tenant()).await;
}

#[tokio::test]
async fn t24_receipts_expire_and_forget_user_erases_them() {
    let w = World::with(Options {
        stale_after: Duration::from_secs(300),
        ..Default::default()
    });
    w.queue(&[A]);
    add_identified(w.engine.clone(), &["m1"]).await.unwrap();
    // Not expired yet: the sweep keeps it (a replay still answers).
    let report = w.engine.recover_stale_ingest_operations(10).await.unwrap();
    assert_eq!(report.receipts_purged, 0);
    w.queue(&[A_PARAPHRASE]);
    assert_eq!(
        add_identified(w.engine.clone(), &["m1"])
            .await
            .unwrap()
            .added,
        1
    );
    assert_eq!(w.active().await.len(), 1);
    // Forget-user erases the receipt: the same request then runs as new.
    w.engine
        .forget_user(ForgetUserInput { tenant: tenant() })
        .await
        .unwrap();
    assert!(
        w.journal
            .inner
            .find_receipt(&tenant(), "x")
            .await
            .unwrap()
            .is_none()
    );
    w.queue(&[A]);
    assert_eq!(
        add_identified(w.engine.clone(), &["m1"])
            .await
            .unwrap()
            .added,
        1
    );
    // Expiry: an engine with a 0 s receipt TTL purges it at its next sweep.
    let expiring = MemoryEngine::new(
        w.facts.clone(),
        w.vectors.clone(),
        w.llm.clone(),
        Arc::new(Embed::default()),
    )
    .with_ingest_journal(Some(IngestJournalComponents {
        store: w.journal.clone(),
        stale_after: Duration::from_secs(300),
        inflight_wait: Duration::from_secs(5),
        receipt_ttl: Duration::ZERO,
    }));
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(
        expiring
            .recover_stale_ingest_operations(10)
            .await
            .unwrap()
            .receipts_purged,
        1
    );
}

#[tokio::test]
async fn t25_recovery_status_distinguishes_idle_recovering_and_stalled() {
    let w = World::new();
    let state = |s: Option<memcore_core::IngestRecoveryStatus>| s.unwrap().state;
    assert_eq!(
        state(w.engine.ingest_recovery_status().await.unwrap()),
        "idle"
    );
    // An in-flight request: a normal backlog.
    w.queue(&[A, B]);
    *w.llm.hold_on.lock().unwrap() = Some(B.to_string());
    let request = tokio::spawn(add_with(w.engine.clone(), tenant()));
    w.llm.reached.notified().await;
    let status = w.engine.ingest_recovery_status().await.unwrap().unwrap();
    assert_eq!(
        (status.state, status.pending, status.rolling_back),
        ("recovering", 1, 0)
    );
    // Older than three stale windows (300 s each) and still journalled: stalled.
    let op = w.journal.inner.operations().remove(0);
    w.journal
        .inner
        .set_updated_at(op.id, Utc::now() - chrono::Duration::minutes(16));
    assert_eq!(
        state(w.engine.ingest_recovery_status().await.unwrap()),
        "stalled"
    );
    // Repeatedly failing rollbacks are stalled too, whatever their age.
    w.vectors.fail_deletes.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        w.age_journal();
        let report = w.engine.recover_stale_ingest_operations(10).await.unwrap();
        assert_eq!(report.failed, 1);
    }
    let status = w.engine.ingest_recovery_status().await.unwrap().unwrap();
    assert_eq!(
        (status.state, status.rolling_back, status.max_attempts),
        ("stalled", 1, 3)
    );
    // Recovered: idle again.
    w.vectors.fail_deletes.store(false, Ordering::SeqCst);
    w.age_journal();
    assert_eq!(
        w.engine
            .recover_stale_ingest_operations(10)
            .await
            .unwrap()
            .rolled_back,
        1
    );
    assert_eq!(
        state(w.engine.ingest_recovery_status().await.unwrap()),
        "idle"
    );
    request.abort();
    let _ = request.await;
}
