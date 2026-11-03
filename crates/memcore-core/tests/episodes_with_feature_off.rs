//! Episode export and import do not depend on `MEMCORE_EPISODES_ENABLED`.
//!
//! Episodes are written through an engine with episodes on; export, import, forget-user and
//! retention then run through an engine on the same stores with episodes off (as the API wires it:
//! the episode store plus a vector store that is opened only if it exists and created only by an
//! explicit import). Restoring episodes never turns episode behaviour on. Every name is invented.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::{Duration, Utc};
use memcore_core::ports::{EpisodeStore, VectorSearchQuery};
use memcore_core::{
    AddMemoryInput, ApplyRetentionInput, EpisodeComponents, EpisodeLifecycle, ExportUserDataInput,
    ForgetUserInput, ImportMode, ImportUserDataInput, MemoryEngine, MemoryMessage, MemorySession,
    MessageRole, RetentionPolicy, SearchEpisodesInput, TenantContext, USER_EXPORT_FORMAT_VERSION,
    USER_EXPORT_FORMAT_VERSION_V2, UserMemoryExport, VectorStore,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider};
use memcore_storage::{LazyVectorStore, MockEpisodeStore, MockFactStore, MockVectorStore};
use serde_json::json;

fn tenant(user: &str) -> TenantContext {
    TenantContext::new("org_episodes_off", user).unwrap()
}

/// Shared stores. The episode vectors sit behind a lazy store that counts how often its collection
/// was created, and "exists" only once the backing store was used.
struct World {
    facts: Arc<MockFactStore>,
    vectors: Arc<MockVectorStore>,
    episodes: Arc<MockEpisodeStore>,
    episode_backing: Arc<MockVectorStore>,
    collection_exists: Arc<AtomicBool>,
    creations: Arc<AtomicUsize>,
}

