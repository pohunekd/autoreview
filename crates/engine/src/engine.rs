//! The engine core: its state, the clock it reads, the retry of one workflow step and the claims
//! that keep one review on one engine.
//!
//! Mirrors `cmd/autoreview/engine.go:1-205`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::future::Future;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use autoreview_go_compat::go_time::parse_rfc3339;
use rustix::fs::{FlockOperation, flock};
use time::OffsetDateTime;
use tokio::task::JoinSet;
use tokio::time::{Interval, MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::discovery::poll::live;
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::metrics::Metrics;
use crate::model::review::APPROVAL_KIND_EXPIRED;
use crate::review::sessions::is_shutdown;
use crate::store::{REVIEW_QUEUED, REVIEW_RUNNING, ReviewFilter, Store};

/// Retry policy of one workflow step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StepRetryPolicy {
    /// Time that one failing step is retried in total.
    pub(crate) total: Duration,
    /// Delay before the first retry.
    pub(crate) initial: Duration,
    /// Largest delay between two attempts.
    pub(crate) max_interval: Duration,
}

/// Mode of the engine lock file, as Go's `os.OpenFile` mode (`engine.go:72`).
const ENGINE_LOCK_MODE: u32 = 0o600;

/// Step that stores the combined review of a workflow (`review.go:18`).
pub(crate) const STEP_ARTIFACT: &str = "artifact";

/// Step that stores the GitHub publication of a workflow (`review.go:23`).
pub(crate) const STEP_PUBLICATION: &str = "publication";

/// Time source of the engine: monotonic instants and waits between attempts.
pub trait Clock: Send + Sync {
    /// Returns the current instant of this clock.
    fn now(&self) -> Instant;

    /// Waits for `delay` on this clock.
    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send;

    /// Returns the current wall-clock time, which persisted deadlines are compared with.
    fn wall_now(&self) -> OffsetDateTime;
}

/// The clock of the running process: the monotonic clock and the Tokio timer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(delay)
    }

    fn wall_now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// The review engine. It holds the configuration, the store, the side effects and the set of
/// reviews this engine has claimed.
pub struct Engine<I, C> {
    /// Validated configuration; every timing and limit is read from here.
    pub(crate) config: Config,
    /// Store of the discovery state and the reviews.
    pub(crate) store: Arc<Store>,
    /// GitHub, pohunek and Matrix behind the trait.
    pub(crate) integration: Arc<I>,
    /// Time source of the step retries.
    pub(crate) clock: C,
    /// Workflow IDs of the reviews claimed by this engine.
    active: Mutex<BTreeSet<String>>,
    /// Review counters of the metrics endpoint; `None` unless `with_metrics` set them.
    pub(crate) metrics: Option<Metrics>,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Creates an engine from the configuration, the store, the integrations and the clock.
    pub fn new(config: Config, store: Arc<Store>, integration: Arc<I>, clock: C) -> Self {
        Self {
            config,
            store,
            integration,
            clock,
            active: Mutex::new(BTreeSet::new()),
            metrics: None,
        }
    }

    /// Returns the instant before which `prune` deletes completed reviews.
    pub(crate) fn prune_cutoff(&self) -> OffsetDateTime {
        // A retention beyond the range of `time::Duration` keeps every review.
        let retention =
            time::Duration::try_from(self.config.review_retention).unwrap_or(time::Duration::MAX);
        self.clock.wall_now().saturating_sub(retention)
    }

    /// Returns the retry policy of the workflow steps, read from the configuration.
    pub(crate) fn step_policy(&self) -> StepRetryPolicy {
        StepRetryPolicy {
            total: self.config.step_retry,
            initial: self.config.step_retry_initial,
            max_interval: self.config.step_retry_max_interval,
        }
    }

    /// Claims a review for this engine if it is free and under the limit.
    ///
    /// A review that is already claimed, or a full engine, is refused.
    pub(crate) fn claim(&self, workflow_id: &str) -> bool {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.contains(workflow_id) {
            return false;
        }
        let claimed = i64::try_from(active.len()).unwrap_or(i64::MAX);
        if claimed >= self.config.max_concurrent_reviews {
            return false;
        }
        active.insert(workflow_id.to_owned());
        true
    }

