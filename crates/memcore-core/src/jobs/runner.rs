use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use memcore_common::{MemcoreError, MemcoreResult};
use tokio::time::{MissedTickBehavior, interval, timeout};

use super::registry::BackgroundJob;
use super::retry::{BackgroundJobRetryPolicy, execute_background_job_with_retries_and_shutdown};
use super::types::{
    BackgroundJobDefinition, BackgroundJobKind, BackgroundJobRun, BackgroundJobSnapshot,
    BackgroundJobStatus,
};
use crate::ShutdownToken;
use crate::ports::{
    AcquiredJobLock, BackgroundJobLockStore, BackgroundJobRunStore, StoredBackgroundJobRun,
};

const DEFAULT_RECENT_RUN_LIMIT: usize = 100;

#[derive(Debug)]
struct BackgroundJobStateInner {
    last_runs: HashMap<BackgroundJobKind, DateTime<Utc>>,
    running: HashSet<BackgroundJobKind>,
    recent_runs: VecDeque<BackgroundJobRun>,
}

#[derive(Debug)]
pub struct InMemoryBackgroundJobState {
    inner: Mutex<BackgroundJobStateInner>,
    recent_run_limit: usize,
}

impl Default for InMemoryBackgroundJobState {
    fn default() -> Self {
        Self::new(DEFAULT_RECENT_RUN_LIMIT)
    }
}

impl InMemoryBackgroundJobState {
    pub fn new(recent_run_limit: usize) -> Self {
        Self {
            inner: Mutex::new(BackgroundJobStateInner {
                last_runs: HashMap::new(),
                running: HashSet::new(),
                recent_runs: VecDeque::new(),
            }),
            recent_run_limit: recent_run_limit.max(1),
        }
    }

    pub fn recent_runs(&self) -> Vec<BackgroundJobRun> {
        self.inner
            .lock()
            .expect("background job state mutex should not be poisoned")
            .recent_runs
            .iter()
            .cloned()
            .collect()
    }

    pub fn is_running(&self, kind: BackgroundJobKind) -> bool {
        self.inner
            .lock()
            .expect("background job state mutex should not be poisoned")
            .running
            .contains(&kind)
    }

    pub fn try_begin(&self, kind: BackgroundJobKind) -> bool {
        let mut inner = self
            .inner
            .lock()
            .expect("background job state mutex should not be poisoned");
        inner.running.insert(kind)
    }

    pub fn is_due(&self, definition: &BackgroundJobDefinition, now: DateTime<Utc>) -> bool {
        let inner = self
            .inner
            .lock()
            .expect("background job state mutex should not be poisoned");

        if inner.running.contains(&definition.kind) {
            return false;
        }

        let Some(last_run) = inner.last_runs.get(&definition.kind) else {
            return true;
        };

        let Ok(interval) = chrono::Duration::from_std(definition.interval) else {
            return false;
        };

        *last_run + interval <= now
    }

    pub fn record_run(&self, run: BackgroundJobRun) {
        let mut inner = self
            .inner
            .lock()
            .expect("background job state mutex should not be poisoned");
        inner.running.remove(&run.kind);
        inner.last_runs.insert(run.kind, run.started_at);
        inner.recent_runs.push_front(run);
        while inner.recent_runs.len() > self.recent_run_limit {
            inner.recent_runs.pop_back();
        }
    }
}

#[derive(Clone)]
pub struct BackgroundJobRunner {
    jobs_enabled: bool,
    runner_interval: Duration,
    jobs: HashMap<BackgroundJobKind, Arc<dyn BackgroundJob>>,
    state: Arc<InMemoryBackgroundJobState>,
    history_enabled: bool,
    run_store: Option<Arc<dyn BackgroundJobRunStore>>,
    lock_enabled: bool,
    lock_owner_id: String,
    lock_ttl: Duration,
    lock_store: Option<Arc<dyn BackgroundJobLockStore>>,
    retry_policy: BackgroundJobRetryPolicy,
    shutdown_token: Option<ShutdownToken>,
    shutdown_timeout: Duration,
}

impl BackgroundJobRunner {
    pub fn new(
        jobs_enabled: bool,
        runner_interval: Duration,
        jobs: Vec<Arc<dyn BackgroundJob>>,
    ) -> Self {
        Self::with_state(
            jobs_enabled,
            runner_interval,
            jobs,
            Arc::new(InMemoryBackgroundJobState::default()),
        )
    }

