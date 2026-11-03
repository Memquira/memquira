//! A vector store that is opened only if it already exists and created only on the first write
//! .
//!
//! Used for episode vectors while `MEMCORE_EPISODES_ENABLED` is off: lifecycle operations (forget,
//! retention, replace import) must reach vectors that were persisted while the feature was on, and
//! an explicit import must restore them, but starting memcore must never create an episode
//! collection. Reads and deletes against a store that does not exist are empty / no-ops; the
//! first `upsert_vector` creates it.
//!
//! Deletion-only access: with a deletion opener, `delete_by_fact_id` and
//! `delete_by_user` reach the configured store even when its embedding identity does not match
//! the configured provider (forget-user and retention must not be blocked by a model change).
//! The deletion opener connects to the same collection without the identity checks and is used
//! ONLY for tenant-filtered deletes; searches and writes still go through the verified opener,
//! so an identity mismatch still refuses them.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use memcore_common::MemcoreResult;
use memcore_core::TenantContext;
use memcore_core::ports::{
    VectorPointState, VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore,
    VectorWriteIntent,
};
use uuid::Uuid;

type BoxFuture<T> = Pin<Box<dyn Future<Output = MemcoreResult<T>> + Send>>;
type OpenExisting = Box<dyn Fn() -> BoxFuture<Option<Arc<dyn VectorStore>>> + Send + Sync>;
type Create = Box<dyn Fn() -> BoxFuture<Arc<dyn VectorStore>> + Send + Sync>;

pub struct LazyVectorStore {
    open_existing: OpenExisting,
    create: Create,
    inner: Mutex<Option<Arc<dyn VectorStore>>>,
    /// Opens the existing store for deletes only, without identity checks.
    open_for_deletion: Option<OpenExisting>,
    deletion_inner: Mutex<Option<Arc<dyn VectorStore>>>,
}

impl LazyVectorStore {
    /// `open_existing` returns the store only if it already exists and never creates it; `create`
    /// opens or creates it.
    pub fn new(
        open_existing: impl Fn() -> BoxFuture<Option<Arc<dyn VectorStore>>> + Send + Sync + 'static,
        create: impl Fn() -> BoxFuture<Arc<dyn VectorStore>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            open_existing: Box::new(open_existing),
            create: Box::new(create),
            inner: Mutex::new(None),
            open_for_deletion: None,
            deletion_inner: Mutex::new(None),
        }
    }

    /// Adds a deletion-only opener: it must open the SAME configured store, only if it
    /// exists, and is used for nothing but `delete_by_fact_id` / `delete_by_user`.
    pub fn with_deletion_opener(
        mut self,
        open: impl Fn() -> BoxFuture<Option<Arc<dyn VectorStore>>> + Send + Sync + 'static,
    ) -> Self {
        self.open_for_deletion = Some(Box::new(open));
        self
    }

    /// The store deletes go to: the verified store when it is already open, else the
    /// deletion-only handle (when configured), else the verified opener.
    async fn deletion_target(&self) -> MemcoreResult<Option<Arc<dyn VectorStore>>> {
        if let Some(store) = self.cached() {
            return Ok(Some(store));
        }
        let Some(open) = &self.open_for_deletion else {
            return self.existing().await;
        };
        if let Some(store) = self
            .deletion_inner
            .lock()
            .expect("lazy vector store lock")
            .clone()
        {
            return Ok(Some(store));
        }
        let store = open().await?;
        if let Some(store) = &store {
            *self.deletion_inner.lock().expect("lazy vector store lock") = Some(store.clone());
        }
        Ok(store)
    }

    fn cached(&self) -> Option<Arc<dyn VectorStore>> {
        self.inner.lock().expect("lazy vector store lock").clone()
    }

    fn remember(&self, store: Arc<dyn VectorStore>) -> Arc<dyn VectorStore> {
        let mut inner = self.inner.lock().expect("lazy vector store lock");
        inner.get_or_insert(store).clone()
    }

    /// The store if it exists (opening it once), without creating it.
    async fn existing(&self) -> MemcoreResult<Option<Arc<dyn VectorStore>>> {
        if let Some(store) = self.cached() {
            return Ok(Some(store));
        }
        Ok((self.open_existing)()
            .await?
            .map(|store| self.remember(store)))
    }

    /// Whether the store has been opened or created (for tests and diagnostics).
    pub fn is_open(&self) -> bool {
        self.cached().is_some()
    }
}

