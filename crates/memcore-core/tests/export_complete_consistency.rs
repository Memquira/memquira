//! A complete export never labels a mixed archive complete. Writes that land while the
//! export pages are detected (two full reads must be identical): continuous writes refuse the
//! export (Conflict, EXPORT_CONCURRENT_MODIFICATION); a single write is absorbed by a retry that
//! returns a consistent archive. Deterministic: the "concurrent" write is injected by the store.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::{
    CompleteExportInput, MemoryEngine, MemoryEvent, MemoryEventOperation, MemoryEventQuery,
    MemoryEventStore, TenantContext,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{MockFactStore, MockMemoryEventStore, MockVectorStore};
use serde_json::json;

/// Delegates to a mock store; each of the first `writes` `list_events` calls records one new event
/// for the listed tenant AFTER reading (a write landing between two pages).
struct WritingDuringReads {
    inner: MockMemoryEventStore,
    writes: AtomicUsize,
}

fn event(t: &TenantContext, i: usize) -> MemoryEvent {
    MemoryEvent::new(
        t.org_id.clone(),
        t.user_id.clone(),
        None,
        MemoryEventOperation::Add,
        None,
        Some(format!("invented memory {i}")),
        None,
        None,
        json!({}),
    )
}

#[async_trait]
impl MemoryEventStore for WritingDuringReads {
    async fn record_event(&self, t: &TenantContext, e: MemoryEvent) -> MemcoreResult<MemoryEvent> {
        self.inner.record_event(t, e).await
    }
    async fn list_events(&self, q: MemoryEventQuery) -> MemcoreResult<Vec<MemoryEvent>> {
        let rows = self.inner.list_events(q.clone()).await?;
        if self
            .writes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            self.inner
                .record_event(&q.tenant, event(&q.tenant, 999))
                .await?;
        }
        Ok(rows)
    }
    async fn list_events_by_org(
        &self,
        q: memcore_core::OrgMemoryEventQuery,
    ) -> MemcoreResult<Vec<MemoryEvent>> {
        self.inner.list_events_by_org(q).await
    }
    async fn delete_events_older_than(
        &self,
        t: &TenantContext,
        c: DateTime<Utc>,
        d: bool,
    ) -> MemcoreResult<usize> {
        self.inner.delete_events_older_than(t, c, d).await
    }
    async fn count_events_by_org(&self, org: &str) -> MemcoreResult<usize> {
        self.inner.count_events_by_org(org).await
    }
    async fn erase_user_audit_content(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.inner.erase_user_audit_content(t).await
    }
    async fn count_user_audit_content(&self, t: &TenantContext) -> MemcoreResult<usize> {
        self.inner.count_user_audit_content(t).await
    }
}

async fn engine_with(writes: usize, tenant: &TenantContext, existing: usize) -> MemoryEngine {
    let store = WritingDuringReads {
        inner: MockMemoryEventStore::new(),
        writes: AtomicUsize::new(0),
    };
    for i in 0..existing {
        store
            .inner
            .record_event(tenant, event(tenant, i))
            .await
            .unwrap();
    }
    store.writes.store(writes, Ordering::SeqCst);
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(Arc::new(store))
}

fn input(tenant: &TenantContext) -> CompleteExportInput {
    CompleteExportInput {
        tenant: tenant.clone(),
        include_deleted: false,
        page_size: 5,
        max_records: 10_000,
    }
}

