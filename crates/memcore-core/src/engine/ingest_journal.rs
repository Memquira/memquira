//! The ingestion journal on the engine side (`ports::ingest_journal` has the
//! contract). A [`JournalSession`] records one add request's fact operations before they are
//! applied and commits them just before the request answers success; every other way out of the
//! request rolls them back:
//!
//! - an error: the request rolls back before it returns the error;
//! - a cancellation (client disconnect, request deadline): the session's drop spawns the rollback,
//!   so it no longer depends on the request future;
//! - a crash or restart (or a rollback that did not finish): the operation stays in the journal and
//!   [`MemoryEngine::recover_stale_ingest_operations`] rolls it back once it is stale.
//!
//! Every undo step is idempotent, so a rollback may be repeated or resumed at any point.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use memcore_common::{MemcoreError, MemcoreResult};
use serde::Serialize;
use uuid::Uuid;

use super::MemoryEngine;
use crate::AddMemoryInput;
use crate::audit::{build_delete_event, build_update_event, record_event_best_effort};
use crate::engine::AddMemoryOutput;
use crate::lifecycle::{LifecycleContext, normalize_operation, restore_fact};
use crate::observability::{
    record_ingest_idempotent_replay, record_ingest_inflight_wait, record_ingest_journal_backlog,
    record_ingest_journal_operation, record_ingest_journal_pending, record_ingest_recovery_sweep,
    record_ingest_recovery_sweep_duration, record_ingest_rollback, record_ingest_rollback_step,
};
use crate::ports::{IngestJournalStep, IngestJournalStore, IngestReceipt};
use crate::{Fact, FactOperation, FactOperationDecision, TenantContext};

/// Default for [`IngestJournalComponents::stale_after`].
pub const DEFAULT_INGEST_RECOVERY_STALE_AFTER: Duration = Duration::from_secs(300);
/// Default for [`IngestJournalComponents::inflight_wait`].
pub const DEFAULT_INGEST_INFLIGHT_WAIT: Duration = Duration::from_secs(10);
/// Default for [`IngestJournalComponents::receipt_ttl`].
pub const DEFAULT_INGEST_RECEIPT_TTL: Duration = Duration::from_secs(86_400);
/// Default batch of one recovery sweep.
pub const DEFAULT_INGEST_RECOVERY_BATCH: usize = 100;
/// How often a waiting request re-reads the other operation's state.
const INFLIGHT_POLL: Duration = Duration::from_millis(20);
/// Waits for other requests' uncommitted facts one candidate may take before the request fails.
const MAX_INFLIGHT_WAITS_PER_CANDIDATE: usize = 3;

/// The ingest journal and its timing.
#[derive(Clone)]
pub struct IngestJournalComponents {
    pub store: Arc<dyn IngestJournalStore>,
    /// An operation whose last journal write is older than this is abandoned (its request ended
    /// without committing or finishing its rollback: a crash, a restart or a lost rollback task)
    /// and the recovery sweep rolls it back. Must exceed the longest a write request can run (the
    /// write admission deadline).
    pub stale_after: Duration,
    /// Longest a request waits for another request's uncommitted fact before it fails with
    /// `409 CONFLICT` (also bounded by the request deadline).
    pub inflight_wait: Duration,
    /// How long the receipt of a successful request is kept for retries of that exact request
    /// (the recovery sweep purges older receipts).
    pub receipt_ttl: Duration,
}

impl IngestJournalComponents {
    pub fn new(store: Arc<dyn IngestJournalStore>) -> Self {
        Self {
            store,
            stale_after: DEFAULT_INGEST_RECOVERY_STALE_AFTER,
            inflight_wait: DEFAULT_INGEST_INFLIGHT_WAIT,
            receipt_ttl: DEFAULT_INGEST_RECEIPT_TTL,
        }
    }
}

/// closure: the stable identity of an add request, when it has one: the SHA-256 of
/// its session, messages (each with its client message id, role, name, content and time) and
/// metadata. `None` when any message lacks a client id (nothing identifies a retry then).
pub(super) fn ingest_request_key(input: &AddMemoryInput) -> Option<String> {
    use sha2::{Digest, Sha256};
    if input.messages.is_empty() || input.messages.iter().any(|m| m.message_id.is_none()) {
        return None;
    }
    let canonical = serde_json::to_vec(&(&input.session, &input.messages, &input.metadata)).ok()?;
    Some(format!("v1:{}", hex::encode(Sha256::digest(&canonical))))
}

