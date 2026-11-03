#![cfg(feature = "qdrant")]

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EmbeddingIdentity, EmbeddingProvider, PointPage, PointScope, StoredPoint, VectorIdentityStatus,
    VectorPointState, VectorRecord, VectorSearchQuery, VectorSearchResult, VectorStore,
    VectorWriteIntent,
};
use memcore_core::{MemoryType, TenantContext};
use qdrant_client::Payload;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    Condition, CountPointsBuilder, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    DeletePointsBuilder, Distance, FieldType, Filter, GetPointsBuilder, PayloadSchemaType, PointId,
    PointStruct, PointsIdsList, QueryPoints, QueryPointsBuilder, ScrollPointsBuilder,
    SearchParamsBuilder, UpsertPointsBuilder, Value as QdrantValue, VectorParamsBuilder,
    point_id::PointIdOptions, value::Kind, vectors_config,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use uuid::Uuid;

const PAYLOAD_ID: &str = "id";
const PAYLOAD_FACT_ID: &str = "fact_id";
const PAYLOAD_ORG_ID: &str = "org_id";
const PAYLOAD_USER_ID: &str = "user_id";
const PAYLOAD_CONTENT: &str = "content";
const PAYLOAD_MEMORY_TYPE: &str = "memory_type";
const PAYLOAD_METADATA: &str = "metadata";

/// Keyword payload indexes of the fact collection: the tenant filter of every search,
/// count and delete (`org_id`, `user_id`), the per-fact replacement delete (`fact_id`) and the
/// `memory_type` condition of dedup and typed search.
pub const FACT_PAYLOAD_INDEXES: &[&str] = &[
    PAYLOAD_ORG_ID,
    PAYLOAD_USER_ID,
    PAYLOAD_FACT_ID,
    PAYLOAD_MEMORY_TYPE,
];

/// Keyword payload indexes of the episode collection.
pub const EPISODE_PAYLOAD_INDEXES: &[&str] = &[PAYLOAD_ORG_ID, PAYLOAD_USER_ID, PAYLOAD_FACT_ID];

/// How the store writes vectors (`MEMCORE_QDRANT_WRITE_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QdrantWriteMode {
    /// Legacy behaviour: every write goes through the filtered pre-delete; a replacement also
    /// counts and deletes first (`delete_by_fact_id`). The store's own default.
    #[default]
    Legacy,
    /// A fresh fact (`VectorWriteIntent::FreshFact`) and a stable-id episode upsert without the
    /// pre-delete; a replacement is one filtered delete and the upsert.
    Optimized,
}

/// What [`QdrantVectorStore::ensure_payload_indexes`] found and created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PayloadIndexReport {
    /// Indexes created by this call, in request order.
    pub created: Vec<String>,
    /// Keyword indexes that already existed (nothing was written for them).
    pub present: Vec<String>,
}

fn memory_type_to_str(value: MemoryType) -> &'static str {
    value.as_str()
}

fn memory_type_from_str(value: &str) -> MemcoreResult<MemoryType> {
    MemoryType::parse_label(value)
        .map_err(|_| MemcoreError::StorageError(format!("invalid memory_type value: {value}")))
}

fn storage_error(context: impl Into<String>, error: impl std::fmt::Display) -> MemcoreError {
    MemcoreError::StorageError(format!("{}: {error}", context.into()))
}

fn ensure_record_tenant(record: &VectorRecord, tenant: &TenantContext) -> MemcoreResult<()> {
    if record.org_id == tenant.org_id && record.user_id == tenant.user_id {
        Ok(())
    } else {
        Err(MemcoreError::Forbidden)
    }
}

fn tenant_filter(tenant: &TenantContext) -> Filter {
    Filter::must([
        Condition::matches(PAYLOAD_ORG_ID, tenant.org_id.clone()),
        Condition::matches(PAYLOAD_USER_ID, tenant.user_id.clone()),
    ])
}

fn tenant_fact_filter(tenant: &TenantContext, fact_id: Uuid) -> Filter {
    Filter::must([
        Condition::matches(PAYLOAD_ORG_ID, tenant.org_id.clone()),
        Condition::matches(PAYLOAD_USER_ID, tenant.user_id.clone()),
        Condition::matches(PAYLOAD_FACT_ID, fact_id.to_string()),
    ])
}

fn search_filter(query: &VectorSearchQuery) -> Filter {
    let mut must = vec![
        Condition::matches(PAYLOAD_ORG_ID, query.tenant.org_id.clone()),
        Condition::matches(PAYLOAD_USER_ID, query.tenant.user_id.clone()),
    ];

    if let Some(types) = &query.memory_types
        && !types.is_empty()
    {
        let type_conditions = types
            .iter()
            .map(|memory_type| {
                Condition::matches(
                    PAYLOAD_MEMORY_TYPE,
                    memory_type_to_str(*memory_type).to_string(),
                )
            })
            .collect::<Vec<_>>();
        must.push(Filter::should(type_conditions).into());
    }

    Filter::must(must)
}

fn record_to_payload(record: &VectorRecord) -> MemcoreResult<Payload> {
    Payload::try_from(serde_json::json!({
        PAYLOAD_ID: record.id.to_string(),
        PAYLOAD_FACT_ID: record.fact_id.to_string(),
        PAYLOAD_ORG_ID: record.org_id,
        PAYLOAD_USER_ID: record.user_id,
        PAYLOAD_CONTENT: record.content,
        PAYLOAD_MEMORY_TYPE: memory_type_to_str(record.memory_type),
        PAYLOAD_METADATA: record.metadata.to_string(),
    }))
    .map_err(|error| storage_error("failed to build qdrant payload", error))
}

fn value_string(value: &QdrantValue) -> MemcoreResult<String> {
    match &value.kind {
        Some(Kind::StringValue(value)) => Ok(value.clone()),
        _ => Err(storage_error(
            "invalid qdrant payload value",
            "expected string value",
        )),
    }
}

fn map_string(payload: &HashMap<String, QdrantValue>, key: &str) -> MemcoreResult<String> {
    let value = payload
        .get(key)
        .ok_or_else(|| storage_error("missing qdrant payload field", format!("{key} missing")))?;
    value_string(value)
}

