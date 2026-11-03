//! The REAL `memcore-maintenance verify-vectors` binary (run as a child process) against disposable
//! PostgreSQL + Qdrant (and SQLite + Qdrant), with the shipped offline `mock` embedding provider
//! (identity `mock/deterministic`, 8 dimensions) recorded on the collections exactly as the API
//! records it at startup. No identity bypass: the production checks run unchanged.
//!
//! Every "no write" claim is a before / after comparison of the PostgreSQL rows and every Qdrant
//! point (id, payload, vector) read with the raw client. Needs `MEMCORE_TEST_POSTGRES_URL` (the
//! runner recreates `ar023t2_cli`) and `MEMCORE_TEST_QDRANT_URL`. Run with `--test-threads 1`.
//! Invented data only.

#![cfg(all(feature = "postgres", feature = "qdrant"))]

#[path = "../../memcore-storage/tests/support/qdrant_cleanup.rs"]
mod qdrant_cleanup;

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use memcore_core::ports::{
    EmbeddingIdentity, PointPage, PointScope, VectorIdentityStatus, VectorPointState,
};
use memcore_core::verify::{
    VerifyMode, VerifyOptions, VerifyStores, confirmation_phrase, verify_vectors,
};
use memcore_core::{
    AddMemoryInput, CandidateFact, EmbeddingProvider, Episode, EpisodeComponents, EpisodeStore,
    FactStore, MemoryEngine, MemoryMessage, MemoryType, MessageRole, TenantContext, VectorRecord,
    VectorSearchQuery, VectorSearchResult, VectorStore, VectorWriteIntent,
};
use memcore_providers::{MockEmbeddingProvider, MockLlmProvider, deterministic_embedding};
use memcore_storage::{
    PostgresEpisodeStore, PostgresFactStore, QdrantVectorStore, SqliteEpisodeStore, SqliteFactStore,
};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{PointStruct, ScrollPointsBuilder, UpsertPointsBuilder};
use serde_json::{Value, json};
use uuid::Uuid;

/// The shipped mock provider's dimensions (`memcore-api` `MOCK_EMBEDDING_DIMENSIONS`).
const DIMS: usize = 8;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn pg_url(database: &str) -> Option<String> {
    let url = env("MEMCORE_TEST_POSTGRES_URL")?;
    Some(format!("{}/{database}", url.rsplit_once('/')?.0))
}

fn raw(url: &str) -> Qdrant {
    Qdrant::from_url(url)
        .skip_compatibility_check()
        .build()
        .unwrap()
}

fn mock_identity() -> EmbeddingIdentity {
    MockEmbeddingProvider::new(DIMS)
        .embedding_identity()
        .expect("mock identity")
}

enum Sql {
    Pg(Arc<PostgresFactStore>, Arc<PostgresEpisodeStore>, String),
    Sl(
        Arc<SqliteFactStore>,
        Arc<SqliteEpisodeStore>,
        String,
        #[allow(dead_code)] tempfile::TempDir,
    ),
}

impl Sql {
    fn facts(&self) -> Arc<dyn FactStore> {
        match self {
            Sql::Pg(f, _, _) => f.clone(),
            Sql::Sl(f, _, _, _) => f.clone(),
        }
    }
    fn episodes(&self) -> Arc<dyn EpisodeStore> {
        match self {
            Sql::Pg(_, e, _) => e.clone(),
            Sql::Sl(_, e, _, _) => e.clone(),
        }
    }
    fn backend(&self) -> &'static str {
        match self {
            Sql::Pg(..) => "postgres",
            Sql::Sl(..) => "sqlite",
        }
    }
    fn url(&self) -> &str {
        match self {
            Sql::Pg(_, _, u) => u,
            Sql::Sl(_, _, u, _) => u,
        }
    }
}

struct Cli {
    _guard: qdrant_cleanup::TestCollections,
    qurl: String,
    facts_collection: String,
    episodes_collection: String,
    sql: Sql,
    vectors: Arc<QdrantVectorStore>,
    episode_vectors: Arc<QdrantVectorStore>,
    tag: String,
}

