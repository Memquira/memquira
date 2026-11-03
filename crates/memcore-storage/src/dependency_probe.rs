//! Cheap reachability probes for server startup and readiness. A probe only connects and asks the
//! dependency whether it is up; it never reads or writes data.

#[cfg(any(feature = "qdrant", feature = "postgres"))]
use std::time::Duration;

/// Result of one probe of one dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The dependency answered.
    Ready,
    /// Not reachable yet (connection refused, timeout, server still starting): worth retrying.
    Unavailable(String),
    /// Retrying cannot help (invalid URL, authentication failure, missing database).
    Fatal(String),
}

/// gRPC status codes that no amount of waiting fixes (`tonic::Code` discriminants).
#[cfg(feature = "qdrant")]
const GRPC_PERMISSION_DENIED: i32 = 7;
#[cfg(feature = "qdrant")]
const GRPC_UNAUTHENTICATED: i32 = 16;

/// Probes Qdrant with its gRPC health check.
#[cfg(feature = "qdrant")]
pub async fn probe_qdrant(url: &str, timeout: Duration) -> ProbeOutcome {
    use qdrant_client::{Qdrant, QdrantError};

    let client = match Qdrant::from_url(url)
        .skip_compatibility_check()
        .timeout(timeout)
        .connect_timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(error) => return ProbeOutcome::Fatal(format!("invalid Qdrant configuration: {error}")),
    };
    // The client's own request and connect timeouts bound this call.
    match client.health_check().await {
        Ok(_) => ProbeOutcome::Ready,
        Err(QdrantError::ResponseError { status })
            if matches!(
                status.code() as i32,
                GRPC_UNAUTHENTICATED | GRPC_PERMISSION_DENIED
            ) =>
        {
            ProbeOutcome::Fatal(format!(
                "Qdrant refused the connection: {}",
                status.message()
            ))
        }
        Err(QdrantError::InvalidUri(error)) => {
            ProbeOutcome::Fatal(format!("invalid Qdrant URL: {error}"))
        }
        Err(error) => ProbeOutcome::Unavailable(format!("Qdrant not reachable: {error}")),
    }
}

/// The LIVE database check behind `/health/ready`.
///
/// Readiness used to report the storage startup check, computed once, so a PostgreSQL outage after
/// startup was never seen (`/health/ready` stayed 200 for a 75 s outage in Level C). For PostgreSQL
/// this probe runs `SELECT 1` on its own lazily-connected pool of at most ONE connection:
/// - concurrent readiness polls queue for that one connection, so an outage can never multiply
///   connection attempts, and readiness never takes a connection from the data-path pools;
/// - every check is bounded: the pool's acquire timeout, plus the readiness handler's outer timeout;
/// - sqlx tests the pooled connection before use, so a connection broken by the outage is dropped
///   and a new one is opened after recovery: no restart needed.
///
/// No URL, credential or driver message is ever returned. SQLite is not probed (`None`): its
/// readiness stays the startup check.
#[derive(Clone, Debug, Default)]
pub struct DatabaseReadinessProbe {
    #[cfg(feature = "postgres")]
    postgres: Option<sqlx::PgPool>,
}

impl DatabaseReadinessProbe {
    /// No live database check (mock or SQLite storage).
    pub fn none() -> Self {
        Self::default()
    }

    /// A live check of the configured PostgreSQL database. Connects lazily: building it never
    /// touches the network.
    #[cfg(feature = "postgres")]
    pub fn postgres(url: &str, timeout: Duration) -> memcore_common::MemcoreResult<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .acquire_timeout(timeout)
            .test_before_acquire(true)
            .connect_lazy(url)
            .map_err(|_| {
                memcore_common::MemcoreError::ValidationError(
                    "invalid Postgres connection URL".to_string(),
                )
            })?;
        Ok(Self {
            postgres: Some(pool),
        })
    }

    /// Whether a live check is configured (PostgreSQL).
    pub fn is_live(&self) -> bool {
        #[cfg(feature = "postgres")]
        if self.postgres.is_some() {
            return true;
        }
        false
    }

    /// `Some(true)` when the database answered `SELECT 1`, `Some(false)` when it did not, `None` when
    /// nothing is probed live. Acquiring the connection is bounded by the pool's acquire timeout;
    /// the caller bounds the whole check (readiness wraps it in the probe timeout).
    pub async fn check(&self) -> Option<bool> {
        #[cfg(feature = "postgres")]
        if let Some(pool) = &self.postgres {
            return Some(sqlx::query("SELECT 1").execute(pool).await.is_ok());
        }
        None
    }
}

/// Probes Postgres by opening (and closing) one connection. The caller bounds the time.
#[cfg(feature = "postgres")]
pub async fn probe_postgres(url: &str) -> ProbeOutcome {
    use sqlx::Connection;

    match sqlx::PgConnection::connect(url).await {
        Ok(connection) => {
            let _ = connection.close().await;
            ProbeOutcome::Ready
        }
        Err(error) => classify_postgres_error(&error),
    }
}

/// Postgres errors that mean "not up yet" are retried; configuration, authentication and
/// missing-database errors are fatal. Messages never include the connection URL.
#[cfg(feature = "postgres")]
fn classify_postgres_error(error: &sqlx::Error) -> ProbeOutcome {
    match error {
        sqlx::Error::Configuration(_) => {
            ProbeOutcome::Fatal("invalid Postgres connection URL".to_string())
        }
        sqlx::Error::Database(db) => {
            // 57P03 cannot_connect_now: the server is starting up or shutting down.
            if db.code().as_deref() == Some("57P03") {
                ProbeOutcome::Unavailable(format!("Postgres is starting: {}", db.message()))
            } else {
                ProbeOutcome::Fatal(format!(
                    "Postgres refused the connection (SQLSTATE {}): {}",
                    db.code().as_deref().unwrap_or("unknown"),
                    db.message()
                ))
            }
        }
        sqlx::Error::Io(error) => {
            ProbeOutcome::Unavailable(format!("Postgres not reachable: {error}"))
        }
        other => ProbeOutcome::Unavailable(format!("Postgres not reachable: {other}")),
    }
}

#[cfg(all(test, feature = "qdrant"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn qdrant_probe_reports_closed_port_as_unavailable() {
        // Port 1 on loopback is never a Qdrant server; the probe must not hang or panic.
        let outcome = probe_qdrant("http://127.0.0.1:1", Duration::from_millis(500)).await;
        assert!(
            matches!(outcome, ProbeOutcome::Unavailable(_)),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn qdrant_probe_reports_invalid_url_as_fatal() {
        let outcome = probe_qdrant("not a url", Duration::from_millis(500)).await;
        assert!(matches!(outcome, ProbeOutcome::Fatal(_)), "{outcome:?}");
    }
}