fn scored_point_to_result(
    point: qdrant_client::qdrant::ScoredPoint,
) -> MemcoreResult<VectorSearchResult> {
    if point.payload.is_empty() {
        return Err(storage_error(
            "missing qdrant point payload",
            "payload absent",
        ));
    }

    let fact_id = Uuid::parse_str(&map_string(&point.payload, PAYLOAD_FACT_ID)?)
        .map_err(|error| storage_error("invalid fact_id uuid in qdrant payload", error))?;
    let memory_type = memory_type_from_str(&map_string(&point.payload, PAYLOAD_MEMORY_TYPE)?)?;
    let metadata: serde_json::Value =
        serde_json::from_str(&map_string(&point.payload, PAYLOAD_METADATA)?)
            .map_err(|error| storage_error("invalid metadata json in qdrant payload", error))?;

    Ok(VectorSearchResult {
        fact_id,
        content: map_string(&point.payload, PAYLOAD_CONTENT)?,
        score: point.score,
        memory_type,
        metadata,
    })
}

/// The path one search takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPath {
    /// The historical filtered HNSW request (no `exact` parameter).
    Approximate,
    /// The same filtered request with Qdrant `params.exact = true` (brute force over the tenant).
    Exact,
}

/// The bounded exact-search decision: exact iff the tenant has at most `max_tenant_points` points in
/// the queried collection (`count <= max`).
pub fn search_path_for(tenant_points: u64, max_tenant_points: u64) -> SearchPath {
    if tenant_points <= max_tenant_points {
        SearchPath::Exact
    } else {
        SearchPath::Approximate
    }
}

/// Process-local counters of the search policy (no tenant identifier is recorded).
#[derive(Debug, Default)]
struct SearchPolicyCounters {
    exact: AtomicU64,
    approximate: AtomicU64,
    tenant_counts: AtomicU64,
    tenant_count_failures: AtomicU64,
    tenant_count_nanos: AtomicU64,
}

/// A snapshot of [`QdrantVectorStore::search_policy_stats`] (diagnostics).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SearchPolicyStats {
    pub exact_searches: u64,
    pub approximate_searches: u64,
    pub tenant_counts: u64,
    pub tenant_count_failures: u64,
    pub tenant_count_nanos: u64,
}

pub struct QdrantVectorStore {
    client: Qdrant,
    collection_name: String,
    dimensions: usize,
    write_mode: QdrantWriteMode,
    /// Query-time HNSW beam sent with every search; `None` sends no parameter.
    search_hnsw_ef: Option<u64>,
    /// EXPERIMENTAL bounded exact search: `Some(n)` counts the tenant's points before
    /// every search and searches exactly when the count is <= n; `None` sends the historical request
    /// and no count.
    exact_search_max_tenant_points: Option<u64>,
    search_policy: Arc<SearchPolicyCounters>,
    /// Qdrant requests made by the write and delete paths (diagnostics and the measurement).
    write_operations: Arc<AtomicU64>,
}

impl QdrantVectorStore {
    pub async fn connect(
        url: &str,
        collection_name: &str,
        dimensions: usize,
    ) -> MemcoreResult<Self> {
        Self::connect_with_identity(url, collection_name, dimensions, None).await
    }

    /// Connects (creating the collection if needed) and fails closed when the store does not match
    /// the configured embedding provider:
    ///
    /// - the collection's vector size must equal `dimensions`;
    /// - a recorded identity (`<collection>__embedding_identity`) must equal `identity`;
    /// - a local provider (`local_*`) requires a recorded identity: it records it on a new or empty
    ///   collection and refuses a non-empty collection without one (no implicit reindex);
    /// - a remote provider on a collection without a record proceeds unchanged and writes nothing
    ///   (stores created by earlier versions keep working).
    pub async fn connect_with_identity(
        url: &str,
        collection_name: &str,
        dimensions: usize,
        identity: Option<&EmbeddingIdentity>,
    ) -> MemcoreResult<Self> {
        Self::connect_verified(url, collection_name, dimensions, identity, None).await
    }

    /// [`Self::connect_with_identity`] plus the vector-level checks: `probe` is the
    /// configured embedding provider (fingerprint of a new store, fingerprint check of a recorded
    /// one, compatibility proof of a legacy one). All checks run before any vector is written.
    pub async fn connect_verified(
        url: &str,
        collection_name: &str,
        dimensions: usize,
        identity: Option<&EmbeddingIdentity>,
        probe: Option<&dyn EmbeddingProvider>,
    ) -> MemcoreResult<Self> {
        if dimensions == 0 {
            return Err(MemcoreError::ValidationError(
                "embedding dimensions must be greater than 0".to_string(),
            ));
        }

        let client = Qdrant::from_url(url)
            .skip_compatibility_check()
            .build()
            .map_err(|error| storage_error("failed to connect qdrant", error))?;

        ensure_collection(&client, collection_name, dimensions).await?;
        check_vector_size(&client, collection_name, dimensions).await?;
        guard_identity(&client, collection_name, identity, probe).await?;

        Ok(Self {
            client,
            collection_name: collection_name.to_string(),
            dimensions,
            write_mode: QdrantWriteMode::default(),
            search_hnsw_ef: None,
            exact_search_max_tenant_points: None,
            search_policy: Arc::default(),
            write_operations: Arc::default(),
        })
    }

    /// Runs the identity checks of [`Self::connect_verified`] against this (existing) store; used
    /// by the lazily opened episode store, which opens without creating.
    pub async fn verify_identity(
        &self,
        identity: Option<&EmbeddingIdentity>,
        probe: Option<&dyn EmbeddingProvider>,
    ) -> MemcoreResult<()> {
        check_vector_size(&self.client, &self.collection_name, self.dimensions).await?;
        guard_identity(&self.client, &self.collection_name, identity, probe).await
    }

    /// The store for `collection_name` only if that collection already exists; never creates it
    /// (lifecycle access to data of a feature that is off).
    pub async fn connect_existing(
        url: &str,
        collection_name: &str,
        dimensions: usize,
    ) -> MemcoreResult<Option<Self>> {
        let client = Qdrant::from_url(url)
            .skip_compatibility_check()
            .build()
            .map_err(|error| storage_error("failed to connect qdrant", error))?;
        let exists = client
            .collection_exists(collection_name)
            .await
            .map_err(|error| storage_error("failed to check qdrant collection", error))?;
        Ok(exists.then(|| Self {
            client,
            collection_name: collection_name.to_string(),
            dimensions,
            write_mode: QdrantWriteMode::default(),
            search_hnsw_ef: None,
            exact_search_max_tenant_points: None,
            search_policy: Arc::default(),
            write_operations: Arc::default(),
        }))
    }

    /// Sets the write mode (default [`QdrantWriteMode::Legacy`]).
    pub fn with_write_mode(mut self, mode: QdrantWriteMode) -> Self {
        self.write_mode = mode;
        self
    }