/// The receipt's JSON: the result with its memories replaced by their ids (no fact text).
#[derive(Serialize, serde::Deserialize)]
struct ReceiptResult {
    output: AddMemoryOutput,
    memory_ids: Vec<Uuid>,
}

/// Rollback claims after which an operation still in the journal is reported stalled.
const STALLED_ATTEMPTS: u32 = 3;
/// Stale windows after which an operation still in the journal is reported stalled.
const STALLED_WINDOWS: u32 = 3;

/// closure: the state of ingestion recovery, for readiness and operators. It never
/// makes the service unready on its own: one stuck operation affects only the facts it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestRecoveryStatus {
    /// `idle` (empty journal), `recovering` (in-flight requests and rollbacks within their
    /// windows), `stalled` (an operation older than three stale windows or claimed three times:
    /// recovery keeps failing; check `ingest_rollback_failed` logs) or `unavailable` (the journal
    /// cannot be read).
    pub state: &'static str,
    pub pending: usize,
    pub rolling_back: usize,
    pub oldest_age_seconds: Option<u64>,
    pub max_attempts: u32,
}

/// Outcome of one recovery sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IngestRecoveryReport {
    /// Stale operations listed.
    pub examined: usize,
    pub rolled_back: usize,
    /// Rollbacks that did not finish (retried by a later sweep).
    pub failed: usize,
    /// Listed operations someone else claimed or resolved first.
    pub not_claimed: usize,
    /// Operations left in the journal after the sweep.
    pub pending_after: usize,
    /// Receipts older than the receipt TTL removed by this sweep.
    pub receipts_purged: usize,
}

/// One add request's journal (a no-op without an ingest journal).
pub(super) struct JournalSession<'a> {
    engine: &'a MemoryEngine,
    tenant: &'a TenantContext,
    operation_id: Uuid,
    /// The request's stable identity, for its receipt.
    request_key: Option<String>,
    /// The operation row exists (it is created with the first step).
    begun: bool,
    /// The steps recorded so far, for a rollback that cannot read them back.
    steps: Vec<IngestJournalStep>,
    /// Roll back on drop: cleared by a commit or an explicit rollback.
    armed: bool,
    /// A commit is in flight: if it is dropped now, the operation may or may not have committed.
    committing: bool,
}