#[tokio::test]
async fn continuous_writes_refuse_the_export_instead_of_labelling_it_complete() {
    let t = TenantContext::new("org_c", "user_c").unwrap();
    let engine = engine_with(usize::MAX, &t, 12).await;
    match engine.export_user_data_complete(input(&t)).await {
        Err(MemcoreError::Conflict(message)) => {
            assert!(
                message.contains("EXPORT_CONCURRENT_MODIFICATION"),
                "{message}"
            )
        }
        other => panic!("expected a concurrent-modification refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_single_concurrent_write_is_absorbed_by_a_consistent_retry() {
    let t = TenantContext::new("org_c", "user_c").unwrap();
    let engine = engine_with(1, &t, 12).await;
    let export = engine
        .export_user_data_complete(input(&t))
        .await
        .expect("retry succeeds once writes stop");
    let manifest = export.completeness.expect("complete");
    assert_eq!(
        manifest.counts.memory_events, 13,
        "the archive reflects one consistent state"
    );
    assert_eq!(export.memory_events.len(), 13);
    let unique: std::collections::HashSet<_> = export.memory_events.iter().map(|e| e.id).collect();
    assert_eq!(unique.len(), 13);
}

#[tokio::test]
async fn no_writes_complete_on_the_first_attempt_and_page_limits_are_validated() {
    let t = TenantContext::new("org_c", "user_c").unwrap();
    let engine = engine_with(0, &t, 12).await;
    let export = engine.export_user_data_complete(input(&t)).await.unwrap();
    assert_eq!(export.completeness.unwrap().counts.memory_events, 12);
    for page_size in [0, 1_001] {
        let mut bad = input(&t);
        bad.page_size = page_size;
        assert!(matches!(
            engine.export_user_data_complete(bad).await,
            Err(MemcoreError::ValidationError(_))
        ));
    }
}

/// Pre-freeze check: the bound is a RECORD count, not a byte or memory bound. One 2 MiB record fits
/// a 1-record bound and comes back intact; a second record of any size is refused.
#[tokio::test]
async fn the_bound_counts_records_not_bytes() {
    let t = TenantContext::new("org_big", "user_big").unwrap();
    let store = Arc::new(MockMemoryEventStore::new());
    let big = "x".repeat(2 * 1024 * 1024);
    let mut one = event(&t, 0);
    one.new_content = Some(big.clone());
    store.record_event(&t, one).await.unwrap();
    let mut bounded = input(&t);
    bounded.max_records = 1;
    let export = store_engine(store.clone())
        .export_user_data_complete(bounded.clone())
        .await
        .expect("one large record fits a one-record bound");
    assert_eq!(export.memory_events.len(), 1);
    assert_eq!(
        export.memory_events[0].new_content.as_deref(),
        Some(big.as_str())
    );
    store.record_event(&t, event(&t, 1)).await.unwrap();
    match store_engine(store).export_user_data_complete(bounded).await {
        Err(MemcoreError::ValidationError(message)) => {
            assert!(message.contains("EXPORT_TOO_LARGE"), "{message}")
        }
        other => panic!("expected EXPORT_TOO_LARGE, got {other:?}"),
    }
}

/// Pre-freeze check: a real concurrent writer (separate task on a multi-threaded runtime) adding
/// events while complete exports run. Every export either succeeds with an internally consistent
/// archive (unique ids, manifest counts equal to the sections) or is refused with
/// EXPORT_CONCURRENT_MODIFICATION; once the writer stops, the export matches the store exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_writer_never_yields_an_inconsistent_complete_archive() {
    let t = TenantContext::new("org_cw", "user_cw").unwrap();
    let store = Arc::new(MockMemoryEventStore::new());
    for i in 0..200 {
        store.record_event(&t, event(&t, i)).await.unwrap();
    }
    let engine = Arc::new(store_engine(store.clone()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (store, t, stop) = (store.clone(), t.clone(), stop.clone());
        tokio::spawn(async move {
            let mut i = 1_000;
            // At most 2,000 writes, so the store stays far below the record bound.
            while !stop.load(Ordering::SeqCst) && i < 3_000 {
                store.record_event(&t, event(&t, i)).await.unwrap();
                i += 1;
                tokio::task::yield_now().await;
            }
            i - 1_000
        })
    };
    let (mut completed, mut refused) = (0, 0);
    let mut paged = input(&t);
    paged.page_size = 50;
    for _ in 0..20 {
        match engine.export_user_data_complete(paged.clone()).await {
            Ok(export) => {
                let manifest = export.completeness.as_ref().expect("complete");
                assert_eq!(manifest.counts.memory_events, export.memory_events.len());
                let unique: std::collections::HashSet<_> =
                    export.memory_events.iter().map(|e| e.id).collect();
                assert_eq!(unique.len(), export.memory_events.len(), "no duplicates");
                completed += 1;
            }
            Err(MemcoreError::Conflict(message)) => {
                assert!(message.contains("EXPORT_CONCURRENT_MODIFICATION"));
                refused += 1;
            }
            Err(other) => panic!("unexpected error {other}"),
        }
    }
    stop.store(true, Ordering::SeqCst);
    let written = writer.await.unwrap();
    assert!(written > 0, "the writer ran concurrently");
    let quiet = engine.export_user_data_complete(input(&t)).await.unwrap();
    assert_eq!(quiet.memory_events.len(), 200 + written);
    eprintln!(
        "concurrent writer: {written} writes; exports completed {completed}, refused {refused}"
    );
}

fn store_engine(store: Arc<MockMemoryEventStore>) -> MemoryEngine {
    MemoryEngine::new(
        Arc::new(MockFactStore::new()),
        Arc::new(MockVectorStore::new()),
        Arc::new(MockLlmProvider::new()),
        Arc::new(MockEmbeddingProvider::new(4)),
    )
    .with_event_store(store)
}
