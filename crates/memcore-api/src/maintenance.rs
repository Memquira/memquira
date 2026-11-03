//! Deletion-only maintenance.
//!
//! When the configured fact collection's embedding identity no longer matches the configured
//! embedding provider, the API refuses to start (by design), so `DELETE
//! /api/v1/users/{user_id}` is unreachable. `memcore-maintenance forget-user` performs the same
//! erasure WITHOUT starting the API and WITHOUT an embedding provider:
//!
//! - it is an operator command (it needs the deployment's database and Qdrant settings), never an
//!   HTTP route, and runs only with an explicit organization, user and confirmation phrase;
//! - it opens only the configured SQL store and the two configured Qdrant collections, and only if
//!   they already exist (nothing is created; the SQL schema is validated, never migrated);
//! - it never builds an LLM or embedding provider, searches, upserts, re-embeds or imports, and it
//!   does not touch the identity checks of normal API startup;
//! - every delete is filtered to the exact (org, user); a protected collection is refused;
//! - each step's outcome and the postcondition (no active fact rows, episode rows, fact points or
//!   episode points left for the user) are reported; anything short of an empty postcondition is a
//!   failure, never success. Re-running is safe (every step is an idempotent filtered delete).
//!
//! This is not a fix for the general SQL / Qdrant consistency problem.

use std::sync::Arc;

use async_trait::async_trait;
use memcore_common::{MemcoreError, MemcoreResult};
#[cfg(feature = "qdrant")]
use memcore_config::EventBackend;
use memcore_config::{FactBackend, Settings, VectorBackend};
use memcore_core::ports::EntityStore;
use memcore_core::{EpisodeStore, FactStore, MemoryEventStore, TenantContext};
use serde::Serialize;

/// Protected collections (kept reference data). Maintenance never mutates them,
/// whatever `MEMCORE_PROTECTED_COLLECTIONS` says.
const BUILT_IN_PROTECTED_PREFIXES: &[&str] = &["memcore_phase", "mem0_phase0_c"];
/// Extra protected collections: comma-separated exact names, or `prefix*`.
pub const PROTECTED_COLLECTIONS_ENV: &str = "MEMCORE_PROTECTED_COLLECTIONS";
const EPISODE_COUNT_LIMIT: usize = 10_000_000;

/// The phrase the operator must repeat to confirm the deletion.
pub fn confirmation_phrase(org_id: &str, user_id: &str) -> String {
    format!("forget-user:{org_id}:{user_id}")
}

/// Validates the scope and the confirmation; returns the tenant to erase.
pub fn validated_scope(org_id: &str, user_id: &str, confirm: &str) -> MemcoreResult<TenantContext> {
    validate_identifiers(org_id, user_id)?;
    if confirm != confirmation_phrase(org_id, user_id) {
        return Err(MemcoreError::ValidationError(
            "confirmation does not match: pass --confirm forget-user:<org-id>:<user-id> for exactly this scope"
                .to_string(),
        ));
    }
    TenantContext::new(org_id, user_id)
}

/// The scope of `verify-vectors`. A dry run (the default) needs no
/// confirmation and accepts none; a repair needs `--confirm verify-vectors:<org>:<user>` for
/// exactly this tenant (a phrase for another tenant, or none, is refused).
pub fn validated_verify_scope(
    org_id: &str,
    user_id: &str,
    repair: bool,
    confirm: Option<&str>,
) -> MemcoreResult<(TenantContext, memcore_core::verify::VerifyOptions)> {
    use memcore_core::verify::{VerifyOptions, confirmation_phrase as verify_phrase};
    validate_identifiers(org_id, user_id)?;
    let tenant = TenantContext::new(org_id, user_id)?;
    let phrase = verify_phrase(&tenant);
    match (repair, confirm) {
        (false, None) => Ok((tenant, VerifyOptions::dry_run())),
        (false, Some(_)) => Err(MemcoreError::ValidationError(
            "--confirm is only valid with --repair (a dry run needs no confirmation)".to_string(),
        )),
        (true, Some(confirm)) if confirm == phrase => Ok((tenant, VerifyOptions::repair(confirm))),
        (true, _) => Err(MemcoreError::ValidationError(
            "confirmation does not match: pass --repair --confirm verify-vectors:<org-id>:<user-id> for exactly this scope"
                .to_string(),
        )),
    }
}

