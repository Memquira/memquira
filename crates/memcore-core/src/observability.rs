//! Passive stage timings. Recording is a no-op unless the API installed a metrics
//! recorder. Every label value is a fixed name from the code: never a tenant, user, query, text,
//! model or URL.

use std::future::Future;
use std::time::Instant;

/// Histogram of stage durations, labelled `path` (`search` / `ingest`) and `stage`.
pub const STAGE_DURATION_METRIC: &str = "memcore_stage_duration_seconds";

/// Search stages (the candidates endpoint runs the fact and episode legs concurrently).
pub const SEARCH_STAGES: &[&str] = &[
    "fact_query_embed",
    "fact_vector_search",
    "fact_sql_fetch",
    "fact_merge",
    "episode_query_embed",
    "episode_vector_search",
    "episode_sql_fetch",
    "episode_merge",
    "d76_render",
];

/// Ingest stages (legacy consolidation path, the shipped default).
pub const INGEST_STAGES: &[&str] = &[
    "episode_write",
    "episode_embed",
    "extract",
    "fact_embed",
    "dedup_classify",
    "fact_write",
];

/// Counter of vector hits that SQL did not confirm, labelled `path` and
/// `reason`. Only the fixed values below are ever used, so the series set is bounded.
pub const VECTOR_HIT_REJECTED_METRIC: &str = "memcore_vector_hits_rejected_total";

/// `path` values: the semantic fact-search leg and the embedding-duplicate check of ingestion.
pub const VECTOR_HIT_PATHS: &[&str] = &["search", "dedup"];

/// `reason` values: the hit's live SQL fact exists but its text or memory type differs (a stale
/// vector), or no live SQL fact of the tenant exists for it (an orphan: absent, soft-deleted or
/// another tenant's id, which a tenant-scoped read cannot tell apart).
pub const VECTOR_HIT_REJECT_REASONS: &[&str] = &["stale_content", "no_live_row"];

/// Counts one vector hit rejected by the SQL authority check. Never records ids or text.
pub(crate) fn record_vector_hit_rejected(path: &'static str, reason: &'static str) {
    metrics::counter!(VECTOR_HIT_REJECTED_METRIC, "path" => path, "reason" => reason).increment(1);
}

/// Counter of write failures by path. One increment means one logical
/// operation that failed after it had committed a write to one store, so another store was left
/// without the matching write (a cross-store window that a retry or `verify-vectors` must close),
/// or one best-effort audit event that could not be stored (`path = "event"`; the operation itself
/// still succeeds). A failure before the operation's first write is not counted: nothing is out of
/// step. Each failure is counted once, by the operation that owns the window, never also by the
/// layers it passes through. Labelled `path` only, from the fixed values below. A counter is
/// observability, not a durable record: it does not survive a restart and repairs nothing.
pub const CROSS_STORE_WRITE_FAILURES_METRIC: &str = "memcore_cross_store_write_failures_total";

/// `path` values:
/// - `fact_add`: a new fact row is stored, its embedding or vector write failed;
/// - `fact_update`: a fact row is updated, its replacement vector (or its verification) failed;
/// - `fact_heal`: the R4 retry repair of a fact's vector failed;
/// - `fact_delete`: a fact row is soft-deleted, its vector or entity cleanup failed;
/// - `episode_index`: ingested episode rows are stored, their embedding, vectors or `embedded` flag
///   failed;
/// - `retention`: rows were pruned, their vector or entity cleanup failed;
/// - `forget_user`: the user's fact rows were deleted, a later deletion or audit erasure failed;
/// - `import`: an import failed after its first write (the replace deletion or a first insert);
/// - `event`: a best-effort audit event was not stored.
pub const CROSS_STORE_WRITE_PATHS: &[&str] = &[
    "fact_add",
    "fact_update",
    "fact_heal",
    "fact_delete",
    "episode_index",
    "retention",
    "forget_user",
    "import",
    "event",
];

