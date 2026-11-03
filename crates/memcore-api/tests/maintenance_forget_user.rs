//! Deletion-only maintenance against stores whose embedding identity no longer
//! matches the configured provider. Invented data only. Needs MEMCORE_TEST_QDRANT_URL (a disposable
//! Qdrant); skipped without it. Every collection is test-owned and removed on drop.

#[path = "../../memcore-storage/tests/support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use memcore_api::AppState;
use memcore_api::maintenance::{self, MaintenanceStores, MaintenanceVectors};
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_config::{EventBackend, FactBackend, Settings, VectorBackend};
use memcore_core::MemoryEventStore;
use memcore_core::{
    EmbeddingIdentity, Episode, EpisodeStore, Fact, FactStore, MemorySource, MemoryType,
    MessageRole, TenantContext,
};
use memcore_storage::{
    QdrantVectorStore, SqliteEntityStore, SqliteEpisodeStore, SqliteFactStore,
    SqliteMemoryEventStore, VectorRecord, VectorStore,
};
use qdrant_cleanup::TestCollections;
use uuid::Uuid;

const DIMS: usize = 8;
const SECRET_CONTENT: &str = "PRIVATE-MAINTENANCE-SENTINEL";
const TENANTS: [(&str, &str); 4] = [
    ("org_a", "user_1"),
    ("org_a", "user_2"),
    ("org_b", "user_1"), // same user id, other organization
    ("org_b", "user_2"),
];

fn qdrant_url() -> Option<String> {
    std::env::var("MEMCORE_TEST_QDRANT_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
}

fn tenant(org: &str, user: &str) -> TenantContext {
    TenantContext::new(org, user).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    db_url: String,
    db_path: std::path::PathBuf,
    url: String,
    facts: String,
    episodes: String,
}

/// Two orgs x two users: 2 facts + 2 episodes in SQL and one point per fact / episode, in
/// collections built by "old-model" (a recorded identity the configured provider does not match).
async fn seed(collections: &TestCollections, url: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("memcore.db");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );
    let facts_store = SqliteFactStore::connect(&db_url).await.unwrap();
    let episode_store = SqliteEpisodeStore::new(facts_store.pool());
    let facts = collections.name("ar048_facts");
    let episodes = format!("{facts}_episodes");
    let old = EmbeddingIdentity::remote("openai_compatible", "old-model", DIMS);
    let fact_vectors = QdrantVectorStore::connect_verified(url, &facts, DIMS, Some(&old), None)
        .await
        .unwrap();
    let episode_vectors =
        QdrantVectorStore::connect_verified(url, &episodes, DIMS, Some(&old), None)
            .await
            .unwrap();
    let now = Utc::now();
    for (org, user) in TENANTS {
        let t = tenant(org, user);
        for i in 0..2 {
            let id = Uuid::new_v4();
            let content = format!("{SECRET_CONTENT} {org} {user} fact {i}");
            let fact = Fact::new(
                id,
                org,
                user,
                MemoryType::Preference,
                content.clone(),
                None,
                MemorySource::UserMessage,
                0.9,
                0.5,
                None,
                None,
                now,
                now,
                serde_json::json!({}),
            )
            .unwrap();
            facts_store.insert_fact(&t, fact.clone()).await.unwrap();
            // The add event carries the fact text, as the API's add path records it.
            SqliteMemoryEventStore::new(facts_store.pool())
                .record_event(
                    &t,
                    memcore_core::audit::build_add_event(
                        &t,
                        &fact,
                        &None,
                        &None,
                        serde_json::json!({}),
                    ),
                )
                .await
                .unwrap();
            fact_vectors
                .upsert_vector(&t, record(id, org, user, &content))
                .await
                .unwrap();
            let episode_id = Uuid::new_v4();
            let episode = Episode {
                id: episode_id,
                org_id: org.into(),
                user_id: user.into(),
                session_id: Some("s1".into()),
                message_id: Some(format!("m{i}")),
                speaker: Some("Ana".into()),
                role: MessageRole::User,
                content: format!("{SECRET_CONTENT} {org} {user} episode {i}"),
                observed_at: None,
                recorded_at: now,
                embedded: true,
                metadata: serde_json::json!({}),
            };
            episode_store
                .insert_episodes(&t, vec![episode])
                .await
                .unwrap();
            episode_vectors
                .upsert_vector(&t, record(episode_id, org, user, "episode text"))
                .await
                .unwrap();
        }
    }
    Fixture {
        _dir: dir,
        db_url,
        db_path,
        url: url.to_string(),
        facts,
        episodes,
    }
}