fn validate_identifiers(org_id: &str, user_id: &str) -> MemcoreResult<()> {
    for (name, value) in [("org", org_id), ("user", user_id)] {
        if value.trim().is_empty() || value != value.trim() {
            return Err(MemcoreError::ValidationError(format!(
                "--{name}-id must be non-empty with no surrounding whitespace"
            )));
        }
        if value.contains(['*', '%', '?']) || value.chars().any(char::is_control) {
            return Err(MemcoreError::ValidationError(format!(
                "--{name}-id must be one exact identifier (no wildcards or control characters)"
            )));
        }
    }
    Ok(())
}

/// True when `collection` must never be mutated by maintenance.
pub fn is_protected_collection(collection: &str, extra: &str) -> bool {
    BUILT_IN_PROTECTED_PREFIXES
        .iter()
        .any(|prefix| collection.starts_with(prefix))
        || extra
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|entry| match entry.strip_suffix('*') {
                Some(prefix) => collection.starts_with(prefix),
                None => collection == entry,
            })
}

/// Tenant-filtered delete and count on one vector collection.
#[async_trait]
pub trait MaintenanceVectors: Send + Sync {
    async fn count_user(&self, tenant: &TenantContext) -> MemcoreResult<u64>;
    async fn delete_user(&self, tenant: &TenantContext) -> MemcoreResult<()>;
}

#[cfg(feature = "qdrant")]
pub struct QdrantMaintenanceVectors(pub memcore_storage::QdrantVectorStore);

#[cfg(feature = "qdrant")]
#[async_trait]
impl MaintenanceVectors for QdrantMaintenanceVectors {
    async fn count_user(&self, tenant: &TenantContext) -> MemcoreResult<u64> {
        self.0.count_tenant_points(tenant).await
    }

    async fn delete_user(&self, tenant: &TenantContext) -> MemcoreResult<()> {
        use memcore_core::VectorStore;
        self.0.delete_by_user(tenant).await
    }
}