impl<'a> JournalSession<'a> {
    pub(super) fn new(
        engine: &'a MemoryEngine,
        tenant: &'a TenantContext,
        request_key: Option<String>,
    ) -> Self {
        Self {
            engine,
            tenant,
            operation_id: Uuid::new_v4(),
            request_key,
            begun: false,
            steps: Vec::new(),
            armed: true,
            committing: false,
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.engine.ingest_journal.is_some()
    }

    /// Records `step` before the operation it describes is applied.
    pub(super) async fn record(&mut self, step: IngestJournalStep) -> MemcoreResult<()> {
        let Some(journal) = &self.engine.ingest_journal else {
            return Ok(());
        };
        if self.begun {
            journal
                .store
                .append(self.tenant, self.operation_id, &step)
                .await?;
        } else {
            journal
                .store
                .begin(self.tenant, self.operation_id, &step)
                .await?;
            self.begun = true;
        }
        self.steps.push(step);
        Ok(())
    }

    /// Records the step of a legacy-path `decision` and returns the id an ADD must use. An UPDATE
    /// keeps the target's current row as its before-image.
    pub(super) async fn record_decision(
        &mut self,
        ctx: &LifecycleContext<'_>,
        decision: &FactOperationDecision,
    ) -> MemcoreResult<Uuid> {
        let new_fact_id = Uuid::new_v4();
        if !self.enabled() {
            return Ok(new_fact_id);
        }
        match normalize_operation(decision)? {
            FactOperation::Add => {
                self.record(IngestJournalStep::Add {
                    fact_id: new_fact_id,
                })
                .await?
            }
            FactOperation::Update => {
                let target = decision
                    .target_fact_id
                    .expect("validated UPDATE has a target");
                // No row: the update fails with NotFound and writes nothing.
                if let Some(before) = ctx.fact_store.get_fact(self.tenant, target).await? {
                    self.record(IngestJournalStep::Update { before }).await?;
                }
            }
            FactOperation::Delete => {
                let target = decision
                    .target_fact_id
                    .expect("validated DELETE has a target");
                self.record(IngestJournalStep::Delete { fact_id: target })
                    .await?;
            }
            _ => {}
        }
        Ok(new_fact_id)
    }

    /// Records a supersession of `target` (by a fact this request adds as `replacement_fact_id`)
    /// with the target's current row as its before-image. `NotFound` when the target is gone.
    pub(super) async fn record_supersession(
        &mut self,
        ctx: &LifecycleContext<'_>,
        target: Uuid,
        replacement_fact_id: Option<Uuid>,
    ) -> MemcoreResult<()> {
        if !self.enabled() {
            return Ok(());
        }
        let before = ctx
            .fact_store
            .get_fact(self.tenant, target)
            .await?
            .ok_or_else(|| MemcoreError::NotFound("memory not found".to_string()))?;
        self.record(IngestJournalStep::Supersede {
            before,
            replacement_fact_id,
        })
        .await
    }

    /// Records an UPDATE of `target` with its current row as the before-image (consolidation v2).
    pub(super) async fn record_update(
        &mut self,
        ctx: &LifecycleContext<'_>,
        target: Uuid,
    ) -> MemcoreResult<()> {
        if !self.enabled() {
            return Ok(());
        }
        if let Some(before) = ctx.fact_store.get_fact(self.tenant, target).await? {
            self.record(IngestJournalStep::Update { before }).await?;
        }
        Ok(())
    }

    /// Another request's in-flight operation that writes `fact_id`, if any.
    pub(super) async fn foreign_in_flight(&self, fact_id: Uuid) -> MemcoreResult<Option<Uuid>> {
        match &self.engine.ingest_journal {
            None => Ok(None),
            Some(journal) => {
                journal
                    .store
                    .find_in_flight(self.tenant, fact_id, self.operation_id)
                    .await
            }
        }
    }

    /// Waits until the other operation `foreign` is resolved (committed or rolled back; an
    /// abandoned one is rolled back here). `Conflict` after too many waits for one candidate or when
    /// it does not resolve in time.
    pub(super) async fn await_in_flight(
        &self,
        foreign: Uuid,
        waits: &mut usize,
    ) -> MemcoreResult<()> {
        *waits += 1;
        if *waits > MAX_INFLIGHT_WAITS_PER_CANDIDATE {
            record_ingest_inflight_wait("timeout");
            return Err(in_flight_conflict());
        }
        self.engine.await_ingest_operation(foreign).await
    }

    /// Commits the operation (the request succeeded) with the receipt of `output` when the
    /// request has a stable identity. On an error the caller rolls back.
    pub(super) async fn commit(&mut self, output: &AddMemoryOutput) -> MemcoreResult<()> {
        let Some(journal) = &self.engine.ingest_journal else {
            self.armed = false;
            return Ok(());
        };
        let receipt = self.request_key.as_ref().and_then(|key| {
            let result = ReceiptResult {
                output: AddMemoryOutput {
                    memories: Vec::new(),
                    ..output.clone()
                },
                memory_ids: output.memories.iter().map(|fact| fact.id).collect(),
            };
            serde_json::to_string(&result)
                .ok()
                .map(|result| IngestReceipt {
                    request_key: key.clone(),
                    operation_id: self.begun.then_some(self.operation_id),
                    committed_at: Utc::now(),
                    result,
                })
        });
        if self.begun {
            self.committing = true;
            journal
                .store
                .commit(self.tenant, self.operation_id, receipt.as_ref())
                .await?;
            self.committing = false;
            record_ingest_journal_operation("committed");
        } else if let Some(receipt) = &receipt {
            // Nothing was journalled: the receipt alone. Best effort: without it a retry runs the
            // request again, which is what happened before receipts existed.
            if let Err(error) = journal.store.record_receipt(self.tenant, receipt).await {
                tracing::warn!(
                    event = "ingest_receipt_not_recorded",
                    error_code = error.code(),
                    "the receipt of a request without fact operations was not recorded"
                );
            }
        }
        self.armed = false;
        Ok(())
    }

    /// Rolls the operation back after `error`. A `Conflict` from the commit means a rollback had
    /// already claimed the operation (it did not commit); any other commit error leaves the
    /// outcome unknown and the journal decides.
    pub(super) async fn rollback(&mut self, error: &MemcoreError) {
        let after_commit_attempt = self.committing;
        self.committing = false;
        self.armed = false;
        if !self.begun {
            return;
        }
        let uncommitted = !after_commit_attempt || matches!(error, MemcoreError::Conflict(_));
        let steps = std::mem::take(&mut self.steps);
        self.engine
            .rollback_own_operation(self.tenant, self.operation_id, steps, "error", uncommitted)
            .await;
    }
}

impl Drop for JournalSession<'_> {
    fn drop(&mut self) {
        if !self.armed || !self.begun {
            return;
        }
        // Cancelled (dropped) before it committed or rolled back: roll back on a task of its own,
        // so the rollback does not depend on the lifetime of the request.
        let engine = self.engine.clone();
        let tenant = self.tenant.clone();
        let operation_id = self.operation_id;
        let steps = std::mem::take(&mut self.steps);
        let uncommitted = !self.committing;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    engine
                        .rollback_own_operation(
                            &tenant,
                            operation_id,
                            steps,
                            "cancelled",
                            uncommitted,
                        )
                        .await;
                });
            }
            Err(_) => {
                // No runtime (shutdown): the operation stays in the journal for the sweep.
                record_ingest_journal_operation("abandoned");
                tracing::warn!(
                    event = "ingest_rollback_abandoned",
                    "a cancelled write could not schedule its rollback; the recovery sweep will"
                );
            }
        }
    }
}