fn record(id: Uuid, org: &str, user: &str, content: &str) -> VectorRecord {
    VectorRecord {
        id,
        fact_id: id,
        org_id: org.into(),
        user_id: user.into(),
        embedding: vec![0.25; DIMS],
        content: content.into(),
        memory_type: MemoryType::Preference,
        metadata: serde_json::json!({}),
    }
}

fn settings(f: &Fixture) -> Settings {
    let mut s = Settings::qdrant_with_collection(f.url.clone(), f.facts.clone());
    s.qdrant_episode_collection = f.episodes.clone();
    s.fact_backend = FactBackend::Sqlite;
    s.event_backend = EventBackend::Sqlite;
    s.vector_backend = VectorBackend::Qdrant;
    s.database_url = f.db_url.clone();
    s
}

/// (fact rows, episode rows, fact points, episode points, content-bearing audit events) for one
/// tenant, read directly.
async fn counts(f: &Fixture, t: &TenantContext) -> (u64, u64, u64, u64, u64) {
    let facts = SqliteFactStore::connect(&f.db_url).await.unwrap();
    let episodes = SqliteEpisodeStore::new(facts.pool());
    let fv = QdrantVectorStore::connect_existing(&f.url, &f.facts, DIMS)
        .await
        .unwrap()
        .unwrap();
    let ev = QdrantVectorStore::connect_existing(&f.url, &f.episodes, DIMS)
        .await
        .unwrap()
        .unwrap();
    (
        facts.count_facts_by_user(t).await.unwrap() as u64,
        episodes.list_episodes(t, 1000).await.unwrap().len() as u64,
        fv.count_tenant_points(t).await.unwrap(),
        ev.count_tenant_points(t).await.unwrap(),
        SqliteMemoryEventStore::new(facts.pool())
            .count_user_audit_content(t)
            .await
            .unwrap() as u64,
    )
}