#[async_trait]
impl VectorStore for LazyVectorStore {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        let store = match self.existing().await? {
            Some(store) => store,
            None => {
                let created = (self.create)().await?;
                self.remember(created)
            }
        };
        store.upsert_vector(tenant, record).await
    }

    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        let store = match self.existing().await? {
            Some(store) => store,
            None => {
                let created = (self.create)().await?;
                self.remember(created)
            }
        };
        store.write_vector(tenant, record, intent).await
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        match self.existing().await? {
            Some(store) => store.search_vectors(query).await,
            None => Ok(Vec::new()),
        }
    }

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        match self.deletion_target().await? {
            Some(store) => store.delete_by_fact_id(tenant, fact_id).await,
            None => Ok(()),
        }
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        match self.deletion_target().await? {
            Some(store) => store.delete_by_user(tenant).await,
            None => Ok(()),
        }
    }

    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        match self.existing().await? {
            Some(store) => store.fact_vector_points(tenant, fact_id).await,
            None => Ok(Some(Vec::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::MockVectorStore;
    use memcore_core::MemoryType;
    use serde_json::json;

    fn record(tenant: &TenantContext) -> VectorRecord {
        VectorRecord {
            id: Uuid::new_v4(),
            fact_id: Uuid::new_v4(),
            org_id: tenant.org_id.clone(),
            user_id: tenant.user_id.clone(),
            embedding: vec![1.0, 0.0],
            content: "episode".to_string(),
            memory_type: MemoryType::Conversation,
            metadata: json!({}),
        }
    }

    /// A lazy store over a backing store that "exists" once `exists` is set.
    fn lazy(
        backing: Arc<MockVectorStore>,
        exists: Arc<std::sync::atomic::AtomicBool>,
        creations: Arc<AtomicUsize>,
    ) -> LazyVectorStore {
        let (open_backing, open_exists) = (backing.clone(), exists.clone());
        LazyVectorStore::new(
            move || {
                let (backing, exists) = (open_backing.clone(), open_exists.clone());
                Box::pin(async move {
                    Ok(exists
                        .load(Ordering::SeqCst)
                        .then_some(backing as Arc<dyn VectorStore>))
                })
            },
            move || {
                let (backing, exists, creations) =
                    (backing.clone(), exists.clone(), creations.clone());
                Box::pin(async move {
                    creations.fetch_add(1, Ordering::SeqCst);
                    exists.store(true, Ordering::SeqCst);
                    Ok(backing as Arc<dyn VectorStore>)
                })
            },
        )
    }

    #[tokio::test]
    async fn reads_and_deletes_never_create_and_the_first_write_does() {
        let tenant = TenantContext::new("org", "user").unwrap();
        let exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let creations = Arc::new(AtomicUsize::new(0));
        let store = lazy(
            Arc::new(MockVectorStore::new()),
            exists.clone(),
            creations.clone(),
        );
        let query = VectorSearchQuery {
            tenant: tenant.clone(),
            embedding: vec![1.0, 0.0],
            limit: 10,
            memory_types: None,
            metadata_filter: None,
        };
        assert!(
            store
                .search_vectors(query.clone())
                .await
                .unwrap()
                .is_empty()
        );
        store.delete_by_user(&tenant).await.unwrap();
        store
            .delete_by_fact_id(&tenant, Uuid::new_v4())
            .await
            .unwrap();
        assert_eq!(creations.load(Ordering::SeqCst), 0);
        assert!(!store.is_open());

        store.upsert_vector(&tenant, record(&tenant)).await.unwrap();
        assert_eq!(creations.load(Ordering::SeqCst), 1);
        assert_eq!(store.search_vectors(query.clone()).await.unwrap().len(), 1);
        store.upsert_vector(&tenant, record(&tenant)).await.unwrap();
        assert_eq!(creations.load(Ordering::SeqCst), 1, "created once");
        store.delete_by_user(&tenant).await.unwrap();
        assert!(store.search_vectors(query).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_existing_store_is_opened_not_created() {
        let tenant = TenantContext::new("org", "user").unwrap();
        let backing = Arc::new(MockVectorStore::new());
        backing
            .upsert_vector(&tenant, record(&tenant))
            .await
            .unwrap();
        let exists = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let creations = Arc::new(AtomicUsize::new(0));
        let store = lazy(backing, exists, creations.clone());
        store.delete_by_user(&tenant).await.unwrap();
        assert!(store.is_open());
        assert_eq!(creations.load(Ordering::SeqCst), 0);
    }
}