    /// Releases the claim of a review so that a later pass can run it again.
    pub(crate) fn release(&self, workflow_id: &str) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        active.remove(workflow_id);
    }
}

impl<I, C> Engine<I, C>
where
    I: Integrations + Send + Sync + 'static,
    C: Clock + 'static,
{
    /// Runs discovery and reviews until `cancel` is cancelled.
    ///
    /// A second engine on the same database is refused. Reviews still running at shutdown
    /// keep their status and resume on the next start.
    ///
    /// # Errors
    ///
    /// Returns `store_unavailable` when the engine lock file cannot be opened,
    /// `engine_already_running` when it is locked, and `configuration_invalid` without a policy.
    pub async fn serve(self: &Arc<Self>, cancel: &CancellationToken) -> Result<(), Error> {
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(ENGINE_LOCK_MODE)
            .open(self.store.engine_lock_path())
            .map_err(|err| Error::workflow("store_unavailable", true, Some(Box::new(err))))?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive)
            .map_err(|err| Error::workflow("engine_already_running", true, Some(Box::new(err))))?;
        let review_cancel = cancel.child_token();
        let mut tasks = JoinSet::new();
        let result = self.serve_locked(cancel, &review_cancel, &mut tasks).await;
        review_cancel.cancel();
        while let Some(joined) = tasks.join_next().await {
            resume_panic(joined);
        }
        // The flock is released only after every review stopped, as Go's deferred close.
        drop(lock);
        result
    }

    /// Runs the poll and worker tickers of [`Engine::serve`] while the engine lock is held.
    async fn serve_locked(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        review_cancel: &CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<(), Error> {
        let mut poll_ticker = ticker(self.config.poll_interval);
        let mut work_ticker = ticker(self.config.worker_tick);
        let policy_digest = self.policy()?.digest();
        tracing::info!(
            policy_digest,
            max_concurrent_reviews = self.config.max_concurrent_reviews,
            "pull request review engine starting"
        );
        self.poll_and_log(cancel).await;
        self.work(review_cancel, tasks);
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    tracing::info!("pull request review engine stopping");
                    return Ok(());
                }
                _ = poll_ticker.tick() => {
                    self.poll_and_log(cancel).await;
                    self.prune(cancel);
                }
                _ = work_ticker.tick() => {
                    while let Some(joined) = tasks.try_join_next() {
                        resume_panic(joined);
                    }
                    self.work(review_cancel, tasks);
                }
            }
        }
    }

    /// Runs one discovery poll and logs its failure or its dispatches.
    ///
    /// A failure after `cancel` is cancelled is not logged.
    async fn poll_and_log(&self, cancel: &CancellationToken) {
        match self.poll(cancel).await {
            Err(err) => {
                if !cancel.is_cancelled() {
                    tracing::error!(
                        error_code = error_code(&err),
                        error = %err,
                        "discovery poll failed"
                    );
                }
            }
            Ok(result) => {
                if result.dispatched > 0 || result.superseded > 0 {
                    tracing::info!(
                        open = result.open,
                        dispatched = result.dispatched,
                        superseded = result.superseded,
                        "discovery poll"
                    );
                }
            }
        }
    }

    /// Deletes the completed reviews last updated before the retention and logs the count.
    ///
    /// A failure after `cancel` is cancelled is not logged.
    fn prune(&self, cancel: &CancellationToken) {
        let cutoff = self.prune_cutoff();
        match live(cancel).and_then(|()| self.store.prune_reviews(cutoff)) {
            Err(err) => {
                if !cancel.is_cancelled() {
                    tracing::error!(error_code = error_code(&err), "review pruning failed");
                }
            }
            Ok(removed) => {
                if removed > 0 {
                    tracing::info!(reviews = removed, "completed reviews pruned");
                }
            }
        }
    }

    /// Expires overdue approval waits and starts every review that can make progress.
    ///
    /// Reviews start up to the concurrency limit; each runs in its own task in `tasks`.
    fn work(self: &Arc<Self>, cancel: &CancellationToken, tasks: &mut JoinSet<()>) {
        if cancel.is_cancelled() {
            return;
        }
        if let Err(err) = self.expire_approvals(cancel) {
            tracing::error!(error_code = error_code(&err), "approval expiry failed");
        }
        let ready = match live(cancel).and_then(|()| self.store.ready_reviews()) {
            Ok(ready) => ready,
            Err(err) => {
                tracing::error!(error_code = error_code(&err), "review queue unreadable");
                return;
            }
        };
        for mut stored in ready {
            if !self.claim(&stored.workflow_id) {
                continue;
            }
            if stored.status != REVIEW_RUNNING {
                let moved = live(cancel).and_then(|()| {
                    self.store.set_review_status(
                        &stored.workflow_id,
                        &stored.status,
                        REVIEW_RUNNING,
                    )
                });
                if !matches!(moved, Ok(true)) {
                    self.release(&stored.workflow_id);
                    continue;
                }
                // A decided wait also moves to running; only a move from the queue starts.
                if stored.status == REVIEW_QUEUED {
                    self.count_started(&stored);
                }
                REVIEW_RUNNING.clone_into(&mut stored.status);
            }
            let engine = Arc::clone(self);
            let cancel = cancel.clone();
            tasks.spawn(async move {
                let workflow_id = stored.workflow_id.clone();
                let review_id = stored.input.pull_request.review_id();
                let result = engine.run_review(&cancel, stored).await;
                match result {
                    Ok(()) => engine.count_completed(&workflow_id),
                    Err(err) if is_shutdown(&err) || cancel.is_cancelled() => {}
                    Err(err) => {
                        // The review keeps its status, so the next tick resumes it.
                        tracing::error!(
                            review_id,
                            workflow_id,
                            error_code = error_code(&err),
                            error = %err,
                            "review interrupted"
                        );
                    }
                }
                engine.release(&workflow_id);
            });
        }
    }

    /// Records the expiry of every awaiting review whose approval deadline has passed.
    ///
    /// # Errors
    ///
    /// Returns `store_state_invalid` for a deadline that does not parse, or the first error of
    /// the store.
    fn expire_approvals(&self, cancel: &CancellationToken) -> Result<(), Error> {
        live(cancel)?;
        let waiting = self.store.list_reviews(
            &ReviewFilter::AwaitingUndecided,
            self.config.state_entry_limit,
        )?;
        let now = self.clock.wall_now();
        for review in waiting {
            let deadline = parse_rfc3339(&review.approval_deadline).map_err(|err| {
                let cause = format!("review {} deadline: {err}", review.workflow_id);
                Error::workflow("store_state_invalid", true, Some(cause.into()))
            })?;
            if now < deadline {
                continue;
            }
            live(cancel)?;
            self.store
                .decide_approval(&review.workflow_id, APPROVAL_KIND_EXPIRED)?;
        }
        Ok(())
    }
}