fn run_binary(
    f: &Fixture,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> (i32, serde_json::Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_memcore-maintenance"));
    cmd.env_clear();
    for key in ["SYSTEMROOT", "PATH", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    // A deployment configured for a NEW embedding model whose endpoint is down: maintenance must
    // not need it (no provider is constructed, nothing is embedded).
    cmd.envs([
        ("MEMCORE_ENV", "development"),
        ("MEMCORE_FACT_BACKEND", "sqlite"),
        ("MEMCORE_EVENT_BACKEND", "sqlite"),
        ("MEMCORE_DATABASE_URL", f.db_url.as_str()),
        ("MEMCORE_VECTOR_BACKEND", "qdrant"),
        ("MEMCORE_QDRANT_URL", f.url.as_str()),
        ("MEMCORE_QDRANT_COLLECTION", f.facts.as_str()),
        ("MEMCORE_QDRANT_EPISODE_COLLECTION", f.episodes.as_str()),
        ("MEMCORE_LLM_PROVIDER", "openai"),
        ("MEMCORE_LLM_MODEL", "new-chat-model"),
        ("MEMCORE_EMBEDDING_PROVIDER", "openai"),
        ("MEMCORE_EMBEDDING_MODEL", "new-embedding-model"),
        ("OPENAI_BASE_URL", "http://127.0.0.1:9/v1"),
        ("OPENAI_API_KEY", "sk-INVENTED-maintenance-key"),
        ("MEMCORE_REAL_PROVIDER_CALLS_ENABLED", "true"),
    ]);
    cmd.envs(extra_env.iter().copied());
    let out = cmd.args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let json = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap_or(-1), json, stdout + &stderr)
}

#[tokio::test]
async fn ar048_maintenance_erases_one_user_from_identity_mismatched_stores() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collections = TestCollections::new(url.clone());
    let f = seed(&collections, &url).await;
    let target = tenant("org_a", "user_1");
    let before_others: Vec<_> = {
        let mut v = Vec::new();
        for (org, user) in &TENANTS[1..] {
            v.push(counts(&f, &tenant(org, user)).await);
        }
        v
    };
    assert_eq!(counts(&f, &target).await, (2, 2, 2, 2, 2));

    // 1. Normal API startup still refuses the mismatched fact collection (the mock provider's
    //    identity is not "old-model"); maintenance changes nothing about that.
    let refusal = match AppState::initialize(settings(&f)).await {
        Ok(_) => panic!("API startup must refuse the identity mismatch"),
        Err(error) => error.to_string(),
    };
    assert!(
        refusal.contains("identity"),
        "refused for another reason: {refusal}"
    );

    // 2. Wrong confirmation / scope: refused, nothing deleted.
    let (code, json, _) = run_binary(
        &f,
        &[
            "forget-user",
            "--org-id",
            "org_a",
            "--user-id",
            "user_1",
            "--confirm",
            "forget-user:org_a:user_2",
        ],
        &[],
    );
    assert_eq!((code, json["outcome"].as_str()), (2, Some("refused")));
    assert_eq!(counts(&f, &target).await, (2, 2, 2, 2, 2));

    // 3. Dry run: counts only.
    let confirm = "forget-user:org_a:user_1";
    let (code, json, _) = run_binary(
        &f,
        &[
            "forget-user",
            "--org-id",
            "org_a",
            "--user-id",
            "user_1",
            "--confirm",
            confirm,
            "--dry-run",
        ],
        &[],
    );
    assert_eq!((code, json["outcome"].as_str()), (0, Some("dry_run")));
    assert_eq!(json["before"]["fact_points"], 2);
    assert_eq!(counts(&f, &target).await, (2, 2, 2, 2, 2));

    // 4. The real run, through the binary, without a working embedding provider.
    let (code, json, output) = run_binary(
        &f,
        &[
            "forget-user",
            "--org-id",
            "org_a",
            "--user-id",
            "user_1",
            "--confirm",
            confirm,
        ],
        &[],
    );
    assert_eq!(code, 0, "{output}");
    assert_eq!(json["outcome"], "deleted");
    assert_eq!(json["complete"], true);
    assert_eq!(json["stores"]["embedding_provider"], "not constructed");
    assert_eq!(counts(&f, &target).await, (0, 0, 0, 0, 0));
    let mut after_others = Vec::new();
    for (org, user) in &TENANTS[1..] {
        after_others.push(counts(&f, &tenant(org, user)).await);
    }
    assert_eq!(
        after_others, before_others,
        "other tenants must be untouched"
    );
    for leaked in [SECRET_CONTENT, "sk-INVENTED-maintenance-key"] {
        assert!(!output.contains(leaked), "output leaked {leaked}");
    }
    // The collections still carry the old identity: API startup still refuses.
    assert!(AppState::initialize(settings(&f)).await.is_err());

    // 5. Repeat: safe, nothing left to delete.
    let (code, json, _) = run_binary(
        &f,
        &[
            "forget-user",
            "--org-id",
            "org_a",
            "--user-id",
            "user_1",
            "--confirm",
            confirm,
        ],
        &[],
    );
    assert_eq!(
        (code, json["outcome"].as_str()),
        (0, Some("nothing_to_delete"))
    );

    // 6. A protected target is refused before anything is opened for deletion.
    let (code, json, _) = run_binary(
        &f,
        &[
            "forget-user",
            "--org-id",
            "org_b",
            "--user-id",
            "user_1",
            "--confirm",
            "forget-user:org_b:user_1",
        ],
        &[("MEMCORE_PROTECTED_COLLECTIONS", f.facts.as_str())],
    );
    assert_eq!((code, json["outcome"].as_str()), (2, Some("refused")));
    assert!(json["error"].as_str().unwrap().contains("protected"));
    assert_eq!(
        counts(&f, &tenant("org_b", "user_1")).await,
        (2, 2, 2, 2, 2)
    );
    let mut protected = settings(&f);
    protected.qdrant_collection = "memcore_phase5_p0_store".into();
    assert!(maintenance::open_stores_with(&protected, "").await.is_err());

    // 7. A missing SQLite file is refused (nothing is created).
    let mut missing = settings(&f);
    missing.database_url = format!(
        "sqlite://{}/absent.db?mode=rwc",
        f._dir.path().to_string_lossy().replace('\\', "/")
    );
    assert!(maintenance::open_stores_with(&missing, "").await.is_err());
    assert!(!f._dir.path().join("absent.db").exists());
    let _ = &f.db_path;
}