    pub fn with_state(
        jobs_enabled: bool,
        runner_interval: Duration,
        jobs: Vec<Arc<dyn BackgroundJob>>,
        state: Arc<InMemoryBackgroundJobState>,
    ) -> Self {
        Self {
            jobs_enabled,
            runner_interval,
            jobs: jobs.into_iter().map(|job| (job.kind(), job)).collect(),
            state,
            history_enabled: false,
            run_store: None,
            lock_enabled: false,
            lock_owner_id: String::new(),
            lock_ttl: Duration::from_secs(300),
            lock_store: None,
            retry_policy: BackgroundJobRetryPolicy::disabled(),
            shutdown_token: None,
            shutdown_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_history_store(
        mut self,
        history_enabled: bool,
        run_store: Option<Arc<dyn BackgroundJobRunStore>>,
    ) -> Self {
        self.history_enabled = history_enabled;
        self.run_store = run_store;
        self
    }

    pub fn with_lock_store(
        mut self,
        lock_enabled: bool,
        owner_id: impl Into<String>,
        ttl: Duration,
        lock_store: Option<Arc<dyn BackgroundJobLockStore>>,
    ) -> Self {
        self.lock_enabled = lock_enabled;
        self.lock_owner_id = owner_id.into();
        self.lock_ttl = ttl;
        self.lock_store = lock_store;
        self
    }

    pub fn with_retry_policy(mut self, retry_policy: BackgroundJobRetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn with_shutdown(
        mut self,
        shutdown_token: ShutdownToken,
        shutdown_timeout: Duration,
    ) -> Self {
        self.shutdown_token = Some(shutdown_token);
        self.shutdown_timeout = shutdown_timeout.max(Duration::from_millis(1));
        self
    }

    pub fn snapshot(&self) -> BackgroundJobSnapshot {
        let mut jobs = self
            .jobs
            .values()
            .map(|job| job.definition())
            .collect::<Vec<_>>();
        jobs.sort_by_key(|definition| definition.kind.as_str());

        BackgroundJobSnapshot {
            jobs_enabled: self.jobs_enabled,
            jobs,
            recent_runs: self.state.recent_runs(),
        }
    }

    pub fn state(&self) -> Arc<InMemoryBackgroundJobState> {
        self.state.clone()
    }

    pub async fn run_due_once(&self) -> Vec<BackgroundJobRun> {
        if !self.jobs_enabled {
            return Vec::new();
        }

        if self.is_shutdown_requested() {
            tracing::info!("background job scheduling skipped because shutdown is requested");
            return Vec::new();
        }

        let now = Utc::now();
        let mut runs = Vec::new();
        for job in self.jobs.values() {
            if self.is_shutdown_requested() {
                tracing::info!("background job scheduling stopped because shutdown is requested");
                break;
            }
            let definition = job.definition();
            if !definition.enabled || !self.state.is_due(&definition, now) {
                continue;
            }
            runs.push(self.run_job(job.clone()).await);
        }
        runs
    }

    pub async fn run_manual(&self, kind: BackgroundJobKind) -> MemcoreResult<BackgroundJobRun> {
        let job = self.jobs.get(&kind).cloned().ok_or_else(|| {
            MemcoreError::ValidationError(format!("job is not registered: {}", kind.as_str()))
        })?;
        Ok(self.run_job(job).await)
    }

    async fn run_job(&self, job: Arc<dyn BackgroundJob>) -> BackgroundJobRun {
        let kind = job.kind();
        if self.is_shutdown_requested() {
            let run = BackgroundJobRun::cancelled(kind, "shutdown requested");
            tracing::warn!(
                job_kind = %kind,
                status = run.status.as_str(),
                error_code = run.error_code.as_deref(),
                "background job cancelled before start"
            );
            self.record_run(run.clone()).await;
            return run;
        }

        if !self.state.try_begin(kind) {
            let run = BackgroundJobRun::skipped(kind, "job is already running");
            tracing::warn!(
                job_kind = %kind,
                status = BackgroundJobStatus::Skipped.as_str(),
                "background job overlapping run skipped"
            );
            self.record_run(run.clone()).await;
            return run;
        }

        let acquired_lock = match self.acquire_lock(kind).await {
            Ok(lock) => lock,
            Err(run) => {
                self.record_run(run.clone()).await;
                return run;
            }
        };

        tracing::info!(
            job_kind = %kind,
            max_attempts = self.retry_policy.total_attempts(),
            retries_enabled = self.retry_policy.enabled,
            "background job started"
        );
        let retry_policy = self.retry_policy.clone();
        let run = match timeout(
            job.interval(),
            execute_background_job_with_retries_and_shutdown(
                kind,
                &retry_policy,
                self.shutdown_token.as_ref().map(ShutdownToken::child_token),
                self.shutdown_timeout,
                || {
                    let job = job.clone();
                    async move { job.run_once().await }
                },
            ),
        )
        .await
        {
            Ok(run) => run,
            Err(_) => {
                let mut run = BackgroundJobRun::failed(kind, "TIMEOUT", "background job timed out");
                run.max_attempts = self.retry_policy.total_attempts();
                run
            }
        };

        let status = run.status;
        tracing::info!(
            job_kind = %kind,
            status = status.as_str(),
            duration_ms = run.duration_ms,
            attempt_count = run.attempt_count,
            max_attempts = run.max_attempts,
            retried = run.retried,
            org_count = run.org_count,
            affected_count = run.affected_count,
            error_code = run.error_code.as_deref(),
            "background job finished"
        );
        self.release_lock(kind, acquired_lock).await;
        self.record_run(run.clone()).await;
        run
    }

    async fn acquire_lock(
        &self,
        kind: BackgroundJobKind,
    ) -> Result<Option<AcquiredJobLock>, BackgroundJobRun> {
        if !self.lock_enabled {
            return Ok(None);
        }

        let Some(store) = &self.lock_store else {
            tracing::warn!(
                job_kind = %kind,
                status = BackgroundJobStatus::Skipped.as_str(),
                error_code = "JOB_LOCK_STORE_NOT_CONFIGURED",
                "background job skipped because distributed lock store is not configured"
            );
            let mut run =
                BackgroundJobRun::skipped(kind, "distributed job lock store is not configured");
            run.error_code = Some("JOB_LOCK_STORE_NOT_CONFIGURED".to_string());
            return Err(run);
        };

        tracing::debug!(
            job_kind = %kind,
            owner_id = %self.lock_owner_id,
            ttl_seconds = self.lock_ttl.as_secs(),
            "background job distributed lock acquire attempt"
        );
        match store
            .try_acquire_lock(kind, &self.lock_owner_id, self.lock_ttl)
            .await
        {
            Ok(Some(lock)) => {
                tracing::info!(
                    job_kind = %kind,
                    owner_id = %self.lock_owner_id,
                    locked_until = %lock.locked_until,
                    "background job distributed lock acquired"
                );
                Ok(Some(lock))
            }
            Ok(None) => {
                tracing::info!(
                    job_kind = %kind,
                    owner_id = %self.lock_owner_id,
                    status = BackgroundJobStatus::Skipped.as_str(),
                    "background job skipped because another owner holds distributed lock"
                );
                let mut run =
                    BackgroundJobRun::skipped(kind, "job is already running on another instance");
                run.error_code = Some("JOB_ALREADY_RUNNING".to_string());
                Err(run)
            }
            Err(error) => {
                tracing::warn!(
                    job_kind = %kind,
                    owner_id = %self.lock_owner_id,
                    status = BackgroundJobStatus::Skipped.as_str(),
                    error_code = error.code(),
                    "background job distributed lock acquisition failed"
                );
                let mut run = BackgroundJobRun::skipped(kind, "distributed job lock unavailable");
                run.error_code = Some("JOB_LOCK_UNAVAILABLE".to_string());
                Err(run)
            }
        }
    }

    async fn release_lock(&self, kind: BackgroundJobKind, acquired_lock: Option<AcquiredJobLock>) {
        if !self.lock_enabled || acquired_lock.is_none() {
            return;
        }

        let Some(store) = &self.lock_store else {
            return;
        };

        match store.release_lock(kind, &self.lock_owner_id).await {
            Ok(true) => {
                tracing::info!(
                    job_kind = %kind,
                    owner_id = %self.lock_owner_id,
                    shutdown_requested = self.is_shutdown_requested(),
                    "background job distributed lock released"
                );
            }
            Ok(false) => {
                tracing::warn!(
                    job_kind = %kind,
                    owner_id = %self.lock_owner_id,
                    error_code = "JOB_LOCK_RELEASE_NOT_OWNER",
                    "background job distributed lock release skipped"
                );
            }
            Err(error) => {
                tracing::warn!(
                    job_kind = %kind,
                    owner_id = %self.lock_owner_id,
                    error_code = error.code(),
                    "background job distributed lock release failed"
                );
            }
        }
    }

    async fn record_run(&self, run: BackgroundJobRun) {
        self.state.record_run(run.clone());
        if !self.history_enabled {
            return;
        }

        let Some(store) = &self.run_store else {
            return;
        };

        let stored = StoredBackgroundJobRun::from(run.clone());
        match store.insert_run(stored).await {
            Ok(_) => {
                tracing::info!(
                    job_kind = %run.kind,
                    status = run.status.as_str(),
                    duration_ms = run.duration_ms,
                    run_id = %run.id,
                    "background job run persisted"
                );
            }
            Err(error) => {
                tracing::warn!(
                    job_kind = %run.kind,
                    status = run.status.as_str(),
                    duration_ms = run.duration_ms,
                    run_id = %run.id,
                    error_code = error.code(),
                    "background job history persistence failed"
                );
            }
        }
    }

    pub async fn run_forever(&self) {
        if !self.jobs_enabled {
            tracing::info!("background job runner disabled");
            return;
        }

        tracing::info!(
            runner_interval_seconds = self.runner_interval.as_secs(),
            "background job runner started"
        );

        let mut ticker = interval(self.runner_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            match &self.shutdown_token {
                Some(token) => {
                    tokio::select! {
                        _ = token.cancelled() => {
                            tracing::info!(
                                timeout_seconds = self.shutdown_timeout.as_secs(),
                                "background job runner shutdown started"
                            );
                            break;
                        }
                        _ = ticker.tick() => {
                            let _ = self.run_due_once().await;
                        }
                    }
                }
                None => {
                    ticker.tick().await;
                    let _ = self.run_due_once().await;
                }
            }
        }

        tracing::info!("background job runner stopped");
    }

    fn is_shutdown_requested(&self) -> bool {
        self.shutdown_token
            .as_ref()
            .is_some_and(ShutdownToken::is_cancelled)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::ports::{
        BackgroundJobLockStore, BackgroundJobRunQuery, BackgroundJobRunQueryResult,
        BackgroundJobRunStore, JobLockRecord, StoredBackgroundJobRun, lock_until_from_ttl,
    };

    struct TestJob {
        kind: BackgroundJobKind,
        enabled: bool,
        interval: Duration,
        runs: Arc<AtomicUsize>,
        fail: bool,
    }

    struct FlakyJob {
        kind: BackgroundJobKind,
        enabled: bool,
        interval: Duration,
        runs: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        error: MemcoreError,
        lock_store: Option<Arc<CapturingLockStore>>,
    }

    struct SlowJob {
        kind: BackgroundJobKind,
        enabled: bool,
        interval: Duration,
        runs: Arc<AtomicUsize>,
        sleep_for: Duration,
    }

    #[derive(Debug, Default)]
    struct CapturingRunStore {
        runs: Mutex<Vec<StoredBackgroundJobRun>>,
        fail_insert: bool,
    }

    #[derive(Debug, Default)]
    struct CapturingLockStore {
        owner_id: Mutex<Option<String>>,
        acquire_count: AtomicUsize,
        release_count: AtomicUsize,
        fail_release: bool,
    }

    #[async_trait]
    impl BackgroundJobRunStore for CapturingRunStore {
        async fn insert_run(
            &self,
            run: StoredBackgroundJobRun,
        ) -> MemcoreResult<StoredBackgroundJobRun> {
            if self.fail_insert {
                return Err(MemcoreError::StorageError(
                    "test persistence failure".to_string(),
                ));
            }
            self.runs
                .lock()
                .expect("capturing store mutex should not be poisoned")
                .push(run.clone());
            Ok(run)
        }

        async fn query_runs(
            &self,
            _query: BackgroundJobRunQuery,
        ) -> MemcoreResult<BackgroundJobRunQueryResult> {
            Ok(BackgroundJobRunQueryResult {
                runs: self
                    .runs
                    .lock()
                    .expect("capturing store mutex should not be poisoned")
                    .clone(),
                next_cursor: None,
            })
        }

        async fn delete_runs_older_than(
            &self,
            _cutoff: DateTime<Utc>,
            _dry_run: bool,
        ) -> MemcoreResult<usize> {
            Ok(0)
        }
    }

    #[async_trait]
    impl BackgroundJobLockStore for CapturingLockStore {
        async fn try_acquire_lock(
            &self,
            kind: BackgroundJobKind,
            owner_id: &str,
            ttl: Duration,
        ) -> MemcoreResult<Option<AcquiredJobLock>> {
            self.acquire_count.fetch_add(1, Ordering::SeqCst);
            let mut current = self
                .owner_id
                .lock()
                .expect("capturing lock store mutex should not be poisoned");
            if current.as_deref().is_some_and(|owner| owner != owner_id) {
                return Ok(None);
            }
            *current = Some(owner_id.to_string());
            Ok(Some(AcquiredJobLock {
                kind,
                owner_id: owner_id.to_string(),
                locked_until: lock_until_from_ttl(Utc::now(), ttl),
            }))
        }

        async fn renew_lock(
            &self,
            kind: BackgroundJobKind,
            owner_id: &str,
            ttl: Duration,
        ) -> MemcoreResult<bool> {
            let _ = (kind, ttl);
            Ok(self
                .owner_id
                .lock()
                .expect("capturing lock store mutex should not be poisoned")
                .as_deref()
                == Some(owner_id))
        }

        async fn release_lock(
            &self,
            kind: BackgroundJobKind,
            owner_id: &str,
        ) -> MemcoreResult<bool> {
            let _ = kind;
            if self.fail_release {
                return Err(MemcoreError::StorageError(
                    "test release failure".to_string(),
                ));
            }
            let mut current = self
                .owner_id
                .lock()
                .expect("capturing lock store mutex should not be poisoned");
            if current.as_deref() != Some(owner_id) {
                return Ok(false);
            }
            *current = None;
            self.release_count.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }

        async fn get_lock(&self, kind: BackgroundJobKind) -> MemcoreResult<Option<JobLockRecord>> {
            let owner = self
                .owner_id
                .lock()
                .expect("capturing lock store mutex should not be poisoned")
                .clone();
            Ok(owner.map(|owner_id| JobLockRecord {
                kind,
                owner_id,
                locked_until: Utc::now() + chrono::Duration::seconds(60),
                acquired_at: Utc::now(),
                heartbeat_at: None,
            }))
        }
    }

    #[async_trait]
    impl BackgroundJob for TestJob {
        fn kind(&self) -> BackgroundJobKind {
            self.kind
        }

        fn interval(&self) -> Duration {
            self.interval
        }

        fn enabled(&self) -> bool {
            self.enabled
        }

        async fn run_once(&self) -> MemcoreResult<BackgroundJobRun> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(MemcoreError::Internal("test failure".to_string()));
            }
            Ok(BackgroundJobRun::running(self.kind).finish(BackgroundJobStatus::Succeeded))
        }
    }

    #[async_trait]
    impl BackgroundJob for FlakyJob {
        fn kind(&self) -> BackgroundJobKind {
            self.kind
        }

        fn interval(&self) -> Duration {
            self.interval
        }

        fn enabled(&self) -> bool {
            self.enabled
        }

        async fn run_once(&self) -> MemcoreResult<BackgroundJobRun> {
            if let Some(lock_store) = &self.lock_store {
                assert!(
                    lock_store
                        .owner_id
                        .lock()
                        .expect("capturing lock store mutex should not be poisoned")
                        .is_some(),
                    "lock should be held while retry attempts execute"
                );
            }
            let attempt = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                return Err(self.error.clone());
            }
            Ok(BackgroundJobRun::running(self.kind).finish(BackgroundJobStatus::Succeeded))
        }
    }

    #[async_trait]
    impl BackgroundJob for SlowJob {
        fn kind(&self) -> BackgroundJobKind {
            self.kind
        }

        fn interval(&self) -> Duration {
            self.interval
        }

        fn enabled(&self) -> bool {
            self.enabled
        }

        async fn run_once(&self) -> MemcoreResult<BackgroundJobRun> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.sleep_for).await;
            Ok(BackgroundJobRun::running(self.kind).finish(BackgroundJobStatus::Succeeded))
        }
    }

    fn retry_policy(max_retries: usize) -> BackgroundJobRetryPolicy {
        BackgroundJobRetryPolicy {
            enabled: true,
            max_retries,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
            backoff_multiplier: 1.0,
            jitter_enabled: false,
        }
    }

    #[test]
    fn state_starts_empty_and_tracks_running() {
        let state = InMemoryBackgroundJobState::default();
        assert!(state.recent_runs().is_empty());
        assert!(!state.is_running(BackgroundJobKind::MemoryUsageSnapshot));
        assert!(state.try_begin(BackgroundJobKind::MemoryUsageSnapshot));
        assert!(state.is_running(BackgroundJobKind::MemoryUsageSnapshot));
        assert!(!state.try_begin(BackgroundJobKind::MemoryUsageSnapshot));
        assert!(state.try_begin(BackgroundJobKind::ProviderUsageRetention));
    }

    #[test]
    fn state_records_runs_and_respects_recent_limit() {
        let state = InMemoryBackgroundJobState::new(2);
        for _ in 0..3 {
            assert!(state.try_begin(BackgroundJobKind::MemoryUsageSnapshot));
            state.record_run(
                BackgroundJobRun::running(BackgroundJobKind::MemoryUsageSnapshot)
                    .finish(BackgroundJobStatus::Succeeded),
            );
        }

        assert_eq!(state.recent_runs().len(), 2);
        assert!(!state.is_running(BackgroundJobKind::MemoryUsageSnapshot));
    }

    #[tokio::test]
    async fn disabled_runner_does_not_run_jobs() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: true,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        );

        assert!(runner.run_due_once().await.is_empty());
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn enabled_runner_runs_due_jobs_and_skips_disabled_jobs() {
        let enabled_runs = Arc::new(AtomicUsize::new(0));
        let disabled_runs = Arc::new(AtomicUsize::new(0));
        let runner = BackgroundJobRunner::new(
            true,
            Duration::from_millis(10),
            vec![
                Arc::new(TestJob {
                    kind: BackgroundJobKind::MemoryUsageSnapshot,
                    enabled: true,
                    interval: Duration::from_secs(1),
                    runs: enabled_runs.clone(),
                    fail: false,
                }),
                Arc::new(TestJob {
                    kind: BackgroundJobKind::ProviderUsageRetention,
                    enabled: false,
                    interval: Duration::from_secs(1),
                    runs: disabled_runs.clone(),
                    fail: false,
                }),
            ],
        );

        let runs = runner.run_due_once().await;
        assert_eq!(runs.len(), 1);
        assert_eq!(enabled_runs.load(Ordering::SeqCst), 1);
        assert_eq!(disabled_runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failed_job_does_not_stop_runner() {
        let failed_runs = Arc::new(AtomicUsize::new(0));
        let successful_runs = Arc::new(AtomicUsize::new(0));
        let runner = BackgroundJobRunner::new(
            true,
            Duration::from_millis(10),
            vec![
                Arc::new(TestJob {
                    kind: BackgroundJobKind::MemoryUsageSnapshot,
                    enabled: true,
                    interval: Duration::from_secs(1),
                    runs: failed_runs.clone(),
                    fail: true,
                }),
                Arc::new(TestJob {
                    kind: BackgroundJobKind::ProviderUsageRetention,
                    enabled: true,
                    interval: Duration::from_secs(1),
                    runs: successful_runs.clone(),
                    fail: false,
                }),
            ],
        );

        let runs = runner.run_due_once().await;
        assert_eq!(runs.len(), 2);
        assert_eq!(failed_runs.load(Ordering::SeqCst), 1);
        assert_eq!(successful_runs.load(Ordering::SeqCst), 1);
        assert!(
            runs.iter()
                .any(|run| run.status == BackgroundJobStatus::Failed)
        );
        assert!(
            runs.iter()
                .any(|run| run.status == BackgroundJobStatus::Succeeded)
        );
    }

    #[tokio::test]
    async fn overlapping_same_job_is_skipped() {
        let runner = BackgroundJobRunner::new(true, Duration::from_millis(10), Vec::new());
        assert!(
            runner
                .state
                .try_begin(BackgroundJobKind::MemoryUsageSnapshot)
        );

        let run = runner
            .run_job(Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: true,
                interval: Duration::from_secs(1),
                runs: Arc::new(AtomicUsize::new(0)),
                fail: false,
            }))
            .await;

        assert_eq!(run.status, BackgroundJobStatus::Skipped);
    }

    #[tokio::test]
    async fn manual_run_works_when_global_runner_is_disabled() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run should work");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_manual_run_is_persisted_when_history_store_is_configured() {
        let runs = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(CapturingRunStore::default());
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        )
        .with_history_store(true, Some(store.clone()));

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run should work");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(
            store
                .runs
                .lock()
                .expect("capturing store mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn skipped_overlap_run_is_persisted() {
        let store = Arc::new(CapturingRunStore::default());
        let runner = BackgroundJobRunner::new(true, Duration::from_millis(10), Vec::new())
            .with_history_store(true, Some(store.clone()));
        assert!(
            runner
                .state
                .try_begin(BackgroundJobKind::MemoryUsageSnapshot)
        );

        let run = runner
            .run_job(Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: true,
                interval: Duration::from_secs(1),
                runs: Arc::new(AtomicUsize::new(0)),
                fail: false,
            }))
            .await;

        assert_eq!(run.status, BackgroundJobStatus::Skipped);
        assert_eq!(
            store
                .runs
                .lock()
                .expect("capturing store mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn persistence_failure_does_not_fail_job_execution_or_memory_state() {
        let runs = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(CapturingRunStore {
            runs: Mutex::new(Vec::new()),
            fail_insert: true,
        });
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs,
                fail: false,
            })],
        )
        .with_history_store(true, Some(store));

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("job execution should not fail due to persistence");

        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(runner.snapshot().recent_runs.len(), 1);
    }

    #[tokio::test]
    async fn distributed_lock_disabled_preserves_existing_manual_behavior() {
        let runs = Arc::new(AtomicUsize::new(0));
        let lock_store = Arc::new(CapturingLockStore::default());
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        )
        .with_lock_store(
            false,
            "owner-a",
            Duration::from_secs(60),
            Some(lock_store.clone()),
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.acquire_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn distributed_lock_acquired_and_released_on_success() {
        let runs = Arc::new(AtomicUsize::new(0));
        let lock_store = Arc::new(CapturingLockStore::default());
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        )
        .with_lock_store(
            true,
            "owner-a",
            Duration::from_secs(60),
            Some(lock_store.clone()),
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.acquire_count.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.release_count.load(Ordering::SeqCst), 1);
        assert!(lock_store.get_lock(run.kind).await.expect("lock").is_none());
    }

    #[tokio::test]
    async fn distributed_lock_skip_is_recorded_when_another_owner_holds_lock() {
        let runs = Arc::new(AtomicUsize::new(0));
        let run_store = Arc::new(CapturingRunStore::default());
        let lock_store = Arc::new(CapturingLockStore {
            owner_id: Mutex::new(Some("owner-b".to_string())),
            acquire_count: AtomicUsize::new(0),
            release_count: AtomicUsize::new(0),
            fail_release: false,
        });
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        )
        .with_history_store(true, Some(run_store.clone()))
        .with_lock_store(
            true,
            "owner-a",
            Duration::from_secs(60),
            Some(lock_store.clone()),
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Skipped);
        assert_eq!(run.error_code.as_deref(), Some("JOB_ALREADY_RUNNING"));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert_eq!(lock_store.release_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            run_store
                .runs
                .lock()
                .expect("capturing store mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn distributed_lock_released_after_job_failure_and_release_failure_is_non_fatal() {
        let runs = Arc::new(AtomicUsize::new(0));
        let lock_store = Arc::new(CapturingLockStore {
            owner_id: Mutex::new(None),
            acquire_count: AtomicUsize::new(0),
            release_count: AtomicUsize::new(0),
            fail_release: true,
        });
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: true,
            })],
        )
        .with_lock_store(
            true,
            "owner-a",
            Duration::from_secs(60),
            Some(lock_store.clone()),
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Failed);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.acquire_count.load(Ordering::SeqCst), 1);
        assert_eq!(runner.snapshot().recent_runs.len(), 1);
    }

    #[tokio::test]
    async fn scheduled_job_uses_retry_policy_and_persists_successful_retry() {
        let runs = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(CapturingRunStore::default());
        let runner = BackgroundJobRunner::new(
            true,
            Duration::from_millis(10),
            vec![Arc::new(FlakyJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: true,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail_until_attempt: 1,
                error: MemcoreError::StorageError("database is locked".to_string()),
                lock_store: None,
            })],
        )
        .with_retry_policy(retry_policy(2))
        .with_history_store(true, Some(store.clone()));

        let runs_result = runner.run_due_once().await;
        assert_eq!(runs_result.len(), 1);
        assert_eq!(runs_result[0].status, BackgroundJobStatus::Succeeded);
        assert_eq!(runs_result[0].attempt_count, 2);
        assert!(runs_result[0].retried);
        assert_eq!(runs.load(Ordering::SeqCst), 2);

        let persisted = store
            .runs
            .lock()
            .expect("capturing store mutex should not be poisoned");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].status, BackgroundJobStatus::Succeeded);
        assert_eq!(persisted[0].attempt_count, 2);
        assert!(persisted[0].retried);
    }

    #[tokio::test]
    async fn manual_job_uses_retry_policy() {
        let runs = Arc::new(AtomicUsize::new(0));
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(FlakyJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail_until_attempt: 1,
                error: MemcoreError::StorageError("service unavailable".to_string()),
                lock_store: None,
            })],
        )
        .with_retry_policy(retry_policy(2));

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(run.attempt_count, 2);
        assert!(run.retried);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn exhausted_retries_are_persisted_as_failed() {
        let runs = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(CapturingRunStore::default());
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(FlakyJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail_until_attempt: usize::MAX,
                error: MemcoreError::StorageError("service unavailable".to_string()),
                lock_store: None,
            })],
        )
        .with_retry_policy(retry_policy(2))
        .with_history_store(true, Some(store.clone()));

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Failed);
        assert_eq!(run.attempt_count, 3);
        assert_eq!(run.max_attempts, 3);
        assert!(run.retried);
        assert_eq!(runs.load(Ordering::SeqCst), 3);

        let persisted = store
            .runs
            .lock()
            .expect("capturing store mutex should not be poisoned");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].status, BackgroundJobStatus::Failed);
        assert_eq!(persisted[0].attempt_count, 3);
        assert!(persisted[0].retried);
    }

    #[tokio::test]
    async fn distributed_lock_is_held_for_full_retry_sequence() {
        let runs = Arc::new(AtomicUsize::new(0));
        let lock_store = Arc::new(CapturingLockStore::default());
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(FlakyJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail_until_attempt: 1,
                error: MemcoreError::StorageError("database is locked".to_string()),
                lock_store: Some(lock_store.clone()),
            })],
        )
        .with_retry_policy(retry_policy(2))
        .with_lock_store(
            true,
            "owner-a",
            Duration::from_secs(60),
            Some(lock_store.clone()),
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(run.attempt_count, 2);
        assert_eq!(lock_store.acquire_count.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.release_count.load(Ordering::SeqCst), 1);
        assert!(lock_store.get_lock(run.kind).await.expect("lock").is_none());
    }

    #[tokio::test]
    async fn lock_acquisition_failure_does_not_retry_job_body() {
        let runs = Arc::new(AtomicUsize::new(0));
        let lock_store = Arc::new(CapturingLockStore {
            owner_id: Mutex::new(Some("owner-b".to_string())),
            acquire_count: AtomicUsize::new(0),
            release_count: AtomicUsize::new(0),
            fail_release: false,
        });
        let runner = BackgroundJobRunner::new(
            false,
            Duration::from_millis(10),
            vec![Arc::new(FlakyJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: false,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail_until_attempt: 1,
                error: MemcoreError::StorageError("database is locked".to_string()),
                lock_store: Some(lock_store.clone()),
            })],
        )
        .with_retry_policy(retry_policy(2))
        .with_lock_store(
            true,
            "owner-a",
            Duration::from_secs(60),
            Some(lock_store.clone()),
        );

        let run = runner
            .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
            .await
            .expect("manual run");
        assert_eq!(run.status, BackgroundJobStatus::Skipped);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert_eq!(lock_store.acquire_count.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.release_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn runner_exits_when_shutdown_token_is_cancelled() {
        let token = ShutdownToken::new();
        let runner = BackgroundJobRunner::new(true, Duration::from_secs(60), Vec::new())
            .with_shutdown(token.child_token(), Duration::from_millis(10));

        let handle = tokio::spawn(async move {
            runner.run_forever().await;
        });
        token.cancel();

        tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .expect("runner should exit after shutdown")
            .expect("runner task should complete");
    }

    #[tokio::test]
    async fn runner_does_not_start_new_jobs_after_shutdown() {
        let token = ShutdownToken::new();
        token.cancel();
        let runs = Arc::new(AtomicUsize::new(0));
        let runner = BackgroundJobRunner::new(
            true,
            Duration::from_millis(10),
            vec![Arc::new(TestJob {
                kind: BackgroundJobKind::MemoryUsageSnapshot,
                enabled: true,
                interval: Duration::from_secs(1),
                runs: runs.clone(),
                fail: false,
            })],
        )
        .with_shutdown(token, Duration::from_millis(10));

        let result = runner.run_due_once().await;
        assert!(result.is_empty());
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn running_job_can_finish_during_shutdown() {
        let token = ShutdownToken::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(
            BackgroundJobRunner::new(
                false,
                Duration::from_millis(10),
                vec![Arc::new(SlowJob {
                    kind: BackgroundJobKind::MemoryUsageSnapshot,
                    enabled: false,
                    interval: Duration::from_secs(1),
                    runs: runs.clone(),
                    sleep_for: Duration::from_millis(10),
                })],
            )
            .with_shutdown(token.child_token(), Duration::from_millis(100)),
        );

        let runner_for_task = runner.clone();
        let handle = tokio::spawn(async move {
            runner_for_task
                .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
                .await
                .expect("manual run")
        });
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        token.cancel();

        let run = handle.await.expect("manual task should complete");
        assert_eq!(run.status, BackgroundJobStatus::Succeeded);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shutdown_timeout_prevents_indefinite_job_wait() {
        let token = ShutdownToken::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(
            BackgroundJobRunner::new(
                false,
                Duration::from_millis(10),
                vec![Arc::new(SlowJob {
                    kind: BackgroundJobKind::MemoryUsageSnapshot,
                    enabled: false,
                    interval: Duration::from_secs(60),
                    runs: runs.clone(),
                    sleep_for: Duration::from_secs(60),
                })],
            )
            .with_shutdown(token.child_token(), Duration::from_millis(5)),
        );

        let runner_for_task = runner.clone();
        let handle = tokio::spawn(async move {
            runner_for_task
                .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
                .await
                .expect("manual run")
        });
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        token.cancel();

        let run = tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .expect("shutdown timeout should stop waiting")
            .expect("manual task should complete");
        assert_eq!(run.status, BackgroundJobStatus::Cancelled);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_backoff_shutdown_records_cancelled_run_and_releases_lock() {
        let token = ShutdownToken::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let run_store = Arc::new(CapturingRunStore::default());
        let lock_store = Arc::new(CapturingLockStore::default());
        let runner = Arc::new(
            BackgroundJobRunner::new(
                false,
                Duration::from_millis(10),
                vec![Arc::new(FlakyJob {
                    kind: BackgroundJobKind::MemoryUsageSnapshot,
                    enabled: false,
                    interval: Duration::from_secs(60),
                    runs: runs.clone(),
                    fail_until_attempt: usize::MAX,
                    error: MemcoreError::StorageError("service unavailable".to_string()),
                    lock_store: Some(lock_store.clone()),
                })],
            )
            .with_retry_policy(BackgroundJobRetryPolicy {
                initial_backoff: Duration::from_secs(60),
                max_backoff: Duration::from_secs(60),
                jitter_enabled: false,
                ..retry_policy(2)
            })
            .with_history_store(true, Some(run_store.clone()))
            .with_lock_store(
                true,
                "owner-a",
                Duration::from_secs(60),
                Some(lock_store.clone()),
            )
            .with_shutdown(token.child_token(), Duration::from_millis(10)),
        );

        let runner_for_task = runner.clone();
        let handle = tokio::spawn(async move {
            runner_for_task
                .run_manual(BackgroundJobKind::MemoryUsageSnapshot)
                .await
                .expect("manual run")
        });
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        token.cancel();

        let run = tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .expect("shutdown should interrupt retry backoff")
            .expect("manual task should complete");
        assert_eq!(run.status, BackgroundJobStatus::Cancelled);
        assert_eq!(run.attempt_count, 1);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(lock_store.release_count.load(Ordering::SeqCst), 1);
        assert!(lock_store.get_lock(run.kind).await.expect("lock").is_none());

        let persisted = run_store
            .runs
            .lock()
            .expect("capturing store mutex should not be poisoned");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].status, BackgroundJobStatus::Cancelled);
    }
}