    pub fn write_mode(&self) -> QdrantWriteMode {
        self.write_mode
    }

    /// Query-time HNSW beam for every search (`MEMCORE_QDRANT_SEARCH_HNSW_EF`).
    /// `None` keeps the earlier request (no search parameter: Qdrant's default).
    pub fn with_search_hnsw_ef(mut self, hnsw_ef: Option<u64>) -> Self {
        self.search_hnsw_ef = hnsw_ef;
        self
    }

    pub fn search_hnsw_ef(&self) -> Option<u64> {
        self.search_hnsw_ef
    }

    /// EXPERIMENTAL bounded exact search (`MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS`).
    /// `None` keeps the historical request and sends no count.
    pub fn with_exact_search_max_tenant_points(mut self, max_tenant_points: Option<u64>) -> Self {
        self.exact_search_max_tenant_points = max_tenant_points;
        self
    }

    pub fn exact_search_max_tenant_points(&self) -> Option<u64> {
        self.exact_search_max_tenant_points
    }

    /// Search-policy counters since this store was built (no tenant identifiers).
    pub fn search_policy_stats(&self) -> SearchPolicyStats {
        let c = &self.search_policy;
        SearchPolicyStats {
            exact_searches: c.exact.load(Ordering::Relaxed),
            approximate_searches: c.approximate.load(Ordering::Relaxed),
            tenant_counts: c.tenant_counts.load(Ordering::Relaxed),
            tenant_count_failures: c.tenant_count_failures.load(Ordering::Relaxed),
            tenant_count_nanos: c.tenant_count_nanos.load(Ordering::Relaxed),
        }
    }

    /// The exact Qdrant query a search sends (built here so tests can inspect it).
    /// `exact` adds `params.exact = true` (bounded exact search); nothing else changes.
    fn search_request(&self, query: &VectorSearchQuery, exact: bool) -> QueryPoints {
        let mut builder = QueryPointsBuilder::new(&self.collection_name)
            .query(query.embedding.clone())
            .filter(search_filter(query))
            .limit(query.limit as u64)
            .with_payload(true);
        let mut params = self
            .search_hnsw_ef
            .map(|ef| SearchParamsBuilder::default().hnsw_ef(ef));
        if exact {
            params = Some(params.unwrap_or_default().exact(true));
        }
        if let Some(params) = params {
            builder = builder.params(params);
        }
        builder.build()
    }

    /// The bounded exact-search count: the tenant's points (`org_id` + `user_id`, the same mandatory
    /// filter every search uses) in THIS store's collection, counted exactly. Read-only; not a write
    /// operation. A failure is an explicit storage error (the search is not attempted).
    pub async fn tenant_point_count_for_search(
        &self,
        tenant: &TenantContext,
    ) -> MemcoreResult<u64> {
        let started = Instant::now();
        let result = self
            .client
            .count(
                CountPointsBuilder::new(&self.collection_name)
                    .filter(tenant_filter(tenant))
                    .exact(true),
            )
            .await;
        let c = &self.search_policy;
        c.tenant_counts.fetch_add(1, Ordering::Relaxed);
        c.tenant_count_nanos
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        match result {
            Ok(response) => Ok(response.result.map(|count| count.count).unwrap_or(0)),
            Err(error) => {
                c.tenant_count_failures.fetch_add(1, Ordering::Relaxed);
                Err(storage_error(
                    "failed to count the tenant's qdrant points for the bounded exact search",
                    error,
                ))
            }
        }
    }

    /// The path the next search of `tenant` takes under this store's policy (performs the count when
    /// the policy is set; `Approximate` without any request when it is unset).
    pub async fn search_path(&self, tenant: &TenantContext) -> MemcoreResult<SearchPath> {
        match self.exact_search_max_tenant_points {
            None => Ok(SearchPath::Approximate),
            Some(max) => Ok(search_path_for(
                self.tenant_point_count_for_search(tenant).await?,
                max,
            )),
        }
    }

    /// Qdrant requests issued by writes and deletes so far (counts, deletes, upserts).
    pub fn write_operation_count(&self) -> u64 {
        self.write_operations.load(Ordering::Relaxed)
    }

    /// The collection this store reads and writes.
    pub fn collection_name(&self) -> &str {
        &self.collection_name
    }

    /// Exact number of points the tenant (org + user) has in this collection (
    /// the deletion-maintenance postcondition). Reads only; no vector is compared or written.
    pub async fn count_tenant_points(&self, tenant: &TenantContext) -> MemcoreResult<u64> {
        self.count_matching(tenant_filter(tenant)).await
    }

    fn count_write_operation(&self) {
        self.write_operations.fetch_add(1, Ordering::Relaxed);
    }