impl World {
    fn new() -> Self {
        Self {
            facts: Arc::new(MockFactStore::new()),
            vectors: Arc::new(MockVectorStore::new()),
            episodes: Arc::new(MockEpisodeStore::new()),
            episode_backing: Arc::new(MockVectorStore::new()),
            collection_exists: Arc::new(AtomicBool::new(false)),
            creations: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn lazy_episode_vectors(&self) -> Arc<dyn VectorStore> {
        let (open_backing, open_exists) =
            (self.episode_backing.clone(), self.collection_exists.clone());
        let (backing, exists, creations) = (
            self.episode_backing.clone(),
            self.collection_exists.clone(),
            self.creations.clone(),
        );
        Arc::new(LazyVectorStore::new(
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
        ))
    }

    fn base(&self) -> MemoryEngine {
        MemoryEngine::new(
            self.facts.clone(),
            self.vectors.clone(),
            Arc::new(MockLlmProvider::new()),
            Arc::new(MockEmbeddingProvider::new(64)),
        )
        .with_min_importance(0.0)
    }

    /// Episodes on (the collection exists from then on).
    fn on(&self) -> MemoryEngine {
        self.collection_exists.store(true, Ordering::SeqCst);
        self.base().with_episodes(Some(EpisodeComponents {
            store: self.episodes.clone(),
            vectors: self.episode_backing.clone(),
        }))
    }

    /// Episodes off, lifecycle storage wired as the API does.
    fn off(&self) -> MemoryEngine {
        self.base().with_episode_lifecycle(Some(EpisodeLifecycle {
            store: self.episodes.clone(),
            vectors: self.lazy_episode_vectors(),
        }))
    }

    async fn rows(&self, user: &TenantContext) -> Vec<memcore_core::Episode> {
        self.episodes.list_episodes(user, 100).await.unwrap()
    }

    async fn vectors_of(&self, user: &TenantContext) -> usize {
        self.episode_backing
            .search_vectors(VectorSearchQuery {
                tenant: user.clone(),
                embedding: vec![1.0 / 8.0; 64],
                limit: 100,
                memory_types: None,
                metadata_filter: None,
            })
            .await
            .unwrap()
            .len()
    }
}

async fn add(engine: &MemoryEngine, user: &TenantContext, id: &str, text: &str) {
    engine
        .add_memory(AddMemoryInput {
            tenant: user.clone(),
            messages: vec![MemoryMessage {
                name: Some("Ines".to_string()),
                message_id: Some(id.to_string()),
                ..MemoryMessage::new(MessageRole::User, text)
            }],
            metadata: json!({}),
            session: MemorySession {
                id: Some("S1".to_string()),
                timestamp: Some(Utc::now() - Duration::days(3)),
            },
        })
        .await
        .unwrap();
}

async fn export(engine: &MemoryEngine, user: &TenantContext) -> UserMemoryExport {
    engine
        .export_user_data(ExportUserDataInput {
            tenant: user.clone(),
            include_events: false,
            include_deleted: false,
        })
        .await
        .unwrap()
}

async fn import(
    engine: &MemoryEngine,
    user: &TenantContext,
    export: UserMemoryExport,
    mode: ImportMode,
) -> memcore_core::ImportUserDataOutput {
    engine
        .import_user_data(ImportUserDataInput {
            tenant: user.clone(),
            export,
            mode,
            restore_events: false,
            dry_run: false,
        })
        .await
        .unwrap()
}

/// Session, message ID, content, observed time and speaker of an episode.
type PortableEpisode = (
    Option<String>,
    Option<String>,
    String,
    Option<chrono::DateTime<Utc>>,
    String,
);

/// The fields an export carries that survive re-import (IDs and `recorded_at` are new on import).
fn portable(episodes: &[memcore_core::Episode]) -> Vec<PortableEpisode> {
    let mut rows: Vec<_> = episodes
        .iter()
        .map(|e| {
            (
                e.session_id.clone(),
                e.message_id.clone(),
                e.content.clone(),
                e.observed_at,
                e.speaker.clone().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

const LONG_A: &str = "I adopted a beagle called Biscuit last spring.";
const LONG_B: &str = "Biscuit learned to fetch the newspaper this week.";

/// After a restart with episodes off, the export still carries the user's
/// episodes; export → import → export preserves them.
#[tokio::test]
async fn export_carries_persisted_episodes_with_the_flag_off_and_round_trips() {
    let world = World::new();
    let ines = tenant("user_ines");
    let on = world.on();
    add(&on, &ines, "D1:1", LONG_A).await;
    add(&on, &ines, "D1:2", LONG_B).await;

    let off = world.off();
    assert!(!off.episodes_enabled());
    let first = export(&off, &ines).await;
    assert_eq!(first.format_version, USER_EXPORT_FORMAT_VERSION_V2);
    let exported = first
        .episodes
        .clone()
        .expect("episodes exported with the flag off");
    assert_eq!(exported.len(), 2);

    // A user without episodes keeps the unchanged v1 export.
    let nobody = export(&off, &tenant("user_nobody")).await;
    assert_eq!(nobody.format_version, USER_EXPORT_FORMAT_VERSION);
    assert!(nobody.episodes.is_none());

    import(&off, &ines, first, ImportMode::Replace).await;
    let second = export(&off, &ines).await;
    assert_eq!(portable(&second.episodes.unwrap()), portable(&exported));
}

/// Importing episodes with the flag off restores rows and vectors (as the
/// enabled path does, creating the episode collection on this first write), and episode behaviour
/// stays off: no new episodes on ingestion, search refused.
#[tokio::test]
async fn import_with_the_flag_off_restores_rows_and_vectors_without_enabling_episodes() {
    let source = World::new();
    let ines = tenant("user_ines");
    add(&source.on(), &ines, "D1:1", LONG_A).await;
    let payload = export(&source.on(), &ines).await;

    let world = World::new();
    let off = world.off();
    assert_eq!(world.creations.load(Ordering::SeqCst), 0);
    let output = import(&off, &ines, payload, ImportMode::Append).await;
    assert_eq!(
        (output.imported_episodes, output.skipped_episodes),
        (Some(1), Some(0))
    );
    assert_eq!(world.rows(&ines).await.len(), 1);
    assert!(
        world.rows(&ines).await[0].embedded,
        "restored with its vector"
    );
    assert_eq!(world.vectors_of(&ines).await, 1);
    assert_eq!(
        world.creations.load(Ordering::SeqCst),
        1,
        "created by the import"
    );

    // Episode behaviour is still off.
    add(&off, &ines, "D2:1", LONG_B).await;
    assert_eq!(
        world.rows(&ines).await.len(),
        1,
        "ingestion creates no episodes"
    );
    assert!(
        off.search_episodes(SearchEpisodesInput {
            tenant: ines.clone(),
            query: "beagle".to_string(),
            limit: 5,
        })
        .await
        .is_err()
    );
}

/// With the flag off, a replace import replaces the user's episodes, an
/// append import keeps the existing append semantics (client message IDs are not stored twice,
/// new ones are added), and another user is untouched.
#[tokio::test]
async fn replace_and_append_imports_with_the_flag_off() {
    let world = World::new();
    let (ines, other) = (tenant("user_ines"), tenant("user_other"));
    let on = world.on();
    add(&on, &ines, "D1:1", LONG_A).await;
    let payload = export(&on, &ines).await;
    add(&on, &ines, "D1:2", LONG_B).await;
    add(&on, &other, "D9:1", LONG_A).await;
    let off = world.off();

    // Replace: only the exported episode remains.
    import(&off, &ines, payload.clone(), ImportMode::Replace).await;
    let rows = world.rows(&ines).await;
    assert_eq!(
        rows.iter()
            .map(|e| e.message_id.clone().unwrap())
            .collect::<Vec<_>>(),
        vec!["D1:1"]
    );
    assert_eq!(world.vectors_of(&ines).await, 1);

    // Append the same export again: nothing doubles. Contract:
    // the export's facts are already stored with identical content, so the re-application is
    // rejected before any write (before R7 the facts were re-keyed into duplicates).
    let err = off
        .import_user_data(ImportUserDataInput {
            tenant: ines.clone(),
            export: payload.clone(),
            mode: ImportMode::Append,
            restore_events: false,
            dry_run: false,
        })
        .await
        .expect_err("re-application rejected");
    assert!(err.is_import_already_applied());
    assert_eq!(world.rows(&ines).await.len(), 1);
    // The episode path itself is unchanged: an episode-only export whose client message ID is
    // already stored adds nothing.
    let mut same_episodes = payload.clone();
    same_episodes.facts.clear();
    let output = import(&off, &ines, same_episodes, ImportMode::Append).await;
    assert_eq!(output.imported_episodes, Some(1));
    assert_eq!(world.rows(&ines).await.len(), 1);
    // Append an export holding a new message (episode only, so its already-applied facts do not
    // make R7 reject it): it is added.
    let mut newer = payload;
    newer.facts.clear();
    let mut episode = newer.episodes.as_ref().unwrap()[0].clone();
    episode.message_id = Some("D3:1".to_string());
    episode.content = LONG_B.to_string();
    newer.episodes = Some(vec![episode]);
    import(&off, &ines, newer, ImportMode::Append).await;
    assert_eq!(world.rows(&ines).await.len(), 2);

    assert_eq!(world.rows(&other).await.len(), 1, "other user untouched");
    assert_eq!(world.vectors_of(&other).await, 1);
}

/// Forget-user and retention keep deleting persisted episode rows and vectors with
/// the flag off, and never create the episode collection doing it.
#[tokio::test]
async fn forget_user_and_retention_with_the_flag_off() {
    let world = World::new();
    let (ines, other) = (tenant("user_ines"), tenant("user_other"));
    let on = world.on();
    add(&on, &ines, "D1:1", LONG_A).await;
    add(&on, &other, "D9:1", LONG_A).await;
    let off = world.off();
    off.forget_user(ForgetUserInput {
        tenant: ines.clone(),
    })
    .await
    .unwrap();
    assert!(world.rows(&ines).await.is_empty());
    assert_eq!(world.vectors_of(&ines).await, 0);
    assert_eq!(world.rows(&other).await.len(), 1);

    let aged = memcore_core::Episode {
        recorded_at: Utc::now() - Duration::days(400),
        message_id: Some("old".to_string()),
        ..world.rows(&other).await[0].clone()
    };
    let aged = memcore_core::Episode {
        id: uuid::Uuid::new_v4(),
        ..aged
    };
    world
        .episodes
        .insert_episodes(&other, vec![aged])
        .await
        .unwrap();
    let output = off
        .apply_retention(ApplyRetentionInput {
            tenant: other.clone(),
            policy: RetentionPolicy {
                enabled: true,
                fact_retention_days: Some(365),
                event_retention_days: None,
            },
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(output.episodes_deleted, Some(1));
    assert_eq!(world.rows(&other).await.len(), 1);

    // A world whose episode collection never existed: forget and retention create nothing.
    let fresh = World::new();
    let off = fresh.off();
    off.forget_user(ForgetUserInput {
        tenant: ines.clone(),
    })
    .await
    .unwrap();
    assert_eq!(fresh.creations.load(Ordering::SeqCst), 0);
    assert!(!fresh.collection_exists.load(Ordering::SeqCst));
}

/// Older exports without episodes still import with the flag off.
#[tokio::test]
async fn exports_without_episodes_still_import() {
    let world = World::new();
    let ines = tenant("user_ines");
    let payload = UserMemoryExport::new(ines.org_id.clone(), ines.user_id.clone(), vec![], vec![]);
    let output = import(&world.off(), &ines, payload, ImportMode::Append).await;
    assert_eq!(
        (output.imported_episodes, output.skipped_episodes),
        (None, None)
    );
    assert_eq!(world.creations.load(Ordering::SeqCst), 0);
}
