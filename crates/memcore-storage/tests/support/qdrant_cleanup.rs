//! Test-owned Qdrant collections, deleted when the guard drops.
//!
//! Every Qdrant-backed test names its collections through [`TestCollections::name`] (or registers a
//! name it built with [`TestCollections::register`]). On drop - including when the test panics -
//! the guard deletes exactly those collections and their companions (`_episodes`,
//! `__embedding_identity`), and nothing else:
//!
//! - ownership marker: a registered name must end in `_<32 hex>` (a fresh UUID), so a shared or
//!   hand-made collection can never be registered by accident;
//! - protected names (kept reference stores) are refused
//!   outright, whatever their suffix;
//! - cleanup errors are reported (a panic when the test itself passed, a message when it is
//!   already failing), never swallowed.
//!
//! There is deliberately no "delete everything matching a prefix" helper.

#![allow(dead_code)]

use std::sync::Mutex;

const PROTECTED_PREFIXES: &[&str] = &["memcore_phase", "mem0_phase0_c", "memcore_launch_"];
const COMPANIONS: &[&str] = &[
    "",
    "__embedding_identity",
    "_episodes",
    "_episodes__embedding_identity",
];

pub struct TestCollections {
    url: String,
    names: Mutex<Vec<String>>,
}

/// True when `name` ends in `_` + 32 lowercase hex digits.
pub fn has_owner_marker(name: &str) -> bool {
    name.rsplit_once('_').is_some_and(|(_, suffix)| {
        suffix.len() == 32
            && suffix
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

pub fn is_protected(name: &str) -> bool {
    PROTECTED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

impl TestCollections {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            names: Mutex::new(Vec::new()),
        }
    }

    /// A guard for one test-built name on the Qdrant at `MEMCORE_TEST_QDRANT_URL` (a no-op guard
    /// when it is unset: those tests skip before touching Qdrant).
    pub fn owning(name: &str) -> Self {
        let url = std::env::var("MEMCORE_TEST_QDRANT_URL").unwrap_or_default();
        let guard = Self::new(url.trim());
        if !guard.url.is_empty() {
            guard.register(name);
        }
        guard
    }

    /// A fresh, registered collection name: `memcore_test_<tag>_<uuid>`.
    pub fn name(&self, tag: &str) -> String {
        let name = format!("memcore_test_{tag}_{}", uuid::Uuid::new_v4().simple());
        self.register(&name);
        name
    }

    /// Registers a test-built name; panics on a name without the ownership marker or a protected one.
    pub fn register(&self, name: &str) {
        assert!(
            !is_protected(name),
            "refusing to register protected collection {name}"
        );
        assert!(
            has_owner_marker(name),
            "refusing to register {name}: test-owned names end in _<uuid simple>"
        );
        self.names.lock().unwrap().push(name.to_string());
    }

    /// Deletes the registered collections now; returns the errors (also used by Drop).
    pub fn cleanup(&self) -> Vec<String> {
        let names: Vec<String> = std::mem::take(&mut *self.names.lock().unwrap());
        if names.is_empty() {
            return Vec::new();
        }
        let url = self.url.clone();
        // Own thread + runtime: works from sync code, async tests and during unwinding.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            runtime.block_on(async move {
                let mut errors = Vec::new();
                let client = match qdrant_client::Qdrant::from_url(&url)
                    .skip_compatibility_check()
                    .build()
                {
                    Ok(client) => client,
                    Err(error) => return vec![format!("connect {url}: {error}")],
                };
                for base in names {
                    for companion in COMPANIONS {
                        let name = format!("{base}{companion}");
                        match client.collection_exists(&name).await {
                            Ok(false) => {}
                            Ok(true) => {
                                if let Err(error) = client.delete_collection(&name).await {
                                    errors.push(format!("delete {name}: {error}"));
                                }
                            }
                            Err(error) => errors.push(format!("check {name}: {error}")),
                        }
                    }
                }
                errors
            })
        })
        .join()
        .unwrap_or_else(|_| vec!["cleanup thread panicked".to_string()])
    }
}

impl Drop for TestCollections {
    fn drop(&mut self) {
        let errors = self.cleanup();
        if errors.is_empty() {
            return;
        }
        if std::thread::panicking() {
            eprintln!("qdrant test cleanup errors (test already failing): {errors:?}");
        } else {
            panic!("qdrant test cleanup failed: {errors:?}");
        }
    }
}