    /// The tenant's points of `fact_id` (ids, and payload when `with_payload`).
    async fn fact_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
        with_payload: bool,
    ) -> MemcoreResult<Vec<qdrant_client::qdrant::RetrievedPoint>> {
        self.count_write_operation();
        let response = self
            .client
            .scroll(
                ScrollPointsBuilder::new(&self.collection_name)
                    .filter(tenant_fact_filter(tenant, fact_id))
                    .limit(256)
                    .with_payload(with_payload)
                    .with_vectors(false),
            )
            .await
            .map_err(|error| storage_error("failed to list qdrant fact points", error))?;
        Ok(response.result)
    }

    /// Replaces the fact's vector without the concurrent-replacement race.
    ///
    /// The write reuses the smallest point id the fact already has (a new random id only when it
    /// has none) and then deletes exactly the OTHER ids it observed (by id, from a tenant-filtered
    /// listing). A point is only ever deleted by a writer that saw it and kept a smaller one, so
    /// the smallest point any writer saw survives: concurrent replacements never leave the fact
    /// without a vector, and overwrite one point instead of each adding their own. No existing
    /// point is re-keyed. The caller (`update_with_vector`) then verifies count and content.
    async fn replace_reusing_point(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        self.check_record(tenant, &record)?;
        let observed: Vec<PointId> = self
            .fact_points(tenant, record.fact_id, false)
            .await?
            .into_iter()
            .filter_map(|point| point.id)
            .collect();
        let keep = observed.iter().min_by_key(|id| point_id_key(id)).cloned();
        let others: Vec<PointId> = observed
            .into_iter()
            .filter(|id| Some(id) != keep.as_ref())
            .collect();
        match keep {
            Some(id) => self.upsert_point_as(id, record).await?,
            None => self.upsert_point(record).await?,
        }
        if !others.is_empty() {
            self.count_write_operation();
            self.client
                .delete_points(
                    DeletePointsBuilder::new(&self.collection_name)
                        .points(PointsIdsList { ids: others })
                        .wait(true),
                )
                .await
                .map_err(|error| storage_error("failed to delete qdrant points", error))?;
        }
        Ok(())
    }

    /// Tenant and dimension checks every write makes before touching Qdrant.
    fn check_record(&self, tenant: &TenantContext, record: &VectorRecord) -> MemcoreResult<()> {
        ensure_record_tenant(record, tenant)?;
        if record.embedding.len() != self.dimensions {
            return Err(MemcoreError::ValidationError(format!(
                "embedding length {} does not match configured dimensions {}",
                record.embedding.len(),
                self.dimensions
            )));
        }
        Ok(())
    }

    /// One upsert of the record's point (its id, vector and payload), acknowledged after apply.
    async fn upsert_point(&self, record: VectorRecord) -> MemcoreResult<()> {
        let id = PointId::from(record.id.to_string());
        self.upsert_point_as(id, record).await
    }

    /// The upsert of `record`'s vector and payload under point `id`.
    async fn upsert_point_as(&self, id: PointId, record: VectorRecord) -> MemcoreResult<()> {
        let payload = record_to_payload(&record)?;
        let point = PointStruct::new(id, record.embedding, payload);
        self.count_write_operation();
        self.client
            .upsert_points(UpsertPointsBuilder::new(&self.collection_name, vec![point]).wait(true))
            .await
            .map_err(|error| storage_error("failed to upsert qdrant vector", error))?;
        Ok(())
    }

    /// Creates the keyword payload indexes in `fields` that the collection does not have yet
    /// . The current schema is read first, so a repeated call writes nothing. Points,
    /// vectors, the vector configuration and HNSW are not touched; `is_tenant` is not set.
    ///
    /// Fails closed when a field already has an index of another type (it is never replaced) or when
    /// Qdrant rejects a read or a create.
    pub async fn ensure_payload_indexes(
        &self,
        fields: &[&str],
    ) -> MemcoreResult<PayloadIndexReport> {
        ensure_payload_indexes(&self.client, &self.collection_name, fields).await
    }

    async fn delete_matching(&self, filter: Filter) -> MemcoreResult<()> {
        self.count_write_operation();
        self.client
            .delete_points(
                DeletePointsBuilder::new(&self.collection_name)
                    .points(filter)
                    .wait(true),
            )
            .await
            .map_err(|error| storage_error("failed to delete qdrant points", error))?;
        Ok(())
    }

    async fn count_matching(&self, filter: Filter) -> MemcoreResult<u64> {
        self.count_write_operation();
        let response = self
            .client
            .count(
                CountPointsBuilder::new(&self.collection_name)
                    .filter(filter)
                    .exact(true),
            )
            .await
            .map_err(|error| storage_error("failed to count qdrant points", error))?;
        Ok(response.result.map(|count| count.count).unwrap_or(0))
    }
}

/// The filter of a verification scope (exact tenant, tenant + entity, or the
/// points without a usable tenant payload).
fn scope_filter(scope: &PointScope) -> Filter {
    match scope {
        PointScope::Tenant(tenant) => tenant_filter(tenant),
        PointScope::TenantEntity(tenant, entity) => tenant_fact_filter(tenant, *entity),
        PointScope::Unattributed => Filter::should([
            Condition::is_empty(PAYLOAD_ORG_ID),
            Condition::is_null(PAYLOAD_ORG_ID),
            Condition::matches(PAYLOAD_ORG_ID, String::new()),
            Condition::is_empty(PAYLOAD_USER_ID),
            Condition::is_null(PAYLOAD_USER_ID),
            Condition::matches(PAYLOAD_USER_ID, String::new()),
        ]),
    }
}

/// A point id as text: the lowercase UUID, or the decimal number.
fn point_id_text(id: &PointId) -> String {
    match &id.point_id_options {
        Some(PointIdOptions::Uuid(uuid)) => uuid.to_ascii_lowercase(),
        Some(PointIdOptions::Num(num)) => num.to_string(),
        None => String::new(),
    }
}

fn point_id_from_text(text: &str) -> PointId {
    match text.parse::<u64>() {
        Ok(num) => PointId::from(num),
        Err(_) => PointId::from(text.to_string()),
    }
}

fn optional_string(payload: &HashMap<String, QdrantValue>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value_string(value).ok())
        .filter(|value| !value.trim().is_empty())
}

impl QdrantVectorStore {
    /// The collection's embedding identity WITHOUT any write (unlike
    /// [`guard_identity`], which may record an identity) and, unless `probe` is given, without any
    /// embedding call. `dimensions` (when given) must equal the collection's vector size.
    pub async fn inspect_identity(
        &self,
        identity: Option<&EmbeddingIdentity>,
        dimensions: Option<usize>,
        probe: Option<&dyn EmbeddingProvider>,
    ) -> VectorIdentityStatus {
        if let Some(dimensions) = dimensions {
            match check_vector_size(&self.client, &self.collection_name, dimensions).await {
                Ok(()) => {}
                Err(MemcoreError::ValidationError(_)) => return VectorIdentityStatus::Mismatch,
                Err(_) => return VectorIdentityStatus::Unavailable,
            }
        }
        let marker = identity_collection_name(&self.collection_name);
        let recorded = match self.client.collection_exists(&marker).await {
            Ok(recorded) => recorded,
            Err(_) => return VectorIdentityStatus::Unavailable,
        };
        if !recorded {
            return VectorIdentityStatus::Unrecorded;
        }
        let Some(identity) = identity else {
            return VectorIdentityStatus::Unavailable;
        };
        let payload = match self
            .client
            .get_points(
                GetPointsBuilder::new(&marker, vec![IDENTITY_POINT.into()]).with_payload(true),
            )
            .await
        {
            Ok(points) => points.result.into_iter().next().map(|point| point.payload),
            Err(_) => return VectorIdentityStatus::Unavailable,
        };
        let recorded_hash = payload
            .as_ref()
            .and_then(|payload| optional_string(payload, "identity_hash"));
        if recorded_hash.as_deref() != Some(identity.semantic_hash().as_str()) {
            return VectorIdentityStatus::Mismatch;
        }
        if let Some(probe) = probe
            && let Some(stored) = payload
                .as_ref()
                .and_then(|payload| payload_vector(payload, "probe_vector"))
        {
            match probe_vector(probe, identity.dimensions).await {
                Ok(fresh) if cosine(&stored, &fresh) >= FINGERPRINT_MIN_COSINE => {}
                Ok(_) => return VectorIdentityStatus::Mismatch,
                Err(_) => return VectorIdentityStatus::Unavailable,
            }
        }
        VectorIdentityStatus::Verified
    }
}

