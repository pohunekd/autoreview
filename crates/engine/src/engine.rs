//! The engine core: its state, the clock it reads, the retry of one workflow step and the claims
//! that keep one review on one engine.
//!
//! Mirrors `cmd/autoreview/engine.go:1-69, 168-184`.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::integrations::Integrations;
use crate::store::Store;

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

/// Time source of the engine: monotonic instants and waits between attempts.
pub(crate) trait Clock: Send + Sync {
    /// Returns the current instant of this clock.
    fn now(&self) -> Instant;

    /// Waits for `delay` on this clock.
    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send;
}

/// The clock of the running process: the monotonic clock and the Tokio timer.
#[derive(Debug, Clone, Copy, Default)]
#[expect(dead_code, reason = "called by C01")]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(delay)
    }
}

/// The review engine. It holds the configuration, the store, the side effects and the set of
/// reviews this engine has claimed.
#[expect(dead_code, reason = "called by C01")]
pub(crate) struct Engine<I, C> {
    /// Validated configuration; every timing and limit is read from here.
    config: Config,
    /// Store of the discovery state and the reviews.
    store: Arc<Store>,
    /// GitHub, pohunek and Matrix behind the trait.
    integration: Arc<I>,
    /// Time source of the step retries.
    clock: C,
    /// Workflow IDs of the reviews claimed by this engine.
    active: Mutex<BTreeSet<String>>,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Creates an engine from the configuration, the store, the integrations and the clock.
    #[expect(dead_code, reason = "called by C01")]
    pub(crate) fn new(config: Config, store: Arc<Store>, integration: Arc<I>, clock: C) -> Self {
        Self {
            config,
            store,
            integration,
            clock,
            active: Mutex::new(BTreeSet::new()),
        }
    }

    /// Returns the retry policy of the workflow steps, read from the configuration.
    #[expect(dead_code, reason = "called by S22")]
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
    #[expect(dead_code, reason = "called by S30")]
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
    #[expect(dead_code, reason = "called by S30")]
    pub(crate) fn release(&self, workflow_id: &str) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        active.remove(workflow_id);
    }
}

/// Retries `step` with a doubling wait until it succeeds or a stop condition holds.
///
/// The stop conditions are a permanent failure, a cancelled `cancel` and a wait that would end
/// past `policy.total`. The wait never exceeds `policy.max_interval`, and the last error is
/// returned unchanged.
#[cfg_attr(not(test), expect(dead_code, reason = "called by S22"))]
pub(crate) async fn retry_step<T, Fut>(
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
fn is_permanent(err: &Error) -> bool {
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
        let result: Result<i64, Error> = block_on(retry_step(policy, &clock, &cancel, || {
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
        let result: Result<i64, Error> = block_on(retry_step(policy, &clock, &cancel, || {
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
        let result: Result<String, Error> = block_on(retry_step(policy, &clock, &cancel, || {
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
