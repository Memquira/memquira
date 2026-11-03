//! Diagnostic (opt-in): reads every fact of a recovered store COPY back through the real SQLite
//! fact store, as ingestion's exact-dedup lookup does, and reports each read error.
//!
//! Runs only when `MEMCORE_DIAG_SQLITE_DB` names a database file (a copy: opening it applies no
//! migration to an up-to-date store, but never point it at evidence); otherwise it is a no-op.
//! `MEMCORE_DIAG_ORG` / `MEMCORE_DIAG_USER` select the tenant.

use memcore_core::ports::{FactSearchQuery, FactStore};
use memcore_core::{MemoryType, TenantContext};
use memcore_storage::SqliteFactStore;

#[tokio::test]
async fn diag_store_readback() {
    let Ok(path) = std::env::var("MEMCORE_DIAG_SQLITE_DB") else {
        return;
    };
    let org = std::env::var("MEMCORE_DIAG_ORG").expect("MEMCORE_DIAG_ORG");
    let user = std::env::var("MEMCORE_DIAG_USER").expect("MEMCORE_DIAG_USER");
    let tenant = TenantContext::new(org, user).expect("tenant");
    let store = SqliteFactStore::connect(&format!("sqlite://{path}"))
        .await
        .expect("open store copy");

    for &memory_type in MemoryType::ALL {
        let mut query = FactSearchQuery::new(tenant.clone(), 50);
        query.memory_types = Some(vec![memory_type]);
        match store.search_facts(query).await {
            Ok(found) => println!("DEDUP-LOOKUP {memory_type:?}: ok ({} facts)", found.len()),
            Err(error) => println!("DEDUP-LOOKUP {memory_type:?}: ERROR {error}"),
        }
    }

    match store
        .search_facts(FactSearchQuery::new(tenant.clone(), 100_000))
        .await
    {
        Ok(all) => println!("READ ALL: ok ({} facts)", all.len()),
        Err(error) => println!("READ ALL: ERROR {error}"),
    }
}