/// The companion collection holding a store's embedding identity record.
pub fn identity_collection_name(collection_name: &str) -> String {
    format!("{collection_name}__embedding_identity")
}

const IDENTITY_POINT: u64 = 1;

async fn check_vector_size(
    client: &Qdrant,
    collection_name: &str,
    dimensions: usize,
) -> MemcoreResult<()> {
    let info = client
        .collection_info(collection_name)
        .await
        .map_err(|error| storage_error("failed to read qdrant collection info", error))?;
    let size = info
        .result
        .and_then(|info| info.config)
        .and_then(|config| config.params)
        .and_then(|params| params.vectors_config)
        .and_then(|vectors| vectors.config)
        .and_then(|config| match config {
            vectors_config::Config::Params(params) => Some(params.size),
            vectors_config::Config::ParamsMap(_) => None,
        });
    match size {
        Some(size) if size as usize == dimensions => Ok(()),
        Some(size) => Err(MemcoreError::ValidationError(format!(
            "qdrant collection {collection_name} stores {size}-dimensional vectors but the \
             configured embedding provider produces {dimensions}; refusing to use it (no implicit \
             reindex)"
        ))),
        None => Err(MemcoreError::ValidationError(format!(
            "qdrant collection {collection_name} has no single unnamed vector configuration; \
             refusing to use it"
        ))),
    }
}

/// The fixed text whose embedding is a store's identity fingerprint.
pub const IDENTITY_PROBE_TEXT: &str =
    "memcore embedding identity probe v1: the quick brown fox jumps over the lazy dog.";
/// Minimum cosine between a stored and a fresh embedding of the same text for "same model".
pub const FINGERPRINT_MIN_COSINE: f32 = 0.999;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

/// The probe embedding, checked for size and finite values.
async fn probe_vector(probe: &dyn EmbeddingProvider, dimensions: usize) -> MemcoreResult<Vec<f32>> {
    let vector = probe.embed_text(IDENTITY_PROBE_TEXT).await?;
    if vector.len() != dimensions || vector.iter().any(|v| !v.is_finite()) {
        return Err(MemcoreError::ValidationError(format!(
            "the embedding probe returned {} values (expected {dimensions} finite values); \
             refusing to use this embedding endpoint",
            vector.len()
        )));
    }
    Ok(vector)
}

fn payload_vector(payload: &HashMap<String, QdrantValue>, key: &str) -> Option<Vec<f32>> {
    let list = match &payload.get(key)?.kind {
        Some(Kind::ListValue(list)) => list,
        _ => return None,
    };
    list.values
        .iter()
        .map(|value| match value.kind {
            Some(Kind::DoubleValue(v)) => Some(v as f32),
            Some(Kind::IntegerValue(v)) => Some(v as f32),
            _ => None,
        })
        .collect()
}

async fn write_identity_record(
    client: &Qdrant,
    collection_name: &str,
    identity: &EmbeddingIdentity,
    fingerprint: Option<&[f32]>,
) -> MemcoreResult<()> {
    let marker = identity_collection_name(collection_name);
    if !client
        .collection_exists(&marker)
        .await
        .map_err(|error| storage_error("failed to check qdrant identity record", error))?
    {
        client
            .create_collection(
                CreateCollectionBuilder::new(&marker)
                    .vectors_config(VectorParamsBuilder::new(1, Distance::Dot)),
            )
            .await
            .map_err(|error| storage_error("failed to create qdrant identity record", error))?;
    }
    let mut record = serde_json::json!({
        "identity_hash": identity.semantic_hash(),
        "identity_summary": summary(identity),
        "identity": serde_json::to_string(identity).unwrap_or_default(),
        "dimensions": identity.dimensions as i64,
        "collection": collection_name,
    });
    if let Some(vector) = fingerprint {
        record["probe_vector"] = serde_json::json!(vector);
        record["probe_text_version"] = serde_json::json!("v1");
    }
    let payload = Payload::try_from(record)
        .map_err(|error| storage_error("failed to build qdrant identity record", error))?;
    client
        .upsert_points(
            UpsertPointsBuilder::new(
                &marker,
                vec![PointStruct::new(IDENTITY_POINT, vec![1.0], payload)],
            )
            .wait(true),
        )
        .await
        .map_err(|error| storage_error("failed to write qdrant identity record", error))?;
    Ok(())
}