fn in_flight_conflict() -> MemcoreError {
    MemcoreError::Conflict(
        "another request is still writing this memory; retry the request".to_string(),
    )
}

/// What undoing one step did (the `result` metric label).
type StepResult = &'static str;

impl MemoryEngine {
    /// closure: the recorded result of a request that already succeeded, with its
    /// memories as they are stored now (facts deleted since are left out).
    pub(super) async fn replay_ingest_receipt(
        &self,
        tenant: &TenantContext,
        request_key: &str,
    ) -> MemcoreResult<Option<AddMemoryOutput>> {
        let Some(journal) = &self.ingest_journal else {
            return Ok(None);
        };
        let Some(receipt) = journal.store.find_receipt(tenant, request_key).await? else {
            return Ok(None);
        };
        let Ok(recorded) = serde_json::from_str::<ReceiptResult>(&receipt.result) else {
            // An unreadable receipt is treated as absent: the request runs again.
            return Ok(None);
        };
        let mut output = recorded.output;
        for id in recorded.memory_ids {
            if let Some(fact) = self.fact_store.get_fact(tenant, id).await? {
                output.memories.push(fact);
            }
        }
        record_ingest_idempotent_replay();
        Ok(Some(output))
    }

    /// Rolls back stale operations of the ingest journal (their requests ended
    /// without committing: a crash, a restart, a lost rollback task, or a rollback that failed).
    /// Safe to run from any number of processes at once: each operation is claimed atomically and
    /// every undo step is idempotent. A no-op without an ingest journal.
    pub async fn recover_stale_ingest_operations(
        &self,
        limit: usize,
    ) -> MemcoreResult<IngestRecoveryReport> {
        let Some(journal) = &self.ingest_journal else {
            return Ok(IngestRecoveryReport::default());
        };
        let started = Instant::now();
        let result = self.recovery_sweep(journal, limit).await;
        record_ingest_recovery_sweep_duration(started.elapsed().as_secs_f64());
        record_ingest_recovery_sweep(if result.is_ok() { "complete" } else { "error" });
        if let Ok(status) = self.ingest_recovery_status().await
            && let Some(status) = status
        {
            record_ingest_journal_backlog(
                status.oldest_age_seconds.unwrap_or(0) as f64,
                status.max_attempts,
            );
        }
        result
    }