impl Cli {
    async fn open(sqlite: bool, tag: &str, identity: Option<EmbeddingIdentity>) -> Option<Self> {
        let (Some(url), Some(qurl)) = (pg_url("ar023t2_cli"), env("MEMCORE_TEST_QDRANT_URL"))
        else {
            eprintln!(
                "skipping T2 CLI test: MEMCORE_TEST_POSTGRES_URL / MEMCORE_TEST_QDRANT_URL not set"
            );
            return None;
        };
        let sql = if sqlite {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cli.db");
            std::fs::File::create(&path).unwrap();
            let su = format!("sqlite:{}", path.to_string_lossy().replace('\\', "/"));
            Sql::Sl(
                Arc::new(SqliteFactStore::connect(&su).await.unwrap()),
                Arc::new(SqliteEpisodeStore::connect(&su).await.unwrap()),
                su,
                dir,
            )
        } else {
            let f = Arc::new(PostgresFactStore::connect(&url).await.unwrap());
            let e = Arc::new(PostgresEpisodeStore::new(f.pool()));
            Sql::Pg(f, e, url)
        };
        let guard = qdrant_cleanup::TestCollections::new(qurl.clone());
        let facts_collection = guard.name(&format!("t2cli{tag}"));
        let episodes_collection = format!("{facts_collection}_episodes");
        // As the API does at startup: an empty collection records the configured identity and a
        // fingerprint from the provider.
        let probe = MockEmbeddingProvider::new(DIMS);
        let identity = identity.unwrap_or_else(mock_identity);
        let vectors = Arc::new(
            QdrantVectorStore::connect_verified(
                &qurl,
                &facts_collection,
                DIMS,
                Some(&identity),
                Some(&probe),
            )
            .await
            .unwrap(),
        );
        let episode_vectors = Arc::new(
            QdrantVectorStore::connect_verified(
                &qurl,
                &episodes_collection,
                DIMS,
                Some(&identity),
                Some(&probe),
            )
            .await
            .unwrap(),
        );
        Some(Self {
            _guard: guard,
            qurl,
            facts_collection,
            episodes_collection,
            sql,
            vectors,
            episode_vectors,
            tag: tag.to_string(),
        })
    }

    fn t(&self, n: usize) -> TenantContext {
        match n {
            0 => TenantContext::new(format!("org_{}_a", self.tag), "u1").unwrap(),
            1 => TenantContext::new(format!("org_{}_a", self.tag), "u2").unwrap(),
            _ => TenantContext::new(format!("org_{}_b", self.tag), "u1").unwrap(),
        }
    }

    fn canary(&self, n: usize, k: usize) -> String {
        let t = self.t(n);
        format!("CANARY-{}-{}-{k}", t.org_id, t.user_id)
    }

    fn engine(&self, llm: MockLlmProvider) -> MemoryEngine {
        MemoryEngine::new(
            self.sql.facts(),
            self.vectors.clone(),
            Arc::new(llm),
            Arc::new(MockEmbeddingProvider::new(DIMS)),
        )
        .with_min_importance(0.0)
        .with_episodes(Some(EpisodeComponents {
            store: self.sql.episodes(),
            vectors: self.episode_vectors.clone(),
        }))
    }