/// Fails closed when the store does not match the configured embedding provider.
/// `probe` (the configured provider; `None` = `MEMCORE_PROVIDER_PROBE=off`)
/// enables the vector-level checks. Nothing here writes a vector point.
///
/// - Recorded store: the identity hash must match. With a probe, the fingerprint (the embedding of
///   [`IDENTITY_PROBE_TEXT`] stored with the record) must still have cosine >= 0.999; a record
///   without a fingerprint gets one, since the identity already matched.
/// - Unrecorded, empty store: a provider with an identity records it (with a fingerprint when
///   probing).
/// - Unrecorded, non-empty (legacy) store: a local provider is refused (no implicit reindex). A
///   remote provider must PROVE compatibility: one stored point's own text is re-embedded and must
///   reproduce its stored vector (cosine >= 0.999); otherwise the store is refused. Nothing is
///   written to a legacy store: it is never silently rebound. With the probe off the store is used
///   as before, with a warning.
async fn guard_identity(
    client: &Qdrant,
    collection_name: &str,
    identity: Option<&EmbeddingIdentity>,
    probe: Option<&dyn EmbeddingProvider>,
) -> MemcoreResult<()> {
    let marker = identity_collection_name(collection_name);
    let recorded = client
        .collection_exists(&marker)
        .await
        .map_err(|error| storage_error("failed to check qdrant identity record", error))?;
    let current = identity.map(EmbeddingIdentity::semantic_hash);
    if recorded {
        let points = client
            .get_points(
                GetPointsBuilder::new(&marker, vec![IDENTITY_POINT.into()]).with_payload(true),
            )
            .await
            .map_err(|error| storage_error("failed to read qdrant identity record", error))?;
        let payload = points.result.into_iter().next().map(|point| point.payload);
        let field = |key: &str| {
            payload
                .as_ref()
                .and_then(|p| p.get(key))
                .and_then(|value| match &value.kind {
                    Some(Kind::StringValue(text)) => Some(text.clone()),
                    _ => None,
                })
        };
        let recorded_hash = field("identity_hash");
        if recorded_hash.is_none() || recorded_hash != current {
            return Err(MemcoreError::ValidationError(format!(
                "qdrant collection {collection_name} was built with embedding identity {} ({}); \
                 the configured provider is {} ({}); refusing to mix vectors (no implicit reindex)",
                recorded_hash.as_deref().unwrap_or("unreadable"),
                field("identity_summary").unwrap_or_default(),
                current.as_deref().unwrap_or("unknown"),
                identity.map(summary).unwrap_or_default(),
            )));
        }
        let (Some(identity), Some(probe)) = (identity, probe) else {
            return Ok(());
        };
        let fresh = probe_vector(probe, identity.dimensions).await?;
        match payload
            .as_ref()
            .and_then(|p| payload_vector(p, "probe_vector"))
        {
            Some(stored) => {
                let similarity = cosine(&stored, &fresh);
                if similarity < FINGERPRINT_MIN_COSINE {
                    return Err(MemcoreError::ValidationError(format!(
                        "qdrant collection {collection_name}: the embedding endpoint no longer \
                         reproduces the store's fingerprint (cosine {similarity:.4} < \
                         {FINGERPRINT_MIN_COSINE}) although the configured identity matches; the \
                         model behind it changed. Refusing to mix vectors; migrate to a new collection"
                    )));
                }
            }
            None => write_identity_record(client, collection_name, identity, Some(&fresh)).await?,
        }
        return Ok(());
    }
    let Some(identity) = identity else {
        return Ok(());
    };
    let existing = client
        .count(CountPointsBuilder::new(collection_name).exact(true))
        .await
        .map_err(|error| storage_error("failed to count qdrant points", error))?
        .result
        .map(|count| count.count)
        .unwrap_or(0);
    if existing == 0 {
        let fingerprint = match probe {
            Some(probe) => Some(probe_vector(probe, identity.dimensions).await?),
            None => None,
        };
        return write_identity_record(client, collection_name, identity, fingerprint.as_deref())
            .await;
    }
    if identity.is_local() {
        return Err(MemcoreError::ValidationError(format!(
            "qdrant collection {collection_name} holds {existing} vectors with no recorded \
             embedding identity; the local provider {} refuses an unrecorded store (no implicit \
             reindex)",
            summary(identity)
        )));
    }
    let Some(probe) = probe else {
        tracing::warn!(
            collection = collection_name,
            "legacy qdrant collection without an embedding identity record used UNVERIFIED \
             (MEMCORE_PROVIDER_PROBE=off)"
        );
        return Ok(());
    };
    // Legacy store: prove the configured provider reproduces a stored vector from its own text.
    let sample = client
        .scroll(
            ScrollPointsBuilder::new(collection_name)
                .limit(1)
                .with_payload(true)
                .with_vectors(true),
        )
        .await
        .map_err(|error| storage_error("failed to sample a legacy qdrant point", error))?
        .result
        .into_iter()
        .next();
    let Some(sample) = sample else {
        return Ok(());
    };
    let text = map_string(&sample.payload, PAYLOAD_CONTENT)?;
    let stored: Option<Vec<f32>> =
        sample
            .vectors
            .and_then(|vectors| match vectors.vectors_options {
                Some(qdrant_client::qdrant::vectors_output::VectorsOptions::Vector(vector)) => {
                    match vector.into_vector() {
                        qdrant_client::qdrant::vector_output::Vector::Dense(dense) => {
                            Some(dense.data)
                        }
                        _ => None,
                    }
                }
                _ => None,
            });
    let Some(stored) = stored else {
        return Err(MemcoreError::ValidationError(format!(
            "qdrant collection {collection_name}: could not read a stored vector to verify the \
             embedding provider; refusing an unrecorded store"
        )));
    };
    let fresh = probe.embed_text(&text).await?;
    let similarity = cosine(&stored, &fresh);
    if similarity < FINGERPRINT_MIN_COSINE {
        return Err(MemcoreError::ValidationError(format!(
            "qdrant collection {collection_name} has no recorded embedding identity and the \
             configured provider {} does not reproduce its stored vectors (cosine {similarity:.4} < \
             {FINGERPRINT_MIN_COSINE}); refusing to mix embedding spaces. Use a new collection \
             (explicit migration) or the provider that built this store",
            summary(identity)
        )));
    }
    tracing::info!(
        collection = collection_name,
        "legacy qdrant collection verified compatible with the configured embedding provider \
         (identity not recorded; the store is not modified)"
    );
    Ok(())
}

fn summary(identity: &EmbeddingIdentity) -> String {
    format!(
        "{} {}{} {}d",
        identity.provider,
        identity.model_id,
        identity
            .revision
            .as_deref()
            .map(|r| format!("@{}", &r[..r.len().min(12)]))
            .unwrap_or_default(),
        identity.dimensions
    )
}

async fn ensure_collection(
    client: &Qdrant,
    collection_name: &str,
    dimensions: usize,
) -> MemcoreResult<()> {
    let exists = client
        .collection_exists(collection_name)
        .await
        .map_err(|error| storage_error("failed to check qdrant collection", error))?;

    if exists {
        return Ok(());
    }

    client
        .create_collection(
            CreateCollectionBuilder::new(collection_name).vectors_config(VectorParamsBuilder::new(
                dimensions as u64,
                Distance::Cosine,
            )),
        )
        .await
        .map_err(|error| storage_error("failed to create qdrant collection", error))?;

    Ok(())
}

async fn ensure_payload_indexes(
    client: &Qdrant,
    collection_name: &str,
    fields: &[&str],
) -> MemcoreResult<PayloadIndexReport> {
    let info = client
        .collection_info(collection_name)
        .await
        .map_err(|error| storage_error("failed to read qdrant payload schema", error))?;
    let schema = info
        .result
        .map(|info| info.payload_schema)
        .unwrap_or_default();

    let mut report = PayloadIndexReport::default();
    for &field in fields {
        match schema.get(field) {
            Some(existing) if existing.data_type == PayloadSchemaType::Keyword as i32 => {
                report.present.push(field.to_string());
            }
            Some(existing) => {
                let found = PayloadSchemaType::try_from(existing.data_type)
                    .map(|kind| kind.as_str_name())
                    .unwrap_or("unknown");
                return Err(MemcoreError::ValidationError(format!(
                    "qdrant collection {collection_name} has a {found} index on payload field \
                     {field}, expected Keyword; refusing to replace it (set \
                     MEMCORE_QDRANT_ENSURE_INDEXES=false to skip the index check)"
                )));
            }
            None => {
                client
                    .create_field_index(
                        CreateFieldIndexCollectionBuilder::new(
                            collection_name,
                            field,
                            FieldType::Keyword,
                        )
                        .wait(true),
                    )
                    .await
                    .map_err(|error| {
                        storage_error(
                            format!(
                                "failed to create qdrant payload index {field} on {collection_name}"
                            ),
                            error,
                        )
                    })?;
                report.created.push(field.to_string());
            }
        }
    }
    Ok(report)
}