/// Returns a ticker whose first tick comes one `period` from now, as Go's `time.NewTicker`.
///
/// A late tick is skipped instead of firing in a burst, as Go's ticker drops ticks.
fn ticker(period: Duration) -> Interval {
    let mut ticker = interval_at(tokio::time::Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker
}

/// Re-raises the panic of a review task, as a panicking Go goroutine stops the process.
fn resume_panic(joined: Result<(), tokio::task::JoinError>) {
    if let Err(err) = joined
        && err.is_panic()
    {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// Retries `step` with a doubling wait until it succeeds or a stop condition holds.
///
/// The stop conditions are a permanent failure, a cancelled `cancel` and a wait that would end
/// past `policy.total`. The wait never exceeds `policy.max_interval`, and the last error is
/// returned unchanged. Each retry is logged with `step`, the error code and the wait.
pub(crate) async fn retry_step<T, Fut>(
    step_name: &str,
    policy: StepRetryPolicy,
    clock: &impl Clock,
    cancel: &CancellationToken,
    mut step: impl FnMut() -> Fut,
) -> Result<T, Error>
where
    Fut: Future<Output = Result<T, Error>>,
{
    let start = clock.now();
    let mut delay = policy.initial;
    loop {
        match step().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if cancel.is_cancelled() {
                    return Err(err);
                }
                if is_permanent(&err) {
                    return Err(err);
                }
                let elapsed = clock.now().saturating_duration_since(start);
                if elapsed.saturating_add(delay) > policy.total {
                    return Err(err);
                }
                tracing::warn!(
                    step = step_name,
                    error_code = error_code(&err),
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    "step failed, retrying"
                );
                if cancel
                    .run_until_cancelled(clock.sleep(delay))
                    .await
                    .is_none()
                {
                    return Err(err);
                }
                delay = delay.saturating_mul(2).min(policy.max_interval);
            }
        }
    }
}

/// Reports whether the first workflow failure in the chain of `err` is marked permanent.
///
/// The chain is searched, not only its top, because Go's `errors.As` reads the first workflow
/// failure.
pub(crate) fn is_permanent(err: &Error) -> bool {
    let mut current: Option<&dyn std::error::Error> = Some(err);
    while let Some(error) = current {
        if let Some(Error::Workflow { permanent, .. }) = error.downcast_ref::<Error>() {
            return *permanent;
        }
        current = error.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use std::future::{Future, ready};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{StepRetryPolicy, retry_step};
    use crate::error::{Error, error_code};
    use crate::testing::harness::FakeClock;

    /// Step name of the retries in these tests.
    const TEST_STEP: &str = "test_step";

    /// Polls `future` once with a waker that is never woken. The futures of these tests complete
    /// on their first poll, so a pending one is a bug that fails the test instead of hanging it.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the retry future waits on a runtime"),
        }
    }

    #[test]
    fn retry_step_stops_on_permanent_errors() {
        let clock = FakeClock::new();
        let cancel = CancellationToken::new();
        let policy = StepRetryPolicy {
            total: Duration::from_secs(1),
            initial: Duration::from_millis(100),
            max_interval: Duration::from_millis(400),
        };
        let mut calls = 0;
        let result: Result<i64, Error> =
            block_on(retry_step(TEST_STEP, policy, &clock, &cancel, || {
                calls += 1;
                ready(Err(Error::workflow("review_identity_invalid", true, None)))
            }));
        let err = result.expect_err("a permanent failure is returned");
        assert_eq!(error_code(&err), "review_identity_invalid");
        assert_eq!(calls, 1);
        assert_eq!(clock.elapsed(), Duration::ZERO);
    }

    // Transient failures retry with doubling waits until one would pass the total: 100ms, 200ms
    // and 400ms fit in one second, the following 400ms does not.
    #[test]
    fn retry_step_respects_its_total_duration() {
        let clock = FakeClock::new();
        let cancel = CancellationToken::new();
        let policy = StepRetryPolicy {
            total: Duration::from_secs(1),
            initial: Duration::from_millis(100),
            max_interval: Duration::from_millis(400),
        };
        let mut calls = 0;
        let result: Result<i64, Error> =
            block_on(retry_step(TEST_STEP, policy, &clock, &cancel, || {
                calls += 1;
                ready(Err(Error::workflow("github_request_failed", false, None)))
            }));
        let err = result.expect_err("a failure past the total duration is returned");
        assert_eq!(error_code(&err), "github_request_failed");
        assert_eq!(calls, 4);
        assert_eq!(clock.elapsed(), Duration::from_millis(700));
    }

    #[test]
    fn retry_step_returns_the_first_success() {
        let clock = FakeClock::new();
        let cancel = CancellationToken::new();
        let policy = StepRetryPolicy {
            total: Duration::from_secs(1),
            initial: Duration::from_millis(100),
            max_interval: Duration::from_millis(400),
        };
        let mut calls = 0;
        let result: Result<String, Error> =
            block_on(retry_step(TEST_STEP, policy, &clock, &cancel, || {
                calls += 1;
                if calls < 3 {
                    ready(Err(Error::workflow("github_request_failed", false, None)))
                } else {
                    ready(Ok("found".to_owned()))
                }
            }));
        assert_eq!(result.expect("the third attempt succeeds"), "found");
        assert_eq!(calls, 3);
    }
}