    /// FX: facts written exactly as `apply_add` writes them (row, then a fresh-fact vector with a
    /// random point id); with 8-dimension mock embeddings the engine's embedding dedup would merge
    /// unrelated texts, so the facts bypass extraction. Episodes go through the engine.
    async fn seed(&self, n: usize) {
        let topic = [
            "kayaking on mountain lakes",
            "baking sourdough bread",
            "restoring vintage radios",
            "growing chilli peppers",
        ];
        let t = self.t(n);
        let embedder = MockEmbeddingProvider::new(DIMS);
        for k in 1..=4 {
            let content = format!("Fact {k} {} prefers {}", self.canary(n, k), topic[k - 1]);
            let now = Utc::now();
            let fact = memcore_core::Fact::new(
                Uuid::new_v4(),
                t.org_id.clone(),
                t.user_id.clone(),
                MemoryType::Skill,
                content.clone(),
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
            .unwrap();
            let stored = self.sql.facts().insert_fact(&t, fact).await.unwrap();
            self.vectors
                .write_vector(
                    &t,
                    VectorRecord {
                        id: Uuid::new_v4(),
                        fact_id: stored.id,
                        org_id: t.org_id.clone(),
                        user_id: t.user_id.clone(),
                        embedding: embedder.embed_text(&content).await.unwrap(),
                        content,
                        memory_type: MemoryType::Skill,
                        metadata: json!({}),
                    },
                    VectorWriteIntent::FreshFact,
                )
                .await
                .unwrap();
        }
        let candidates: Vec<CandidateFact> = Vec::new();
        let mut messages: Vec<MemoryMessage> = (1..=3)
            .map(|k| MemoryMessage {
                role: MessageRole::User,
                content: format!(
                    "Episode {k} message {} about weekend plans",
                    self.canary(n, 10 + k)
                ),
                message_id: Some(format!("e{k}")),
                ..Default::default()
            })
            .collect();
        messages.push(MemoryMessage {
            role: MessageRole::User,
            content: "short message 4".into(),
            message_id: Some("e4".into()),
            ..Default::default()
        });
        self.engine(MockLlmProvider::new().with_extraction_candidates(candidates))
            .add_memory(AddMemoryInput {
                tenant: self.t(n),
                messages,
                metadata: json!({}),
                session: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            self.sql
                .facts()
                .list_fact_states(&t, None, 100)
                .await
                .unwrap()
                .len(),
            4,
            "4 seeded facts"
        );
    }

    async fn fact(&self, n: usize, k: usize) -> memcore_core::Fact {
        let marker = self.canary(n, k);
        self.sql
            .facts()
            .list_fact_states(&self.t(n), None, 1000)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.fact)
            .find(|f| f.content.contains(&marker))
            .unwrap()
    }

    async fn plant(&self, collection: &str, point: Uuid, payload: Value, text: &str) {
        raw(&self.qurl)
            .upsert_points(
                UpsertPointsBuilder::new(
                    collection,
                    vec![PointStruct::new(
                        point.to_string(),
                        deterministic_embedding(text, DIMS).unwrap(),
                        qdrant_client::Payload::try_from(payload).unwrap(),
                    )],
                )
                .wait(true),
            )
            .await
            .unwrap();
    }

    fn payload(&self, n: usize, fact: Uuid, content: &str) -> Value {
        let t = self.t(n);
        json!({"id": Uuid::new_v4().to_string(), "fact_id": fact.to_string(), "org_id": t.org_id, "user_id": t.user_id, "content": content, "memory_type": "skill", "metadata": "{}"})
    }

    /// The protocol's divergence fixture DV for tenant `n` (one item per category). Returns the
    /// ids that must never be touched: the unattributable and conflict points.
    async fn plant_dv(&self, n: usize) -> (Uuid, Uuid) {
        let (f1, f2, f3, f4) = (
            self.fact(n, 1).await,
            self.fact(n, 2).await,
            self.fact(n, 3).await,
            self.fact(n, 4).await,
        );
        let t = self.t(n);
        // missing fact point
        self.vectors.delete_by_fact_id(&t, f1.id).await.unwrap();
        // stale point (same point id, other text)
        let f2_point = self
            .vectors
            .fact_vector_points(&t, f2.id)
            .await
            .unwrap()
            .unwrap()[0]
            .point_id
            .clone();
        let f2_point = f2_point.trim_start_matches("u:").to_string();
        self.plant(
            &self.facts_collection,
            Uuid::parse_str(&f2_point).unwrap(),
            self.payload(n, f2.id, "STALE obsolete text"),
            "STALE obsolete text",
        )
        .await;
        // orphan of an absent fact
        self.plant(
            &self.facts_collection,
            Uuid::new_v4(),
            self.payload(n, Uuid::new_v4(), "ORPHAN absent fact text"),
            "ORPHAN absent",
        )
        .await;
        // orphan of a soft-deleted fact (row deleted directly, point kept)
        self.sql.facts().soft_delete_fact(&t, f3.id).await.unwrap();
        // a fact with 2 points
        self.plant(
            &self.facts_collection,
            Uuid::new_v4(),
            self.payload(n, f4.id, &f4.content),
            &f4.content,
        )
        .await;
        // embeddable episode with embedded = 0 and no point
        self.sql
            .episodes()
            .insert_episodes(
                &t,
                vec![Episode {
                    id: Uuid::new_v4(),
                    org_id: t.org_id.clone(),
                    user_id: t.user_id.clone(),
                    session_id: Some("S9".into()),
                    message_id: Some(format!("p{n}")),
                    speaker: None,
                    role: MessageRole::User,
                    content: format!("Pending episode {} never embedded", self.canary(n, 30)),
                    observed_at: None,
                    recorded_at: Utc::now(),
                    embedded: false,
                    metadata: json!({}),
                }],
            )
            .await
            .unwrap();
        // orphan episode point
        let ep = Uuid::new_v4();
        self.plant(&self.episodes_collection, ep, json!({"id": ep.to_string(), "fact_id": ep.to_string(), "org_id": t.org_id, "user_id": t.user_id, "content": "ORPHAN episode text", "memory_type": "conversation", "metadata": "{\"kind\":\"episode\"}"}), "ORPHAN episode").await;
        // unattributable point (no tenant payload)
        let unattributable = Uuid::new_v4();
        self.plant(
            &self.facts_collection,
            unattributable,
            json!({"fact_id": Uuid::new_v4().to_string(), "content": "UNATTRIBUTABLE text"}),
            "UNATTRIBUTABLE",
        )
        .await;
        // attribution conflict: tenant n payload naming the OTHER tenant's live fact
        let other = if n == 0 { 1 } else { 0 };
        let other_fact = self.fact(other, 1).await;
        let conflict = Uuid::new_v4();
        self.plant(
            &self.facts_collection,
            conflict,
            self.payload(n, other_fact.id, "CONFLICT text naming another tenant"),
            "CONFLICT",
        )
        .await;
        (unattributable, conflict)
    }

    /// Every SQL row and every Qdrant point (id, payload, vector) of both collections.
    async fn snapshot(&self) -> String {
        let mut out = String::new();
        for n in 0..3 {
            let t = self.t(n);
            let mut rows: Vec<String> = self
                .sql
                .facts()
                .list_fact_states(&t, None, 10_000)
                .await
                .unwrap()
                .iter()
                .map(|s| {
                    format!(
                        "{}|{}|{}|{}",
                        s.fact.id, s.fact.content, s.deleted, s.fact.updated_at
                    )
                })
                .collect();
            rows.sort();
            let mut eps: Vec<String> = self
                .sql
                .episodes()
                .list_episodes(&t, 10_000)
                .await
                .unwrap()
                .iter()
                .map(|e| format!("{}|{}|{}", e.id, e.content, e.embedded))
                .collect();
            eps.sort();
            out.push_str(&format!("T{n} rows {rows:?}\nT{n} episodes {eps:?}\n"));
        }
        for c in [&self.facts_collection, &self.episodes_collection] {
            out.push_str(&format!("{c}: {}\n", points_dump(&self.qurl, c).await));
        }
        let mut names: Vec<String> = raw(&self.qurl)
            .list_collections()
            .await
            .unwrap()
            .collections
            .into_iter()
            .map(|c| c.name)
            .filter(|n| n.starts_with(&self.facts_collection))
            .collect();
        names.sort();
        out.push_str(&format!("collections {names:?}"));
        out
    }

    fn command(&self, n: usize, extra: &[&str]) -> (i32, Value, String) {
        let t = self.t(n);
        let mut args = vec![
            "verify-vectors",
            "--org-id",
            t.org_id.as_str(),
            "--user-id",
            t.user_id.as_str(),
        ];
        args.extend_from_slice(extra);
        let output = Command::new(env!("CARGO_BIN_EXE_memcore-maintenance"))
            .args(&args)
            .env("MEMCORE_FACT_BACKEND", self.sql.backend())
            .env("MEMCORE_EVENT_BACKEND", "mock")
            .env("MEMCORE_DATABASE_URL", self.sql.url())
            .env("MEMCORE_POSTGRES_URL", self.sql.url())
            .env("MEMCORE_VECTOR_BACKEND", "qdrant")
            .env("MEMCORE_QDRANT_URL", &self.qurl)
            .env("MEMCORE_QDRANT_COLLECTION", &self.facts_collection)
            .env(
                "MEMCORE_QDRANT_EPISODE_COLLECTION",
                &self.episodes_collection,
            )
            .env("MEMCORE_EMBEDDING_PROVIDER", "mock")
            .env("MEMCORE_LLM_PROVIDER", "mock")
            .env_remove("MEMCORE_TEST_POSTGRES_URL")
            .env_remove("MEMCORE_TEST_QDRANT_URL")
            .output()
            .expect("run memcore-maintenance");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let value = serde_json::from_str(&stdout).unwrap_or(Value::Null);
        (
            output.status.code().unwrap_or(-1),
            value,
            format!("{stdout}\n{stderr}"),
        )
    }
}

async fn points_dump(url: &str, collection: &str) -> String {
    let client = raw(url);
    let mut all = Vec::new();
    let mut offset = None;
    loop {
        let mut req = ScrollPointsBuilder::new(collection)
            .limit(256)
            .with_payload(true)
            .with_vectors(true);
        if let Some(o) = offset.clone() {
            req = req.offset(o);
        }
        let page = client.scroll(req).await.unwrap();
        for p in page.result {
            let mut payload: Vec<String> = p
                .payload
                .iter()
                .map(|(k, v)| format!("{k}={v:?}"))
                .collect();
            payload.sort();
            all.push(format!("{:?}|{payload:?}|{:?}", p.id, p.vectors));
        }
        match page.next_page_offset {
            Some(o) => offset = Some(o),
            None => break,
        }
    }
    all.sort();
    format!("{} points {all:?}", all.len())
}

macro_rules! cli {
    ($sqlite:expr, $tag:expr, $identity:expr) => {
        match Cli::open($sqlite, $tag, $identity).await {
            Some(c) => c,
            None => return,
        }
    };
}

fn counts(report: &Value, section: &str) -> BTreeMap<String, u64> {
    report[section]
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(_, v)| v.as_u64().unwrap_or(0) > 0)
                .map(|(k, v)| (k.clone(), v.as_u64().unwrap()))
                .collect()
        })
        .unwrap_or_default()
}