#[async_trait]
impl VectorStore for QdrantVectorStore {
    async fn upsert_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
    ) -> MemcoreResult<()> {
        self.check_record(tenant, &record)?;
        self.delete_matching(tenant_fact_filter(tenant, record.fact_id))
            .await?;
        self.upsert_point(record).await
    }

    async fn write_vector(
        &self,
        tenant: &TenantContext,
        record: VectorRecord,
        intent: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        match (self.write_mode, intent) {
            // A replacement, in both modes: reuse the fact's point, delete only observed extras
            // (the earlier delete-then-new-random-id replacement raced).
            (_, VectorWriteIntent::Replace) => self.replace_reusing_point(tenant, record).await,
            // Legacy behaviour for fresh facts and episodes: filtered pre-delete + upsert.
            (QdrantWriteMode::Legacy, _) => self.upsert_vector(tenant, record).await,
            // A stable-id claim that does not hold is not trusted: replace instead.
            (QdrantWriteMode::Optimized, VectorWriteIntent::StablePointId)
                if record.id != record.fact_id =>
            {
                self.replace_reusing_point(tenant, record).await
            }
            // Fresh fact / stable-id episode: no other point of the fact can exist, so the upsert
            // alone keeps one point per fact; a replayed upsert of the same point id overwrites.
            (QdrantWriteMode::Optimized, _) => {
                self.check_record(tenant, &record)?;
                self.upsert_point(record).await
            }
        }
    }

    async fn search_vectors(
        &self,
        query: VectorSearchQuery,
    ) -> MemcoreResult<Vec<VectorSearchResult>> {
        if query.embedding.len() != self.dimensions {
            return Err(MemcoreError::ValidationError(format!(
                "query embedding length {} does not match configured dimensions {}",
                query.embedding.len(),
                self.dimensions
            )));
        }

        // Unset -> no count, the historical request.
        // The count and the search are two requests: a write between them can move the tenant across
        // the bound, which only changes the path of this one search (both paths filter on the tenant).
        let path = self.search_path(&query.tenant).await?;
        match path {
            SearchPath::Exact => &self.search_policy.exact,
            SearchPath::Approximate => &self.search_policy.approximate,
        }
        .fetch_add(1, Ordering::Relaxed);

        // metadata_filter intentionally ignored in this phase.
        let response = self
            .client
            .query(self.search_request(&query, path == SearchPath::Exact))
            .await
            .map_err(|error| storage_error("failed to query qdrant vectors", error))?;

        let mut results = Vec::with_capacity(response.result.len());
        for point in response.result {
            results.push(scored_point_to_result(point)?);
        }

        results.truncate(query.limit);
        Ok(results)
    }

    async fn delete_by_fact_id(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<()> {
        let filter = tenant_fact_filter(tenant, fact_id);
        if self.count_matching(filter.clone()).await? == 0 {
            return Err(MemcoreError::NotFound(format!(
                "vector record not found for fact: {fact_id}"
            )));
        }

        self.delete_matching(filter).await
    }

    async fn delete_by_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        self.delete_matching(tenant_filter(tenant)).await
    }

    async fn fact_vector_points(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        let mut out = Vec::new();
        for point in self.fact_points(tenant, fact_id, true).await? {
            out.push(VectorPointState {
                point_id: point.id.as_ref().map(point_id_key).unwrap_or_default(),
                content: map_string(&point.payload, PAYLOAD_CONTENT)?,
                memory_type: memory_type_from_str(&map_string(
                    &point.payload,
                    PAYLOAD_MEMORY_TYPE,
                )?)?,
            });
        }
        Ok(Some(out))
    }

    /// Qdrant's own paging: points in id order; `cursor` is the `next_page_offset` the previous
    /// page returned (opaque to the caller). Payload only, no vectors.
    async fn scroll_points(
        &self,
        scope: &PointScope,
        cursor: Option<String>,
        limit: usize,
    ) -> MemcoreResult<PointPage> {
        let limit = u32::try_from(limit.max(1)).unwrap_or(u32::MAX);
        let mut request = ScrollPointsBuilder::new(&self.collection_name)
            .filter(scope_filter(scope))
            .limit(limit)
            .with_payload(true)
            .with_vectors(false);
        if let Some(cursor) = cursor {
            request = request.offset(point_id_from_text(&cursor));
        }
        let response = self
            .client
            .scroll(request)
            .await
            .map_err(|error| storage_error("failed to scroll qdrant points", error))?;
        let metadata_only = matches!(scope, PointScope::Unattributed);
        let points = response
            .result
            .into_iter()
            .map(|point| StoredPoint {
                point_id: point.id.as_ref().map(point_id_text).unwrap_or_default(),
                entity_id: optional_string(&point.payload, PAYLOAD_FACT_ID),
                org_id: optional_string(&point.payload, PAYLOAD_ORG_ID),
                user_id: optional_string(&point.payload, PAYLOAD_USER_ID),
                content: if metadata_only {
                    None
                } else {
                    optional_string(&point.payload, PAYLOAD_CONTENT)
                },
                memory_type: optional_string(&point.payload, PAYLOAD_MEMORY_TYPE)
                    .and_then(|value| memory_type_from_str(&value).ok()),
            })
            .collect();
        Ok(PointPage {
            points,
            next_cursor: response.next_page_offset.as_ref().map(point_id_text),
        })
    }

    async fn count_points(&self, scope: &PointScope) -> MemcoreResult<u64> {
        self.count_matching(scope_filter(scope)).await
    }

    /// One filtered delete: the listed ids AND the exact tenant payload, so no other tenant's
    /// point (and no point without tenant payload) can match.
    async fn delete_points(
        &self,
        tenant: &TenantContext,
        point_ids: &[String],
    ) -> MemcoreResult<()> {
        if point_ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<PointId> = point_ids.iter().map(|id| point_id_from_text(id)).collect();
        self.delete_matching(Filter::must([
            Condition::has_id(ids),
            Condition::matches(PAYLOAD_ORG_ID, tenant.org_id.clone()),
            Condition::matches(PAYLOAD_USER_ID, tenant.user_id.clone()),
        ]))
        .await
    }
}