/// Vectors whose deletes fail (Qdrant down mid-run); counts still work.
struct FailingDeletes(Arc<dyn MaintenanceVectors>);

#[async_trait]
impl MaintenanceVectors for FailingDeletes {
    async fn count_user(&self, t: &TenantContext) -> MemcoreResult<u64> {
        self.0.count_user(t).await
    }
    async fn delete_user(&self, _t: &TenantContext) -> MemcoreResult<()> {
        Err(MemcoreError::StorageError(
            "failed to delete qdrant points: connection refused (injected)".into(),
        ))
    }
}

#[tokio::test]
async fn ar048_partial_failures_are_reported_and_a_rerun_recovers() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let collections = TestCollections::new(url.clone());
    let f = seed(&collections, &url).await;
    let target = tenant("org_b", "user_2");
    let other = tenant("org_a", "user_2");
    let other_before = counts(&f, &other).await;

    // Qdrant failure: SQL rows are deleted, the points are not -> incomplete, never success.
    let good = maintenance::open_stores_with(&settings(&f), "")
        .await
        .unwrap();
    let stores = MaintenanceStores {
        fact_vectors: good
            .fact_vectors
            .clone()
            .map(|v| Arc::new(FailingDeletes(v)) as Arc<dyn MaintenanceVectors>),
        episode_vectors: good.episode_vectors.clone(),
        facts: good.facts.clone(),
        episodes: good.episodes.clone(),
        entities: good.entities.clone(),
        events: good.events.clone(),
        ingest_journal: good.ingest_journal.clone(),
        description: good.description.clone(),
    };
    let report = maintenance::forget_user(&stores, &target, false).await;
    assert!(!report.complete);
    assert_eq!(report.outcome, "incomplete");
    assert_eq!(report.after.fact_points, Some(2));
    assert!(
        report
            .steps
            .iter()
            .any(|s| s.step == "fact_points" && s.status == "failed")
    );
    assert!(
        !report.steps.iter().any(|s| s.step == "audit_event"),
        "no success event on failure"
    );
    assert_eq!(counts(&f, &target).await, (0, 0, 2, 0, 0));

    // SQL failure: a read-only database cannot delete -> incomplete; points are still deleted.
    let target2 = tenant("org_a", "user_1");
    let ro_url = f.db_url.replace("mode=rwc", "mode=ro");
    let ro = SqliteFactStore::new(memcore_storage::connect_sqlite_pool(&ro_url).await.unwrap());
    let sql_down = MaintenanceStores {
        facts: Arc::new(SqliteFactStore::new(ro.pool())),
        episodes: Arc::new(SqliteEpisodeStore::new(ro.pool())),
        entities: Arc::new(SqliteEntityStore::new(ro.pool())),
        events: Some(Arc::new(SqliteMemoryEventStore::new(ro.pool()))),
        fact_vectors: good.fact_vectors.clone(),
        episode_vectors: good.episode_vectors.clone(),
        ingest_journal: None,
        description: serde_json::json!({}),
    };
    let report = maintenance::forget_user(&sql_down, &target2, false).await;
    assert!(!report.complete);
    assert_eq!(report.after.fact_rows, Some(2));
    assert!(
        report
            .steps
            .iter()
            .any(|s| s.step == "fact_rows" && s.status == "failed")
    );
    // The audit erasure fails on the read-only database too, and the content events are
    // counted in the postcondition: incomplete, never success.
    assert!(
        report
            .steps
            .iter()
            .any(|s| s.step == "audit_events" && s.status == "failed")
    );
    assert_eq!(report.after.audit_content_events, Some(2));
    assert_eq!(counts(&f, &target2).await, (2, 2, 0, 0, 2));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains(SECRET_CONTENT));

    // Recovery: re-running with working stores completes both users.
    for t in [&target, &target2] {
        let report = maintenance::forget_user(&good, t, false).await;
        assert!(report.complete, "{report:?}");
        assert_eq!(counts(&f, t).await, (0, 0, 0, 0, 0));
    }
    assert_eq!(counts(&f, &other).await, other_before);
}