fn bmap(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[tokio::test]
async fn g5_v1_v6_v2_v3_real_cli_dry_run_refusals_and_confirmed_repair_on_postgres() {
    let c = cli!(false, "pg", None);
    for n in 0..3 {
        c.seed(n).await;
    }
    let (unattributable, conflict) = c.plant_dv(0).await;
    let _ = c.plant_dv(1).await; // TB's own DV must stay untouched by every TA command

    // V1: dry run.
    let before = c.snapshot().await;
    let (code, report, raw_out) = c.command(0, &[]);
    eprintln!(
        "G5 V1 dry run: exit {code} outcome {} facts {:?} episodes {:?} global {:?}",
        report["outcome"],
        counts(&report, "facts"),
        counts(&report, "episodes"),
        counts(&report, "global")
    );
    assert_eq!(code, 1, "V1 exit 1: {raw_out}");
    assert_eq!(report["outcome"], "repair_needed");
    assert_eq!(
        report["stores"]["embedding_provider"],
        "not constructed (dry run)"
    );
    assert_eq!(
        counts(&report, "facts"),
        bmap(&[
            ("duplicate_points", 1),
            ("hard_deleted_orphan", 1),
            ("missing_vector", 1),
            ("soft_deleted_orphan", 1),
            ("stale_content", 1)
        ]),
        "V1 fact categories"
    );
    assert_eq!(
        counts(&report, "episodes"),
        bmap(&[
            ("consistent", 3),
            ("episode_pending", 1),
            ("episode_short_exempt", 1),
            ("hard_deleted_orphan", 1)
        ]),
        "V1 episode categories"
    );
    // Global: the unattributable scan is collection-wide (TA's and TB's planted points); the
    // attribution conflict is TA's own point naming TB's fact.
    assert_eq!(
        counts(&report, "global"),
        bmap(&[
            ("tenant_attribution_conflict", 1),
            ("unattributable_point", 2)
        ]),
        "V1 global"
    );
    assert_eq!(
        c.snapshot().await,
        before,
        "V1: PostgreSQL rows and every Qdrant point unchanged"
    );
    for k in 1..=40 {
        assert!(
            !raw_out.contains(&c.canary(1, k)) && !raw_out.contains(&c.canary(2, k)),
            "no other tenant's text"
        );
        assert!(
            !raw_out.contains(&c.canary(0, k)),
            "report carries hashes, not text"
        );
    }
    assert!(
        !raw_out.contains("STALE obsolete") && !raw_out.contains("CONFLICT text"),
        "no payload text in the report"
    );

    // V6: refusals before any read / write.
    let other = confirmation_phrase(&c.t(1));
    for extra in [
        vec!["--repair"],
        vec!["--repair", "--confirm", other.as_str()],
        vec!["--repair", "--confirm", "verify-vectors:all"],
        vec!["--confirm", "x"],
    ] {
        let (code, _, out) = c.command(0, &extra);
        eprintln!("G5 V6 {extra:?}: exit {code}");
        assert_eq!(code, 2, "V6 refused ({extra:?}): {out}");
    }
    assert_eq!(c.snapshot().await, before, "V6: nothing changed");

    // V2: confirmed repair.
    let phrase = confirmation_phrase(&c.t(0));
    let tb_tc_before: Vec<String> = before
        .lines()
        .filter(|l| l.starts_with("T1") || l.starts_with("T2"))
        .map(str::to_string)
        .collect();
    let (code, report, out) = c.command(0, &["--repair", "--confirm", &phrase]);
    eprintln!(
        "G5 V2 repair: exit {code} outcome {} repairs {} remaining {}",
        report["outcome"], report["repairs"], report["remaining_inconsistent"]
    );
    assert_eq!(
        report["stores"]["embedding_provider"],
        "constructed (repair)"
    );
    assert_eq!(
        report["remaining_inconsistent"], 0,
        "V2: nothing of TA remains: {out}"
    );
    assert_eq!(
        report["outcome"], "attention_required",
        "global items remain, so not clean: {out}"
    );
    assert_eq!(
        code, 0,
        "attention_required exits 0 - reported separately from the outcome"
    );
    assert_eq!(report["repairs"]["failed"], 0);
    let after = c.snapshot().await;
    let tb_tc_after: Vec<String> = after
        .lines()
        .filter(|l| l.starts_with("T1") || l.starts_with("T2"))
        .map(str::to_string)
        .collect();
    assert_eq!(tb_tc_after, tb_tc_before, "TB / TC rows untouched");
    assert!(
        after.contains(&format!("{unattributable}"))
            || after.contains(&unattributable.to_string().replace('-', "")),
        "the unattributable point is kept"
    );
    assert!(
        after.contains(&conflict.to_string()),
        "the conflict point is kept"
    );
    let t = c.t(0);
    for k in [1usize, 2, 4] {
        let f = c.fact(0, k).await;
        let pts = c
            .vectors
            .fact_vector_points(&t, f.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pts.len(), 1, "fact {k}: one point");
        assert_eq!(pts[0].content, f.content, "fact {k}: point = SQL text");
    }
    let pending = c.sql.episodes().list_episodes(&t, 1000).await.unwrap();
    assert!(
        pending
            .iter()
            .filter(|e| e.is_embeddable())
            .all(|e| e.embedded),
        "pending episode embedded"
    );
    assert!(
        pending
            .iter()
            .filter(|e| !e.is_embeddable())
            .all(|e| !e.embedded),
        "short episode untouched"
    );
    let (code, dry, _) = c.command(0, &[]);
    assert_eq!(code, 0, "fresh dry run: TA clean (global items only)");
    assert!(
        dry["outcome"] == "attention_required" || dry["outcome"] == "clean",
        "{}",
        dry["outcome"]
    );

    // V3: repeated repair performs no write.
    let before_v3 = c.snapshot().await;
    let (_, report, _) = c.command(0, &["--repair", "--confirm", &phrase]);
    assert_eq!(report["repairs"]["attempted"], 0, "V3: no repair attempted");
    assert_eq!(c.snapshot().await, before_v3, "V3: no write");
    eprintln!(
        "G5 V3: second repair outcome {} repairs {}",
        report["outcome"], report["repairs"]
    );
}

/// A fact vector store that fails the n-th `write_vector` AFTER the real write (V4).
struct AfterFault(Arc<QdrantVectorStore>, Mutex<usize>, usize);

#[async_trait]
impl VectorStore for AfterFault {
    async fn upsert_vector(&self, t: &TenantContext, r: VectorRecord) -> MemcoreResult<()> {
        self.0.upsert_vector(t, r).await
    }
    async fn write_vector(
        &self,
        t: &TenantContext,
        r: VectorRecord,
        i: VectorWriteIntent,
    ) -> MemcoreResult<()> {
        self.0.write_vector(t, r, i).await?;
        let mut n = self.1.lock().unwrap();
        *n += 1;
        if *n == self.2 {
            return Err(MemcoreError::Internal(
                "injected write_vector failure (after)".into(),
            ));
        }
        Ok(())
    }
    async fn search_vectors(&self, q: VectorSearchQuery) -> MemcoreResult<Vec<VectorSearchResult>> {
        self.0.search_vectors(q).await
    }
    async fn delete_by_fact_id(&self, t: &TenantContext, id: Uuid) -> MemcoreResult<()> {
        self.0.delete_by_fact_id(t, id).await
    }
    async fn delete_by_user(&self, t: &TenantContext) -> MemcoreResult<()> {
        self.0.delete_by_user(t).await
    }
    async fn fact_vector_points(
        &self,
        t: &TenantContext,
        id: Uuid,
    ) -> MemcoreResult<Option<Vec<VectorPointState>>> {
        self.0.fact_vector_points(t, id).await
    }
    async fn scroll_points(
        &self,
        s: &PointScope,
        c: Option<String>,
        l: usize,
    ) -> MemcoreResult<PointPage> {
        self.0.scroll_points(s, c, l).await
    }
    async fn count_points(&self, s: &PointScope) -> MemcoreResult<u64> {
        self.0.count_points(s).await
    }
    async fn delete_points(&self, t: &TenantContext, ids: &[String]) -> MemcoreResult<()> {
        self.0.delete_points(t, ids).await
    }
}

#[tokio::test]
async fn g5_v4_interrupted_repair_on_real_stores_is_completed_by_the_real_cli() {
    let c = cli!(false, "v4", None);
    for n in 0..3 {
        c.seed(n).await;
    }
    c.plant_dv(0).await;
    let faulted = AfterFault(c.vectors.clone(), Mutex::new(0), 2);
    let embedding = MockEmbeddingProvider::new(DIMS);
    let facts = c.sql.facts();
    let episodes = c.sql.episodes();
    let stores = VerifyStores {
        facts: facts.as_ref(),
        episodes: Some(episodes.as_ref()),
        fact_vectors: Some(&faulted),
        episode_vectors: Some(c.episode_vectors.as_ref()),
        embedding: Some(&embedding),
        fact_identity: VectorIdentityStatus::Verified,
        episode_identity: VectorIdentityStatus::Verified,
    };
    let mut options = VerifyOptions::dry_run();
    options.mode = VerifyMode::Repair {
        confirmation: confirmation_phrase(&c.t(0)),
    };
    let first = verify_vectors(&stores, &c.t(0), &options).await;
    eprintln!(
        "G5 V4 interrupted: exit {} outcome {} repairs {:?}",
        first.exit_code, first.outcome, first.repairs
    );
    assert!(
        *faulted.1.lock().unwrap() >= 2,
        "X1: the second write_vector ran and failed after its effect"
    );
    assert_ne!(first.exit_code, 0, "V4: exit != 0");
    assert!(first.repairs.failed >= 1, "the failed item is reported");
    let (code, report, out) =
        c.command(0, &["--repair", "--confirm", &confirmation_phrase(&c.t(0))]);
    eprintln!(
        "G5 V4 CLI re-run: exit {code} outcome {}",
        report["outcome"]
    );
    assert_eq!(report["remaining_inconsistent"], 0, "{out}");
    let t = c.t(0);
    for k in [1usize, 2, 4] {
        let f = c.fact(0, k).await;
        assert_eq!(
            c.vectors
                .fact_vector_points(&t, f.id)
                .await
                .unwrap()
                .unwrap()
                .len(),
            1,
            "no duplicate point for fact {k}"
        );
    }
}

#[tokio::test]
async fn g5_v7_identity_mismatch_refuses_re_embedding_through_the_real_cli() {
    let other = EmbeddingIdentity::remote("mock", "another-model", DIMS);
    let c = cli!(false, "v7", Some(other));
    for n in 0..3 {
        c.seed(n).await;
    }
    let (_, conflict) = c.plant_dv(0).await;
    let (code, report, out) =
        c.command(0, &["--repair", "--confirm", &confirmation_phrase(&c.t(0))]);
    eprintln!(
        "G5 V7: exit {code} outcome {} identity {} repairs {}",
        report["outcome"], report["identity"], report["repairs"]
    );
    assert_ne!(
        code, 0,
        "V7: the repair cannot complete under a mismatched identity: {out}"
    );
    let identity = report["identity"].to_string().to_ascii_lowercase();
    assert!(
        identity.contains("mismatch") || out.to_ascii_lowercase().contains("identity"),
        "identity reported: {out}"
    );
    let t = c.t(0);
    let f1 = c.fact(0, 1).await;
    assert!(
        c.vectors
            .fact_vector_points(&t, f1.id)
            .await
            .unwrap()
            .unwrap()
            .is_empty(),
        "V7: no vector written for the missing point"
    );
    let f2 = c.fact(0, 2).await;
    assert_eq!(
        c.vectors
            .fact_vector_points(&t, f2.id)
            .await
            .unwrap()
            .unwrap()[0]
            .content,
        "STALE obsolete text",
        "no re-embedding of the stale point"
    );
    let _ = conflict;
}

#[tokio::test]
async fn g5_v1_sl_real_cli_dry_run_on_sqlite_and_qdrant() {
    let c = cli!(true, "sl", None);
    for n in 0..3 {
        c.seed(n).await;
    }
    c.plant_dv(0).await;
    let before = c.snapshot().await;
    let (code, report, out) = c.command(0, &[]);
    eprintln!(
        "G5 V1-SL: exit {code} outcome {} facts {:?}",
        report["outcome"],
        counts(&report, "facts")
    );
    assert_eq!(code, 1, "{out}");
    assert_eq!(report["outcome"], "repair_needed");
    assert_eq!(report["stores"]["sql"], "sqlite");
    assert_eq!(
        counts(&report, "facts"),
        bmap(&[
            ("duplicate_points", 1),
            ("hard_deleted_orphan", 1),
            ("missing_vector", 1),
            ("soft_deleted_orphan", 1),
            ("stale_content", 1)
        ])
    );
    assert_eq!(c.snapshot().await, before, "V1-SL: no write");
}