/// The stores one maintenance run works on. `None` vectors = the collection does not exist.
pub struct MaintenanceStores {
    pub facts: Arc<dyn FactStore>,
    pub episodes: Arc<dyn EpisodeStore>,
    pub entities: Arc<dyn EntityStore>,
    pub events: Option<Arc<dyn MemoryEventStore>>,
    pub fact_vectors: Option<Arc<dyn MaintenanceVectors>>,
    pub episode_vectors: Option<Arc<dyn MaintenanceVectors>>,
    /// The ingest journal (in-flight writes; before-images hold fact text).
    pub ingest_journal: Option<Arc<dyn memcore_core::ports::IngestJournalStore>>,
    /// What was opened, for the report (no credentials).
    pub description: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Remaining {
    /// `None` = could not be counted (the error is in the report).
    pub fact_rows: Option<u64>,
    pub episode_rows: Option<u64>,
    pub fact_points: Option<u64>,
    pub episode_points: Option<u64>,
    /// Audit events of the user other than content-free forget-user tombstones.
    pub audit_content_events: Option<u64>,
}

impl Remaining {
    pub fn is_empty(&self) -> bool {
        [
            self.fact_rows,
            self.episode_rows,
            self.fact_points,
            self.episode_points,
            self.audit_content_events,
        ]
        .iter()
        .all(|count| *count == Some(0))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub step: &'static str,
    /// `ok`, `failed`, `absent` (collection does not exist) or `skipped` (dry run).
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForgetUserReport {
    pub operation: &'static str,
    pub org_id: String,
    pub user_id: String,
    pub dry_run: bool,
    pub stores: serde_json::Value,
    pub before: Remaining,
    pub steps: Vec<StepReport>,
    pub after: Remaining,
    pub count_errors: Vec<String>,
    /// `deleted`, `nothing_to_delete`, `dry_run` or `incomplete`.
    pub outcome: &'static str,
    /// True only when every step succeeded (or had nothing to reach) and nothing is left.
    pub complete: bool,
}

/// Error text for the report: no connection strings or credentials.
fn scrub(error: &MemcoreError) -> String {
    let text = error.to_string();
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(char::is_whitespace) {
        if word.contains("://") && word.contains('@') {
            out.push_str("[redacted-url]");
            if word.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else {
            out.push_str(word);
        }
    }
    out.chars().take(400).collect()
}

async fn count(
    stores: &MaintenanceStores,
    tenant: &TenantContext,
    errors: &mut Vec<String>,
) -> Remaining {
    let mut note = |what: &str, result: MemcoreResult<u64>| match result {
        Ok(n) => Some(n),
        Err(error) => {
            errors.push(format!("{what}: {}", scrub(&error)));
            None
        }
    };
    let fact_rows = note(
        "fact rows",
        stores
            .facts
            .count_facts_by_user(tenant)
            .await
            .map(|n| n as u64),
    );
    let episode_rows = note(
        "episode rows",
        stores
            .episodes
            .list_episodes(tenant, EPISODE_COUNT_LIMIT)
            .await
            .map(|e| e.len() as u64),
    );
    let fact_points = match &stores.fact_vectors {
        Some(v) => note("fact points", v.count_user(tenant).await),
        None => Some(0),
    };
    let episode_points = match &stores.episode_vectors {
        Some(v) => note("episode points", v.count_user(tenant).await),
        None => Some(0),
    };
    let audit_content_events = match &stores.events {
        Some(events) => note(
            "audit events",
            events
                .count_user_audit_content(tenant)
                .await
                .map(|n| n as u64),
        ),
        None => Some(0),
    };
    Remaining {
        fact_rows,
        episode_rows,
        fact_points,
        episode_points,
        audit_content_events,
    }
}

fn step(name: &'static str, result: MemcoreResult<Option<u64>>) -> StepReport {
    match result {
        Ok(deleted) => StepReport {
            step: name,
            status: "ok",
            deleted,
            error: None,
        },
        Err(error) => StepReport {
            step: name,
            status: "failed",
            deleted: None,
            error: Some(scrub(&error)),
        },
    }
}

/// Runs the deletion (or, with `dry_run`, only the counts) for exactly `tenant`.
///
/// Order: vector points first, then SQL rows, so an interrupted run never leaves searchable
/// vectors whose rows are gone; every step runs even if an earlier one failed (each is an
/// independent filtered delete), and the postcondition is re-counted at the end.
pub async fn forget_user(
    stores: &MaintenanceStores,
    tenant: &TenantContext,
    dry_run: bool,
) -> ForgetUserReport {
    let mut count_errors = Vec::new();
    let before = count(stores, tenant, &mut count_errors).await;
    let mut steps = Vec::new();
    if dry_run {
        for name in [
            "fact_points",
            "episode_points",
            "fact_rows",
            "episode_rows",
            "entity_rows",
            "ingest_journal_rows",
            "audit_events",
        ] {
            steps.push(StepReport {
                step: name,
                status: "skipped",
                deleted: None,
                error: None,
            });
        }
    } else {
        for (name, vectors) in [
            ("fact_points", &stores.fact_vectors),
            ("episode_points", &stores.episode_vectors),
        ] {
            steps.push(match vectors {
                None => StepReport {
                    step: name,
                    status: "absent",
                    deleted: Some(0),
                    error: None,
                },
                Some(v) => step(name, v.delete_user(tenant).await.map(|()| None)),
            });
        }
        steps.push(step(
            "fact_rows",
            stores.facts.delete_user_data(tenant).await.map(|()| None),
        ));
        steps.push(step(
            "episode_rows",
            stores
                .episodes
                .delete_user_episodes(tenant)
                .await
                .map(|n| Some(n as u64)),
        ));
        steps.push(step(
            "entity_rows",
            stores
                .entities
                .delete_user_entities(tenant)
                .await
                .map(|n| Some(n as u64)),
        ));
        steps.push(match &stores.ingest_journal {
            None => StepReport {
                step: "ingest_journal_rows",
                status: "absent",
                deleted: Some(0),
                error: None,
            },
            Some(journal) => step(
                "ingest_journal_rows",
                journal
                    .delete_user_operations(tenant)
                    .await
                    .map(|n| Some(n as u64)),
            ),
        });
        // The user's audit events carry fact text; content-free tombstones are kept.
        steps.push(match &stores.events {
            None => StepReport {
                step: "audit_events",
                status: "absent",
                deleted: Some(0),
                error: None,
            },
            Some(events) => step(
                "audit_events",
                events
                    .erase_user_audit_content(tenant)
                    .await
                    .map(|n| Some(n as u64)),
            ),
        });
    }
    let after = count(stores, tenant, &mut count_errors).await;
    let steps_ok = steps.iter().all(|s| s.status != "failed");
    let complete = !dry_run && steps_ok && after.is_empty() && count_errors.is_empty();
    let outcome = if dry_run {
        "dry_run"
    } else if !complete {
        "incomplete"
    } else if before.is_empty() {
        "nothing_to_delete"
    } else {
        "deleted"
    };
    if complete && let Some(events) = &stores.events {
        let event = memcore_core::audit::build_forget_user_event(
            tenant,
            &Some("maintenance".to_string()),
            &None,
        );
        let recorded = events.record_event(tenant, event).await.map(|_| None);
        steps.push(step("audit_event", recorded));
    }
    ForgetUserReport {
        operation: "forget-user",
        org_id: tenant.org_id.clone(),
        user_id: tenant.user_id.clone(),
        dry_run,
        stores: stores.description.clone(),
        before,
        steps,
        after,
        count_errors,
        outcome,
        complete,
    }
}

/// Opens the configured stores for deletion only. Fails closed on an unsupported or unsafe
/// target: in-memory or missing SQLite file, mock or LanceDB backends, protected or identical
/// collection names, an unvalidated SQL schema.
pub async fn open_stores(settings: &Settings) -> MemcoreResult<MaintenanceStores> {
    let protected_extra = std::env::var(PROTECTED_COLLECTIONS_ENV).unwrap_or_default();
    open_stores_with(settings, &protected_extra).await
}

pub async fn open_stores_with(
    settings: &Settings,
    protected_extra: &str,
) -> MemcoreResult<MaintenanceStores> {
    let (fact_collection, episode_collection) = validated_target(settings, protected_extra)?;
    open_checked(settings, fact_collection, episode_collection).await
}

/// The checks every maintenance command makes before opening anything: persistent SQL, the Qdrant
/// vector backend, two distinct non-empty collection names, none protected.
fn validated_target(settings: &Settings, protected_extra: &str) -> MemcoreResult<(String, String)> {
    let unsupported = |what: String| {
        Err(MemcoreError::ValidationError(format!(
            "maintenance refused: {what}"
        )))
    };
    if settings.fact_backend == FactBackend::Mock {
        return unsupported("MEMCORE_FACT_BACKEND=mock has no persistent data".to_string());
    }
    match settings.vector_backend {
        VectorBackend::Qdrant => {}
        VectorBackend::Mock => {
            return unsupported(
                "MEMCORE_VECTOR_BACKEND=mock has no persistent vectors".to_string(),
            );
        }
        VectorBackend::LanceDb => {
            return unsupported(
                "deletion maintenance supports the qdrant vector backend only".to_string(),
            );
        }
    }
    let (fact_collection, episode_collection) = (
        settings.qdrant_collection.trim().to_string(),
        settings.qdrant_episode_collection.trim().to_string(),
    );
    if fact_collection.is_empty()
        || episode_collection.is_empty()
        || fact_collection == episode_collection
    {
        return unsupported(
            "the fact and episode collections must be two distinct, non-empty names".to_string(),
        );
    }
    for collection in [&fact_collection, &episode_collection] {
        if is_protected_collection(collection, protected_extra) {
            return unsupported(format!("collection {collection} is protected"));
        }
    }
    Ok((fact_collection, episode_collection))
}

/// What `verify-vectors` works on. Vectors `None` = the collection does not
/// exist (nothing is created).
#[cfg(feature = "qdrant")]
pub struct VerifyMaintenanceStores {
    pub facts: Arc<dyn FactStore>,
    pub episodes: Arc<dyn EpisodeStore>,
    pub fact_vectors: Option<Arc<memcore_storage::QdrantVectorStore>>,
    pub episode_vectors: Option<Arc<memcore_storage::QdrantVectorStore>>,
    /// Built only for a confirmed repair.
    pub embedding: Option<Arc<dyn memcore_core::EmbeddingProvider>>,
    pub fact_identity: memcore_core::ports::VectorIdentityStatus,
    pub episode_identity: memcore_core::ports::VectorIdentityStatus,
    /// What was opened, for the report (no credentials).
    pub description: serde_json::Value,
}

/// Opens the stores for `verify-vectors` with the same refusals as `forget-user`. A dry run builds
/// no usable provider (the configured identity is read from the provider object, with no network
/// call), opens the existing collections only, and inspects their identity without writing. A
/// repair builds the configured embedding provider (refusing embedding fallback), opens the
/// collections with its dimensions and verifies their identity and fingerprint, still without
/// writing an identity record.
#[cfg(feature = "qdrant")]
pub async fn open_verify_stores(
    settings: &Settings,
    repair: bool,
) -> MemcoreResult<VerifyMaintenanceStores> {
    let protected_extra = std::env::var(PROTECTED_COLLECTIONS_ENV).unwrap_or_default();
    let (fact_collection, episode_collection) = validated_target(settings, &protected_extra)?;
    let sql = match settings.fact_backend {
        FactBackend::Sqlite => sqlite_stores(&settings.database_url).await?,
        #[cfg(feature = "postgres")]
        FactBackend::Postgres => postgres_stores(settings).await?,
        #[cfg(not(feature = "postgres"))]
        FactBackend::Postgres => {
            return Err(MemcoreError::ValidationError(
                "maintenance refused: MEMCORE_FACT_BACKEND=postgres needs a build with the `postgres` feature"
                    .to_string(),
            ));
        }
        FactBackend::Mock => unreachable!("refused by validated_target"),
    };
    let (identity, dimensions) = crate::state::configured_embedding_identity(settings)?;
    let embedding = if repair {
        Some(crate::state::maintenance_embedding_provider(settings).await?)
    } else {
        None
    };
    let url = settings.qdrant_url.trim();
    if url.is_empty() {
        return Err(MemcoreError::ValidationError(
            "maintenance refused: MEMCORE_QDRANT_URL is required".to_string(),
        ));
    }
    // A dry run never writes, so the dimensions only matter for a repair's writes.
    let open_dimensions = if repair { dimensions } else { 1 };
    let open = |collection: String| async move {
        Ok::<_, MemcoreError>(
            memcore_storage::QdrantVectorStore::connect_existing(url, &collection, open_dimensions)
                .await?
                .map(Arc::new),
        )
    };
    let fact_vectors = open(fact_collection.clone()).await?;
    let episode_vectors = open(episode_collection.clone()).await?;
    let probe = embedding.as_deref();
    let fact_identity = inspect_identity(
        fact_vectors.as_deref(),
        identity.as_ref(),
        repair.then_some(dimensions),
        probe,
    )
    .await;
    let episode_identity = inspect_identity(
        episode_vectors.as_deref(),
        identity.as_ref(),
        repair.then_some(dimensions),
        probe,
    )
    .await;
    let description = serde_json::json!({
        "sql": sql.kind,
        "fact_collection": fact_collection,
        "fact_collection_exists": fact_vectors.is_some(),
        "episode_collection": episode_collection,
        "episode_collection_exists": episode_vectors.is_some(),
        "embedding_provider": if repair { "constructed (repair)" } else { "not constructed (dry run)" },
    });
    Ok(VerifyMaintenanceStores {
        facts: sql.facts,
        episodes: sql.episodes,
        fact_vectors,
        episode_vectors,
        embedding,
        fact_identity,
        episode_identity,
        description,
    })
}

#[cfg(feature = "qdrant")]
async fn inspect_identity(
    store: Option<&memcore_storage::QdrantVectorStore>,
    identity: Option<&memcore_core::EmbeddingIdentity>,
    dimensions: Option<usize>,
    probe: Option<&dyn memcore_core::EmbeddingProvider>,
) -> memcore_core::ports::VectorIdentityStatus {
    match store {
        None => memcore_core::ports::VectorIdentityStatus::Unavailable,
        Some(store) => store.inspect_identity(identity, dimensions, probe).await,
    }
}

#[cfg(feature = "qdrant")]
fn as_store(
    store: Option<&memcore_storage::QdrantVectorStore>,
) -> Option<&dyn memcore_core::VectorStore> {
    store.map(|store| store as &dyn memcore_core::VectorStore)
}

/// One `verify-vectors` run over the opened stores.
#[cfg(feature = "qdrant")]
pub async fn verify_vectors(
    stores: &VerifyMaintenanceStores,
    tenant: &TenantContext,
    options: &memcore_core::verify::VerifyOptions,
) -> memcore_core::verify::VerifyReport {
    let verify = memcore_core::verify::VerifyStores {
        facts: stores.facts.as_ref(),
        episodes: Some(stores.episodes.as_ref()),
        fact_vectors: as_store(stores.fact_vectors.as_deref()),
        episode_vectors: as_store(stores.episode_vectors.as_deref()),
        embedding: stores.embedding.as_deref(),
        fact_identity: stores.fact_identity,
        episode_identity: stores.episode_identity,
    };
    memcore_core::verify::verify_vectors(&verify, tenant, options).await
}

#[cfg(not(feature = "qdrant"))]
async fn open_checked(
    _settings: &Settings,
    _fact_collection: String,
    _episode_collection: String,
) -> MemcoreResult<MaintenanceStores> {
    Err(MemcoreError::ValidationError(
        "maintenance refused: MEMCORE_VECTOR_BACKEND=qdrant needs a build with the `qdrant` feature"
            .to_string(),
    ))
}

/// Opens the SQL store and the two existing collections (scope and protection already checked).
#[cfg(feature = "qdrant")]
async fn open_checked(
    settings: &Settings,
    fact_collection: String,
    episode_collection: String,
) -> MemcoreResult<MaintenanceStores> {
    let unsupported = |what: String| {
        Err(MemcoreError::ValidationError(format!(
            "maintenance refused: {what}"
        )))
    };
    let sql = match settings.fact_backend {
        FactBackend::Sqlite => sqlite_stores(&settings.database_url).await?,
        #[cfg(feature = "postgres")]
        FactBackend::Postgres => postgres_stores(settings).await?,
        #[cfg(not(feature = "postgres"))]
        FactBackend::Postgres => {
            return unsupported(
                "MEMCORE_FACT_BACKEND=postgres needs a build with the `postgres` feature"
                    .to_string(),
            );
        }
        FactBackend::Mock => unreachable!("refused above"),
    };
    let (facts, episodes, entities, sql_kind) = (sql.facts, sql.episodes, sql.entities, sql.kind);
    let ingest_journal = Some(sql.journal);
    let events: Option<Arc<dyn MemoryEventStore>> = match settings.event_backend {
        EventBackend::Mock => None,
        _ if same_sql(settings) => Some(sql.events),
        EventBackend::Sqlite => Some(sqlite_stores(&settings.database_url).await?.events),
        #[cfg(feature = "postgres")]
        EventBackend::Postgres => Some(postgres_stores(settings).await?.events),
        #[cfg(not(feature = "postgres"))]
        EventBackend::Postgres => {
            return unsupported(
                "MEMCORE_EVENT_BACKEND=postgres needs a build with the `postgres` feature"
                    .to_string(),
            );
        }
    };

    let (fact_vectors, episode_vectors) = {
        let url = settings.qdrant_url.trim();
        if url.is_empty() {
            return unsupported("MEMCORE_QDRANT_URL is required".to_string());
        }
        let open = |collection: String| async move {
            // Existing collection only, no identity check, no create (dimensions are not used for deletes).
            Ok::<_, MemcoreError>(
                memcore_storage::QdrantVectorStore::connect_existing(url, &collection, 1)
                    .await?
                    .map(|store| {
                        Arc::new(QdrantMaintenanceVectors(store)) as Arc<dyn MaintenanceVectors>
                    }),
            )
        };
        (
            open(fact_collection.clone()).await?,
            open(episode_collection.clone()).await?,
        )
    };
    let description = serde_json::json!({
        "sql": sql_kind,
        "events": match settings.event_backend { EventBackend::Mock => "none", EventBackend::Sqlite => "sqlite", EventBackend::Postgres => "postgres" },
        "fact_collection": fact_collection,
        "fact_collection_exists": fact_vectors.is_some(),
        "episode_collection": episode_collection,
        "episode_collection_exists": episode_vectors.is_some(),
        "embedding_provider": "not constructed",
    });
    Ok(MaintenanceStores {
        facts,
        episodes,
        entities,
        events,
        fact_vectors,
        episode_vectors,
        ingest_journal,
        description,
    })
}

#[cfg(feature = "qdrant")]
fn same_sql(settings: &Settings) -> bool {
    matches!(
        (&settings.fact_backend, &settings.event_backend),
        (FactBackend::Sqlite, EventBackend::Sqlite)
            | (FactBackend::Postgres, EventBackend::Postgres)
    )
}

#[cfg(feature = "qdrant")]
struct SqlStores {
    facts: Arc<dyn FactStore>,
    episodes: Arc<dyn EpisodeStore>,
    entities: Arc<dyn EntityStore>,
    journal: Arc<dyn memcore_core::ports::IngestJournalStore>,
    events: Arc<dyn MemoryEventStore>,
    kind: &'static str,
}

#[cfg(feature = "qdrant")]
fn schema_refused(error: MemcoreError) -> MemcoreError {
    MemcoreError::ValidationError(format!(
        "maintenance refused: SQL schema not valid: {error}"
    ))
}

#[cfg(feature = "qdrant")]
async fn sqlite_stores(database_url: &str) -> MemcoreResult<SqlStores> {
    let Some(path) = memcore_storage::sqlite_path_from_database_url(database_url)? else {
        return Err(MemcoreError::ValidationError(
            "maintenance refused: an in-memory SQLite database has nothing to maintain".to_string(),
        ));
    };
    if !path.is_file() {
        return Err(MemcoreError::ValidationError(format!(
            "maintenance refused: the SQLite database {} does not exist (nothing is created)",
            path.display()
        )));
    }
    let pool = memcore_storage::connect_sqlite_pool(database_url).await?;
    memcore_storage::check_sqlite_pool_startup(
        &pool,
        memcore_storage::StorageMigrationMode::ValidateOnly,
        true,
    )
    .await
    .map_err(schema_refused)?;
    Ok(SqlStores {
        facts: Arc::new(memcore_storage::SqliteFactStore::new(pool.clone())),
        episodes: Arc::new(memcore_storage::SqliteEpisodeStore::new(pool.clone())),
        entities: Arc::new(memcore_storage::SqliteEntityStore::new(pool.clone())),
        journal: Arc::new(memcore_storage::SqliteIngestJournalStore::new(pool.clone())),
        events: Arc::new(memcore_storage::SqliteMemoryEventStore::new(pool)),
        kind: "sqlite",
    })
}

#[cfg(all(feature = "postgres", feature = "qdrant"))]
async fn postgres_stores(settings: &Settings) -> MemcoreResult<SqlStores> {
    let url = settings
        .postgres_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            MemcoreError::ValidationError("MEMCORE_POSTGRES_URL is required".to_string())
        })?;
    let pool = memcore_storage::connect_postgres_pool(url).await?;
    memcore_storage::check_postgres_pool_startup(
        &pool,
        memcore_storage::StorageMigrationMode::ValidateOnly,
        true,
    )
    .await
    .map_err(schema_refused)?;
    Ok(SqlStores {
        facts: Arc::new(memcore_storage::PostgresFactStore::new(pool.clone())),
        episodes: Arc::new(memcore_storage::PostgresEpisodeStore::new(pool.clone())),
        entities: Arc::new(memcore_storage::PostgresEntityStore::new(pool.clone())),
        journal: Arc::new(memcore_storage::PostgresIngestJournalStore::new(
            pool.clone(),
        )),
        events: Arc::new(memcore_storage::PostgresMemoryEventStore::new(pool)),
        kind: "postgres",
    })
}

/// The evidence schema of `memcore-maintenance rollout-preflight`.
pub const ROLLOUT_PREFLIGHT_SCHEMA: &str = "memcore.ar023.rollout-preflight/v1";

/// A read-only inventory of the configured SQL store plus the
/// candidate's own verdict on the environment it will start with (configuration validation,
/// including `Settings::check_context_cache_policy`). Returns the JSON report and the exit code:
/// 0 PASS, 4 BLOCKED. Opens no vector store, builds no provider, runs no migration and writes
/// nothing (`memcore_storage::rollout_inventory`). The database is selected from the raw
/// environment (`MEMCORE_FACT_BACKEND`, `MEMCORE_DATABASE_URL` / `MEMCORE_POSTGRES_URL`) so an
/// invalid configuration still gets an inventory; its configuration verdict is then REFUSED.
pub async fn rollout_preflight(
    assessment: memcore_storage::rollout_inventory::Assessment,
) -> (serde_json::Value, i32) {
    use memcore_storage::rollout_inventory as inventory;

    let mut reasons: Vec<String> = Vec::new();
    let config = match memcore_config::load_settings() {
        Ok(settings) => match settings.check_context_cache_policy() {
            Ok(()) => serde_json::json!({
                "verdict": "PASS",
                "context_cache_backend": format!("{:?}", settings.context_cache_backend).to_ascii_lowercase(),
            }),
            Err(error) => serde_json::json!({ "verdict": "REFUSED", "error": scrub(&error) }),
        },
        Err(error) => serde_json::json!({ "verdict": "REFUSED", "error": scrub(&error) }),
    };
    if config["verdict"] != "PASS" {
        reasons.push("configuration: the candidate refuses this environment".to_string());
    }
    let backend = std::env::var("MEMCORE_FACT_BACKEND")
        .unwrap_or_else(|_| "sqlite".to_string())
        .trim()
        .to_ascii_lowercase();
    let read: MemcoreResult<(inventory::RawInventory, Vec<i64>)> = match backend.as_str() {
        "sqlite" => {
            let url = std::env::var("MEMCORE_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://./data/memcore.db".to_string());
            match memcore_storage::sqlite_path_from_database_url(&url) {
                Ok(Some(path)) => inventory::read_sqlite(&path).await.map(|raw| {
                    let versions = memcore_storage::sqlite_migrations()
                        .iter()
                        .map(|m| m.version)
                        .collect();
                    (raw, versions)
                }),
                Ok(None) => Err(MemcoreError::ValidationError(
                    "rollout inventory: an in-memory SQLite database has no deployment state"
                        .to_string(),
                )),
                Err(error) => Err(error),
            }
        }
        #[cfg(feature = "postgres")]
        "postgres" => match std::env::var("MEMCORE_POSTGRES_URL") {
            Ok(url) if !url.trim().is_empty() => {
                inventory::read_postgres(url.trim()).await.map(|raw| {
                    let versions = memcore_storage::postgres_migrations()
                        .iter()
                        .map(|m| m.version)
                        .collect();
                    (raw, versions)
                })
            }
            _ => Err(MemcoreError::ValidationError(
                "rollout inventory: MEMCORE_POSTGRES_URL is required".to_string(),
            )),
        },
        other => Err(MemcoreError::ValidationError(format!(
            "rollout inventory: unsupported MEMCORE_FACT_BACKEND {other:?} for this build"
        ))),
    };
    let mut report = serde_json::json!({
        "schema": ROLLOUT_PREFLIGHT_SCHEMA,
        "tool": {
            "binary": "memcore-maintenance",
            "version": env!("CARGO_PKG_VERSION"),
            "git_sha": option_env!("MEMCORE_BUILD_GIT_SHA").unwrap_or("unknown"),
        },
        "assessment": assessment,
        "config": config,
    });
    match read {
        Ok((raw, versions)) => {
            let inv = inventory::assess(raw, &versions, assessment);
            reasons.extend(inv.reasons.iter().cloned());
            if inv.outcome != "PASS" && inv.reasons.is_empty() {
                reasons.push("inventory: not complete".to_string());
            }
            report["inventory"] = serde_json::to_value(&inv).unwrap_or_default();
        }
        Err(error) => {
            reasons.push(format!("inventory: {}", scrub(&error)));
            report["inventory"] = serde_json::Value::Null;
        }
    }
    let pass = reasons.is_empty();
    report["outcome"] = serde_json::json!(if pass { "PASS" } else { "BLOCKED" });
    report["reasons"] = serde_json::json!(reasons);
    (report, if pass { 0 } else { 4 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_requires_exact_confirmation_and_identifiers() {
        assert!(validated_scope("org_a", "user_1", "forget-user:org_a:user_1").is_ok());
        for (org, user, confirm) in [
            ("org_a", "user_1", "forget-user:org_a:user_2"),
            ("org_a", "user_1", "yes"),
            ("", "user_1", "forget-user::user_1"),
            ("org_a", " user_1", "forget-user:org_a: user_1"),
            ("org_a", "*", "forget-user:org_a:*"),
            ("org_%", "user_1", "forget-user:org_%:user_1"),
        ] {
            assert!(
                validated_scope(org, user, confirm).is_err(),
                "{org} {user} {confirm}"
            );
        }
    }

    #[test]
    fn verify_scope_dry_run_by_default_and_repair_needs_the_exact_phrase() {
        use memcore_core::verify::VerifyMode;
        let (tenant, options) =
            validated_verify_scope("org_a", "u1", false, None).expect("dry run");
        assert_eq!(tenant.user_id, "u1");
        assert_eq!(options.mode, VerifyMode::DryRun);
        let (_, options) =
            validated_verify_scope("org_a", "u1", true, Some("verify-vectors:org_a:u1"))
                .expect("repair");
        assert!(matches!(options.mode, VerifyMode::Repair { .. }));
        for (repair, confirm) in [
            (true, None),
            (true, Some("verify-vectors:org_a:u2")),
            (true, Some("verify-vectors:org_b:u1")),
            (true, Some("forget-user:org_a:u1")),
            (true, Some("verify-vectors:org_a:u1 ")),
            (true, Some("verify-vectors:all")),
            (false, Some("verify-vectors:org_a:u1")),
        ] {
            assert!(
                validated_verify_scope("org_a", "u1", repair, confirm).is_err(),
                "{confirm:?}"
            );
        }
        assert!(validated_verify_scope("org_*", "u1", false, None).is_err());
        assert!(validated_verify_scope("org_a", "", false, None).is_err());
    }

    #[test]
    fn protected_collections() {
        assert!(is_protected_collection("memcore_phase5_p0_store", ""));
        assert!(is_protected_collection("mem0_phase0_c1", ""));
        assert!(is_protected_collection("ref_store", "other, ref_store"));
        assert!(is_protected_collection("frozen_x", "frozen_*"));
        assert!(!is_protected_collection("memcore_launch_idx", "frozen_*"));
        assert!(!is_protected_collection("memcore", ""));
    }

    #[test]
    fn scrub_removes_credentialed_urls() {
        let error = MemcoreError::StorageError(
            "failed to connect postgres://memcore:s3cret@db:5432/memcore now".to_string(),
        );
        let text = scrub(&error);
        assert!(!text.contains("s3cret"), "{text}");
        assert!(text.contains("[redacted-url]"));
    }
}