#[tokio::test]
async fn ar068_cleanup_guard_removes_only_test_owned_collections() {
    let Some(url) = qdrant_url() else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL not set");
        return;
    };
    let client = qdrant_client::Qdrant::from_url(&url)
        .skip_compatibility_check()
        .build()
        .unwrap();
    let names = {
        let guard = TestCollections::new(url.clone());
        let a = guard.name("guard");
        QdrantVectorStore::connect(&url, &a, DIMS).await.unwrap();
        QdrantVectorStore::connect(&url, &format!("{a}_episodes"), DIMS)
            .await
            .unwrap();
        let b = format!("memcore_handmade_{}", Uuid::new_v4().simple());
        guard.register(&b);
        QdrantVectorStore::connect(&url, &b, DIMS).await.unwrap();
        vec![a.clone(), format!("{a}_episodes"), b]
    };
    for name in &names {
        assert!(
            !client.collection_exists(name).await.unwrap(),
            "{name} left behind"
        );
    }
    for bad in [
        "memcore_phase5_p0_store",
        "memcore_launch_idx",
        "shared_collection",
        "memcore_x_123",
    ] {
        let result =
            std::panic::catch_unwind(|| TestCollections::new("http://127.0.0.1:1").register(bad));
        assert!(result.is_err(), "{bad} must be refused");
    }
    // Cleanup errors are reported, not hidden.
    let unreachable = TestCollections::new("http://127.0.0.1:1");
    unreachable.name("unreachable");
    assert!(!unreachable.cleanup().is_empty());
}