/// A total order on point ids (uuid text, numeric ids zero-padded after a prefix).
fn point_id_key(id: &PointId) -> String {
    match &id.point_id_options {
        Some(PointIdOptions::Uuid(uuid)) => format!("u:{}", uuid.to_ascii_lowercase()),
        Some(PointIdOptions::Num(num)) => format!("n:{num:020}"),
        None => String::new(),
    }
}

/// The exact Qdrant query a search sends, with and without the query-time hnsw_ef.
#[cfg(test)]
mod search_request_tests {
    use super::*;

    fn store(hnsw_ef: Option<u64>) -> QdrantVectorStore {
        // The client is lazy: building it opens no connection, and these tests send nothing.
        let client = Qdrant::from_url("http://127.0.0.1:1")
            .skip_compatibility_check()
            .build()
            .expect("client");
        QdrantVectorStore {
            client,
            collection_name: "facts".to_string(),
            dimensions: 4,
            write_mode: QdrantWriteMode::Optimized,
            search_hnsw_ef: None,
            exact_search_max_tenant_points: None,
            search_policy: Arc::default(),
            write_operations: Arc::default(),
        }
        .with_search_hnsw_ef(hnsw_ef)
    }

    fn query(memory_types: Option<Vec<MemoryType>>) -> VectorSearchQuery {
        VectorSearchQuery {
            tenant: TenantContext::new("org_a", "user_a").unwrap(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            limit: 50,
            memory_types,
            metadata_filter: None,
        }
    }

    #[test]
    fn unset_sends_the_pre_ar031_request_with_no_search_params() {
        let request = store(None).search_request(&query(None), false);
        assert!(request.params.is_none(), "{:?}", request.params);
        // The same request the earlier code built inline.
        let previous = QueryPointsBuilder::new("facts")
            .query(vec![0.1, 0.2, 0.3, 0.4])
            .filter(search_filter(&query(None)))
            .limit(50)
            .with_payload(true)
            .build();
        assert_eq!(request, previous);
    }

    #[test]
    fn hnsw_ef_128_adds_exactly_the_query_time_parameter() {
        let unset = store(None).search_request(&query(Some(vec![MemoryType::Preference])), false);
        let ef = store(Some(128)).search_request(&query(Some(vec![MemoryType::Preference])), false);
        let params = ef.params.expect("search params");
        assert_eq!(params.hnsw_ef, Some(128));
        assert_eq!(params, SearchParamsBuilder::default().hnsw_ef(128).build());
        // Everything else, tenant predicates and the memory-type filter included, is identical.
        let mut without = ef.clone();
        without.params = None;
        assert_eq!(without, unset);
        let filter = format!("{:?}", ef.filter);
        assert!(
            filter.contains("org_a") && filter.contains("user_a"),
            "{filter}"
        );
        assert_eq!(ef.limit, Some(50));
        assert_eq!(ef.collection_name, "facts");
    }

    /// The fixed candidate bound selects exact for 0, 1, 11,999 and
    /// 12,000 tenant points and approximate for 12,001.
    #[test]
    fn bounded_exact_decision_at_the_12000_boundary() {
        for count in [0, 1, 11_999, 12_000] {
            assert_eq!(search_path_for(count, 12_000), SearchPath::Exact, "{count}");
        }
        assert_eq!(search_path_for(12_001, 12_000), SearchPath::Approximate);
        assert_eq!(search_path_for(u64::MAX, 12_000), SearchPath::Approximate);
    }

    /// The exact request is the historical request plus exactly `params.exact = true`: same collection,
    /// vector, tenant + memory-type filter, limit and payload selection.
    #[test]
    fn exact_path_adds_only_params_exact() {
        let q = query(Some(vec![MemoryType::Preference]));
        let approximate = store(None).search_request(&q, false);
        let exact = store(None).search_request(&q, true);
        assert!(approximate.params.is_none());
        let params = exact.params.expect("search params");
        assert_eq!(params, SearchParamsBuilder::default().exact(true).build());
        assert_eq!(params.hnsw_ef, None);
        let mut without = exact.clone();
        without.params = None;
        assert_eq!(without, approximate);
    }

    /// Unset policy: no count is ever attempted (the client points at a closed port, so any request
    /// would fail) and the path is approximate.
    #[tokio::test]
    async fn unset_policy_sends_no_count() {
        let s = store(None);
        assert_eq!(s.exact_search_max_tenant_points(), None);
        let path = s
            .search_path(&query(None).tenant)
            .await
            .expect("no request is made");
        assert_eq!(path, SearchPath::Approximate);
        assert_eq!(s.search_policy_stats().tenant_counts, 0);
    }

    /// A count failure is an explicit storage error naming the count; the search is not attempted and
    /// there is no silent fallback to the approximate path.
    #[tokio::test]
    async fn count_failure_is_an_explicit_error_without_fallback() {
        let s = store(None).with_exact_search_max_tenant_points(Some(12_000));
        let error = s
            .search_vectors(query(None))
            .await
            .expect_err("closed port: the count must fail");
        let text = error.to_string();
        assert!(
            matches!(error, MemcoreError::StorageError(_)) && text.contains("bounded exact search"),
            "{text}"
        );
        let stats = s.search_policy_stats();
        assert_eq!((stats.tenant_counts, stats.tenant_count_failures), (1, 1));
        assert_eq!((stats.exact_searches, stats.approximate_searches), (0, 0));
    }

    /// A count that never answers is bounded by the caller's deadline (request deadlines drop the
    /// future, which cancels the in-flight count; the search is never sent).
    #[tokio::test]
    async fn hanging_count_is_bounded_by_the_request_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let client = Qdrant::from_url(&url)
            .skip_compatibility_check()
            .build()
            .expect("client");
        let s = QdrantVectorStore {
            client,
            collection_name: "facts".to_string(),
            dimensions: 4,
            write_mode: QdrantWriteMode::Optimized,
            search_hnsw_ef: None,
            exact_search_max_tenant_points: Some(12_000),
            search_policy: Arc::default(),
            write_operations: Arc::default(),
        };
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            s.search_vectors(query(None)),
        )
        .await;
        assert!(
            outcome.is_err(),
            "the deadline must cancel the hanging count"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(s.search_policy_stats().exact_searches, 0);
        drop(listener);
    }
}