/// Counts one write failure under `path` (a value of [`CROSS_STORE_WRITE_PATHS`]). Never records
/// ids, tenants, error text or content.
pub(crate) fn record_write_failure(path: &'static str) {
    debug_assert!(
        CROSS_STORE_WRITE_PATHS.contains(&path),
        "unknown path {path}"
    );
    metrics::counter!(CROSS_STORE_WRITE_FAILURES_METRIC, "path" => path).increment(1);
}

/// Returns `result` unchanged, counting a write failure under `path` when it is an error. For the
/// part of an operation that runs after its first committed write.
pub(crate) fn counted_write<T>(
    path: &'static str,
    result: memcore_common::MemcoreResult<T>,
) -> memcore_common::MemcoreResult<T> {
    if result.is_err() {
        record_write_failure(path);
    }
    result
}

/// Awaits `future` and records its wall-clock duration under `path` / `stage`.
pub(crate) async fn timed<T>(
    path: &'static str,
    stage: &'static str,
    future: impl Future<Output = T>,
) -> T {
    let started = Instant::now();
    let output = future.await;
    record_stage(path, stage, started);
    output
}

/// Counter of classification provider calls skipped on the legacy ingest path because the
/// candidate's related-fact lookup returned no fact
/// (`MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED`). No labels. A skipped call makes no provider
/// request, so it appears in no provider-usage record or provider metric; this counter is where it
/// is counted.
pub const CLASSIFICATION_SKIPPED_METRIC: &str = "memcore_ingest_classification_skipped_total";

/// Counts one skipped classification call. Never records ids, tenants or content.
/// A classifier Update / Delete the semantic matcher stored as an Add, by
/// reason (bounded: `target_not_offered`, `different_subject`, `event_is_history`,
/// `distinct_event_times`).
pub const RELATED_DECISION_DOWNGRADED_METRIC: &str =
    "memcore_ingest_related_decision_downgraded_total";

pub(crate) fn record_related_decision_downgraded(reason: &'static str) {
    metrics::counter!(RELATED_DECISION_DOWNGRADED_METRIC, "reason" => reason).increment(1);
}

pub(crate) fn record_classification_skipped() {
    metrics::counter!(CLASSIFICATION_SKIPPED_METRIC).increment(1);
}

/// Records the time since `started` under `path` / `stage`.
pub(crate) fn record_stage(path: &'static str, stage: &'static str, started: Instant) {
    metrics::histogram!(STAGE_DURATION_METRIC, "path" => path, "stage" => stage)
        .record(started.elapsed().as_secs_f64());
}

/// Facts removed by a failed write's compensation, and removals that failed.
pub const PARTIAL_WRITE_COMPENSATED_METRIC: &str = "memcore_ingest_partial_write_compensated_total";

pub fn record_partial_write_compensation(rolled_back: usize, failures: usize) {
    metrics::counter!(PARTIAL_WRITE_COMPENSATED_METRIC, "outcome" => "rolled_back")
        .increment(rolled_back as u64);
    metrics::counter!(PARTIAL_WRITE_COMPENSATED_METRIC, "outcome" => "failed")
        .increment(failures as u64);
}

/// Journalled ingestion operations by `outcome` (`committed`, `rolled_back`,
/// `rollback_failed`, `abandoned` — a rollback that could not even be claimed or started).
pub const INGEST_JOURNAL_OPERATIONS_METRIC: &str = "memcore_ingest_journal_operations_total";
/// Rollbacks by `trigger` (`error`, `cancelled`, `recovery_sweep`, `on_demand`) and
/// `result` (`rolled_back`, `failed`, `not_claimed`).
pub const INGEST_ROLLBACK_METRIC: &str = "memcore_ingest_rollback_total";
/// Rolled-back steps by `kind` (journal step kind) and `result` (`undone`, `noop`,
/// `kept_modified`, `rolled_forward`, `failed`).
pub const INGEST_ROLLBACK_STEPS_METRIC: &str = "memcore_ingest_rollback_steps_total";
/// Waits for another request's uncommitted fact by `result` (`resolved`,
/// `recovered`, `timeout`).
pub const INGEST_INFLIGHT_WAIT_METRIC: &str = "memcore_ingest_inflight_wait_total";
/// Operations in the ingest journal at the last recovery sweep.
pub const INGEST_JOURNAL_PENDING_METRIC: &str = "memcore_ingest_journal_pending";
/// Recovery sweeps by `result` (`complete`, `error`).
pub const INGEST_RECOVERY_SWEEPS_METRIC: &str = "memcore_ingest_recovery_sweeps_total";