    /// closure: the recovery state (`None` without an ingest journal).
    pub async fn ingest_recovery_status(&self) -> MemcoreResult<Option<IngestRecoveryStatus>> {
        let Some(journal) = &self.ingest_journal else {
            return Ok(None);
        };
        let backlog = match journal.store.backlog().await {
            Ok(backlog) => backlog,
            Err(_) => {
                return Ok(Some(IngestRecoveryStatus {
                    state: "unavailable",
                    pending: 0,
                    rolling_back: 0,
                    oldest_age_seconds: None,
                    max_attempts: 0,
                }));
            }
        };
        let oldest_age = backlog
            .oldest_updated_at
            .map(|at| (Utc::now() - at).num_seconds().max(0) as u64);
        let stalled_after = journal
            .stale_after
            .as_secs()
            .saturating_mul(u64::from(STALLED_WINDOWS));
        let state = if backlog.pending == 0 {
            "idle"
        } else if backlog.max_attempts >= STALLED_ATTEMPTS
            || oldest_age.is_some_and(|age| age > stalled_after)
        {
            "stalled"
        } else {
            "recovering"
        };
        Ok(Some(IngestRecoveryStatus {
            state,
            pending: backlog.pending,
            rolling_back: backlog.rolling_back,
            oldest_age_seconds: oldest_age,
            max_attempts: backlog.max_attempts,
        }))
    }

    async fn recovery_sweep(
        &self,
        journal: &IngestJournalComponents,
        limit: usize,
    ) -> MemcoreResult<IngestRecoveryReport> {
        let stale_before = stale_cutoff(journal.stale_after);
        let ids = journal.store.list_stale(stale_before, limit).await?;
        let mut report = IngestRecoveryReport {
            examined: ids.len(),
            ..Default::default()
        };
        for operation_id in ids {
            match self
                .rollback_claimed(journal, operation_id, Some(stale_before), "recovery_sweep")
                .await
            {
                Ok(true) => report.rolled_back += 1,
                Ok(false) => report.not_claimed += 1,
                Err(_) => report.failed += 1,
            }
        }
        report.pending_after = journal.store.count_operations().await?;
        let receipts_before = Utc::now()
            - chrono::Duration::from_std(journal.receipt_ttl)
                .unwrap_or_else(|_| chrono::Duration::days(36_500));
        match journal.store.purge_receipts(receipts_before).await {
            Ok(purged) => report.receipts_purged = purged,
            Err(error) => tracing::warn!(
                event = "ingest_receipt_purge_failed",
                error_code = error.code(),
                "old ingest receipts were not purged; the next sweep retries"
            ),
        }
        record_ingest_journal_pending(report.pending_after);
        if report.examined > 0 {
            tracing::warn!(
                event = "ingest_recovery_sweep",
                examined = report.examined,
                rolled_back = report.rolled_back,
                failed = report.failed,
                not_claimed = report.not_claimed,
                pending_after = report.pending_after,
                "stale ingestion operations were rolled back"
            );
        }
        Ok(report)
    }

    /// Claims `operation_id` (see [`IngestJournalStore::claim`]) and rolls it back. `Ok(false)`
    /// when it was not claimed; `Err` when the rollback did not finish (it is released for a
    /// later sweep).
    async fn rollback_claimed(
        &self,
        journal: &IngestJournalComponents,
        operation_id: Uuid,
        stale_before: Option<chrono::DateTime<Utc>>,
        trigger: &'static str,
    ) -> MemcoreResult<bool> {
        let Some(operation) = journal.store.claim(operation_id, stale_before).await? else {
            record_ingest_rollback(trigger, "not_claimed");
            return Ok(false);
        };
        let undone = match operation.tenant() {
            Ok(tenant) => self.undo_steps(&tenant, &operation.steps).await,
            Err(error) => Err(error),
        };
        match undone {
            Ok(()) => {
                journal.store.finish(operation_id).await?;
                record_ingest_rollback(trigger, "rolled_back");
                record_ingest_journal_operation("rolled_back");
                Ok(true)
            }
            Err(error) => {
                if let Err(release) = journal.store.release(operation_id).await {
                    tracing::warn!(
                        event = "ingest_rollback_release_failed",
                        error_code = release.code(),
                        "a failed rollback could not be released; it is retried once stale"
                    );
                }
                record_ingest_rollback(trigger, "failed");
                record_ingest_journal_operation("rollback_failed");
                tracing::warn!(
                    event = "ingest_rollback_failed",
                    trigger,
                    attempts = operation.attempts,
                    error_code = error.code(),
                    "an ingestion rollback did not finish; it is retried once stale"
                );
                Err(error)
            }
        }
    }