/// The same erasure on the PostgreSQL topology (fact + event backends on Postgres). Needs
/// MEMCORE_TEST_POSTGRES_URL (a disposable database) and MEMCORE_TEST_QDRANT_URL.
#[cfg(feature = "postgres")]
#[tokio::test]
async fn ar048_maintenance_on_postgres() {
    use memcore_storage::{PostgresEpisodeStore, PostgresFactStore};
    let (Some(url), Ok(pg)) = (qdrant_url(), std::env::var("MEMCORE_TEST_POSTGRES_URL")) else {
        eprintln!("skipping: MEMCORE_TEST_QDRANT_URL / MEMCORE_TEST_POSTGRES_URL not set");
        return;
    };
    let collections = TestCollections::new(url.clone());
    let run = Uuid::new_v4().simple().to_string();
    let orgs = [format!("org_a_{run}"), format!("org_b_{run}")];
    let facts_store = PostgresFactStore::connect(&pg).await.unwrap();
    let episode_store = PostgresEpisodeStore::new(facts_store.pool());
    let facts = collections.name("ar048_pg_facts");
    let episodes = format!("{facts}_episodes");
    let old = EmbeddingIdentity::remote("openai_compatible", "old-model", DIMS);
    let fv = QdrantVectorStore::connect_verified(&url, &facts, DIMS, Some(&old), None)
        .await
        .unwrap();
    let ev = QdrantVectorStore::connect_verified(&url, &episodes, DIMS, Some(&old), None)
        .await
        .unwrap();
    let now = Utc::now();
    let tenants: Vec<TenantContext> = orgs
        .iter()
        .flat_map(|org| ["user_1", "user_2"].map(|user| tenant(org, user)))
        .collect();
    for t in &tenants {
        let id = Uuid::new_v4();
        let fact = Fact::new(
            id,
            t.org_id.clone(),
            t.user_id.clone(),
            MemoryType::Preference,
            format!("{SECRET_CONTENT} pg fact"),
            None,
            MemorySource::UserMessage,
            0.9,
            0.5,
            None,
            None,
            now,
            now,
            serde_json::json!({}),
        )
        .unwrap();
        facts_store.insert_fact(t, fact.clone()).await.unwrap();
        memcore_storage::PostgresMemoryEventStore::new(facts_store.pool())
            .record_event(
                t,
                memcore_core::audit::build_add_event(t, &fact, &None, &None, serde_json::json!({})),
            )
            .await
            .unwrap();
        fv.upsert_vector(t, record(id, &t.org_id, &t.user_id, "x"))
            .await
            .unwrap();
        let episode_id = Uuid::new_v4();
        episode_store
            .insert_episodes(
                t,
                vec![Episode {
                    id: episode_id,
                    org_id: t.org_id.clone(),
                    user_id: t.user_id.clone(),
                    session_id: None,
                    message_id: None,
                    speaker: None,
                    role: MessageRole::User,
                    content: format!("{SECRET_CONTENT} pg episode"),
                    observed_at: None,
                    recorded_at: now,
                    embedded: true,
                    metadata: serde_json::json!({}),
                }],
            )
            .await
            .unwrap();
        ev.upsert_vector(t, record(episode_id, &t.org_id, &t.user_id, "y"))
            .await
            .unwrap();
    }
    let count = |t: TenantContext| {
        let (facts_store, episode_store, fv, ev) = (&facts_store, &episode_store, &fv, &ev);
        async move {
            (
                facts_store.count_facts_by_user(&t).await.unwrap() as u64,
                episode_store.list_episodes(&t, 100).await.unwrap().len() as u64,
                fv.count_tenant_points(&t).await.unwrap(),
                ev.count_tenant_points(&t).await.unwrap(),
                memcore_storage::PostgresMemoryEventStore::new(facts_store.pool())
                    .count_user_audit_content(&t)
                    .await
                    .unwrap() as u64,
            )
        }
    };
    let target = tenants[0].clone();
    assert_eq!(count(target.clone()).await, (1, 1, 1, 1, 1));

    let mut settings = Settings::qdrant_with_collection(url.clone(), facts.clone());
    settings.qdrant_episode_collection = episodes.clone();
    settings.fact_backend = FactBackend::Postgres;
    settings.event_backend = EventBackend::Postgres;
    settings.postgres_url = Some(pg.clone());
    let refusal = match AppState::initialize(settings.clone()).await {
        Ok(_) => panic!("API startup must refuse the identity mismatch"),
        Err(error) => error.to_string(),
    };
    assert!(refusal.contains("identity"), "{refusal}");

    let fixture = Fixture {
        _dir: tempfile::tempdir().unwrap(),
        db_url: "unused".into(),
        db_path: Default::default(),
        url: url.clone(),
        facts: facts.clone(),
        episodes: episodes.clone(),
    };
    let confirm = maintenance::confirmation_phrase(&target.org_id, &target.user_id);
    let pg_env = [
        ("MEMCORE_FACT_BACKEND", "postgres"),
        ("MEMCORE_EVENT_BACKEND", "postgres"),
        ("MEMCORE_POSTGRES_URL", pg.as_str()),
    ];
    let args = [
        "forget-user",
        "--org-id",
        target.org_id.as_str(),
        "--user-id",
        target.user_id.as_str(),
        "--confirm",
        confirm.as_str(),
    ];
    let (code, json, output) = run_binary(&fixture, &args, &pg_env);
    assert_eq!(
        (code, json["outcome"].as_str()),
        (0, Some("deleted")),
        "{output}"
    );
    assert_eq!(json["stores"]["sql"], "postgres");
    assert_eq!(count(target.clone()).await, (0, 0, 0, 0, 0));
    for other in &tenants[1..] {
        assert_eq!(
            count(other.clone()).await,
            (1, 1, 1, 1, 1),
            "{other:?} touched"
        );
    }
    assert!(!output.contains(SECRET_CONTENT) && !output.contains(&pg));
    let (code, json, _) = run_binary(&fixture, &args, &pg_env);
    assert_eq!(
        (code, json["outcome"].as_str()),
        (0, Some("nothing_to_delete"))
    );
    // Leave the shared test database as found.
    for t in &tenants[1..] {
        facts_store.delete_user_data(t).await.unwrap();
        episode_store.delete_user_episodes(t).await.unwrap();
    }
}