pub const INGEST_JOURNAL_OUTCOMES: &[&str] =
    &["committed", "rolled_back", "rollback_failed", "abandoned"];
pub const INGEST_ROLLBACK_TRIGGERS: &[&str] =
    &["error", "cancelled", "recovery_sweep", "on_demand"];
pub const INGEST_ROLLBACK_RESULTS: &[&str] = &["rolled_back", "failed", "not_claimed"];
pub const INGEST_ROLLBACK_STEP_RESULTS: &[&str] = &[
    "undone",
    "noop",
    "kept_modified",
    "rolled_forward",
    "failed",
];
pub const INGEST_INFLIGHT_WAIT_RESULTS: &[&str] = &["resolved", "recovered", "timeout"];

pub fn record_ingest_journal_operation(outcome: &'static str) {
    metrics::counter!(INGEST_JOURNAL_OPERATIONS_METRIC, "outcome" => outcome).increment(1);
}

pub fn record_ingest_rollback(trigger: &'static str, result: &'static str) {
    metrics::counter!(INGEST_ROLLBACK_METRIC, "trigger" => trigger, "result" => result)
        .increment(1);
}

pub fn record_ingest_rollback_step(kind: &'static str, result: &'static str) {
    metrics::counter!(INGEST_ROLLBACK_STEPS_METRIC, "kind" => kind, "result" => result)
        .increment(1);
}

pub fn record_ingest_inflight_wait(result: &'static str) {
    metrics::counter!(INGEST_INFLIGHT_WAIT_METRIC, "result" => result).increment(1);
}

pub fn record_ingest_journal_pending(pending: usize) {
    metrics::gauge!(INGEST_JOURNAL_PENDING_METRIC).set(pending as f64);
}

/// closure: age in seconds of the oldest journalled operation at the last sweep (0
/// when the journal is empty), and the most rollback claims of one operation.
pub const INGEST_JOURNAL_OLDEST_AGE_METRIC: &str = "memcore_ingest_journal_oldest_age_seconds";
pub const INGEST_JOURNAL_MAX_ATTEMPTS_METRIC: &str = "memcore_ingest_journal_max_rollback_attempts";
/// closure: duration of one recovery sweep.
pub const INGEST_RECOVERY_SWEEP_SECONDS_METRIC: &str = "memcore_ingest_recovery_sweep_seconds";

pub fn record_ingest_journal_backlog(oldest_age_seconds: f64, max_attempts: u32) {
    metrics::gauge!(INGEST_JOURNAL_OLDEST_AGE_METRIC).set(oldest_age_seconds);
    metrics::gauge!(INGEST_JOURNAL_MAX_ATTEMPTS_METRIC).set(f64::from(max_attempts));
}

pub fn record_ingest_recovery_sweep_duration(seconds: f64) {
    metrics::histogram!(INGEST_RECOVERY_SWEEP_SECONDS_METRIC).record(seconds);
}

/// closure: add requests answered from the receipt of an earlier success.
pub const INGEST_IDEMPOTENT_REPLAYS_METRIC: &str = "memcore_ingest_idempotent_replays_total";

pub fn record_ingest_idempotent_replay() {
    metrics::counter!(INGEST_IDEMPOTENT_REPLAYS_METRIC).increment(1);
}

pub fn record_ingest_recovery_sweep(result: &'static str) {
    metrics::counter!(INGEST_RECOVERY_SWEEPS_METRIC, "result" => result).increment(1);
}