    /// The rollback of a request's own operation (after an error, or from its cancellation).
    /// `uncommitted` is false only when a commit may have gone through (it was interrupted or its
    /// outcome is unknown); then nothing is undone unless the journal still holds the operation.
    pub(super) async fn rollback_own_operation(
        &self,
        tenant: &TenantContext,
        operation_id: Uuid,
        steps: Vec<IngestJournalStep>,
        trigger: &'static str,
        uncommitted: bool,
    ) {
        let Some(journal) = &self.ingest_journal else {
            return;
        };
        match self
            .rollback_claimed(journal, operation_id, None, trigger)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                // Not claimed: committed, or a recovery claimed it first. In the second case that
                // rollback read the steps when it claimed, so undo this request's own steps too (a
                // step applied after that read would otherwise stay).
                let held = match journal.store.operation_state(operation_id).await {
                    Ok(state) => state.is_some(),
                    Err(_) => true,
                };
                if uncommitted || held {
                    let _ = self.undo_steps(tenant, &steps).await;
                }
            }
            Err(_) => {
                // The journal or a store failed: undo what this request applied (idempotent); the
                // operation stays in the journal, so the sweep resumes or repeats the rollback.
                if uncommitted {
                    let _ = self.undo_steps(tenant, &steps).await;
                }
            }
        }
    }

    /// Waits for another operation of the same tenant to resolve; rolls it back here when it is
    /// abandoned (stale). `Conflict` when it does not resolve within the wait bound.
    async fn await_ingest_operation(&self, foreign: Uuid) -> MemcoreResult<()> {
        let Some(journal) = &self.ingest_journal else {
            return Ok(());
        };
        let started = Instant::now();
        let mut limit = journal.inflight_wait;
        if let Some(deadline) = crate::deadline::current_deadline() {
            limit = limit.min(deadline.saturating_duration_since(started));
        }
        loop {
            match journal.store.operation_state(foreign).await? {
                None => {
                    record_ingest_inflight_wait("resolved");
                    return Ok(());
                }
                Some(state) => {
                    let stale_before = stale_cutoff(journal.stale_after);
                    if state.updated_at < stale_before {
                        self.rollback_claimed(journal, foreign, Some(stale_before), "on_demand")
                            .await?;
                        record_ingest_inflight_wait("recovered");
                        return Ok(());
                    }
                }
            }
            if started.elapsed() >= limit {
                record_ingest_inflight_wait("timeout");
                return Err(in_flight_conflict());
            }
            tokio::time::sleep(INFLIGHT_POLL).await;
        }
    }

    /// Undoes `steps` in reverse order. Stops at the first step that fails (the rollback is
    /// retried later; every step is idempotent).
    async fn undo_steps(
        &self,
        tenant: &TenantContext,
        steps: &[IngestJournalStep],
    ) -> MemcoreResult<()> {
        let mut result = Ok(());
        for step in steps.iter().rev() {
            match self.undo_step(tenant, step).await {
                Ok(outcome) => record_ingest_rollback_step(step.kind(), outcome),
                Err(error) => {
                    record_ingest_rollback_step(step.kind(), "failed");
                    result = Err(error);
                    break;
                }
            }
        }
        self.invalidate_user_context_cache(tenant).await;
        result
    }

    async fn undo_step(
        &self,
        tenant: &TenantContext,
        step: &IngestJournalStep,
    ) -> MemcoreResult<StepResult> {
        match step {
            IngestJournalStep::Add { fact_id } => self.undo_add(tenant, *fact_id).await,
            IngestJournalStep::Update { before } => self.undo_update(tenant, before).await,
            IngestJournalStep::Supersede {
                before,
                replacement_fact_id,
            } => {
                // The target first: an interruption between the two leaves both statements active
                // (repaired on resume), never none.
                let target = self
                    .undo_supersession_target(tenant, before, *replacement_fact_id)
                    .await?;
                let replacement = match replacement_fact_id {
                    Some(id) => self.undo_add(tenant, *id).await?,
                    None => "noop",
                };
                Ok(if target == "noop" {
                    replacement
                } else {
                    target
                })
            }
            IngestJournalStep::Delete { fact_id } => {
                self.roll_forward_delete(tenant, *fact_id).await
            }
        }
    }

    /// Removes a fact the operation added: soft-deletes the row (an audit record stays; the exact
    /// dedup key is released), removes its vector and entity links.
    async fn undo_add(&self, tenant: &TenantContext, fact_id: Uuid) -> MemcoreResult<StepResult> {
        let live = self.fact_store.get_fact(tenant, fact_id).await?;
        if let Some(fact) = &live {
            match self.fact_store.soft_delete_fact(tenant, fact_id).await {
                Ok(()) | Err(MemcoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
            // Recorded with the row's deletion: a resumed rollback finds the row already gone.
            record_event_best_effort(
                &self.event_store,
                tenant,
                build_delete_event(
                    tenant,
                    fact,
                    &self.audit_provider_name,
                    &self.audit_model_name,
                    serde_json::json!({ "reason": "ingest_rollback" }),
                ),
            )
            .await;
        }
        // Always: a previous attempt may have deleted the row but not the point.
        match self.vector_store.delete_by_fact_id(tenant, fact_id).await {
            Ok(()) | Err(MemcoreError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
        self.delete_entity_data_for_facts(tenant, &[fact_id])
            .await?;
        Ok(if live.is_some() { "undone" } else { "noop" })
    }

    /// Restores the before-image of an in-place update (row and vector) when the row changed.
    async fn undo_update(
        &self,
        tenant: &TenantContext,
        before: &Fact,
    ) -> MemcoreResult<StepResult> {
        let Some(current) = self.fact_store.get_fact(tenant, before.id).await? else {
            // Deleted since: nothing to restore.
            return Ok("noop");
        };
        if &current == before {
            return Ok("noop");
        }
        let ctx = self.lifecycle_context();
        let restored = restore_fact(&ctx, tenant, before.clone()).await?;
        self.record_rollback_update_event(tenant, &current, &restored)
            .await;
        Ok("undone")
    }

    /// Restores a superseded target's row (its vector is not changed by a supersession) unless a
    /// different replacement superseded it since.
    async fn undo_supersession_target(
        &self,
        tenant: &TenantContext,
        before: &Fact,
        replacement: Option<Uuid>,
    ) -> MemcoreResult<StepResult> {
        let Some(current) = self.fact_store.get_fact(tenant, before.id).await? else {
            return Ok("noop");
        };
        if &current == before {
            return Ok("noop");
        }
        if current.superseded_by.is_some() && current.superseded_by != replacement {
            return Ok("kept_modified");
        }
        let restored = self.fact_store.update_fact(tenant, before.clone()).await?;
        self.record_rollback_update_event(tenant, &current, &restored)
            .await;
        Ok("undone")
    }

    /// A soft delete cannot be undone: once the row is deleted, make sure its vector is gone too.
    async fn roll_forward_delete(
        &self,
        tenant: &TenantContext,
        fact_id: Uuid,
    ) -> MemcoreResult<StepResult> {
        match self
            .fact_store
            .get_fact_including_deleted(tenant, fact_id)
            .await?
        {
            Some(state) if state.deleted => {
                match self.vector_store.delete_by_fact_id(tenant, fact_id).await {
                    Ok(()) | Err(MemcoreError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                }
                self.delete_entity_data_for_facts(tenant, &[fact_id])
                    .await?;
                Ok("rolled_forward")
            }
            // Not deleted (the delete never ran) or not this tenant's row.
            _ => Ok("noop"),
        }
    }

    async fn record_rollback_update_event(
        &self,
        tenant: &TenantContext,
        current: &Fact,
        restored: &Fact,
    ) {
        record_event_best_effort(
            &self.event_store,
            tenant,
            build_update_event(
                tenant,
                current,
                restored,
                &self.audit_provider_name,
                &self.audit_model_name,
                serde_json::json!({ "reason": "ingest_rollback" }),
            ),
        )
        .await;
    }

    fn lifecycle_context(&self) -> LifecycleContext<'_> {
        LifecycleContext {
            fact_store: self.fact_store.as_ref(),
            vector_store: self.vector_store.as_ref(),
            embedding_provider: self.embedding_provider.as_ref(),
        }
    }
}

fn stale_cutoff(stale_after: Duration) -> chrono::DateTime<Utc> {
    Utc::now()
        - chrono::Duration::from_std(stale_after).unwrap_or_else(|_| chrono::Duration::days(36_500))
}
