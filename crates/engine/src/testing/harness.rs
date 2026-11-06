//! Test doubles of the engine's time source and the harness of the engine scenarios.

// Rust guideline compliant 2026-06-26

use std::future::{Future, ready};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use autoreview_policy::Rule;
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::fake::{FakeIntegration, INTEGRATION_POLICY, reviewer_output};
use crate::config::Config;
use crate::engine::{Clock, Engine, SystemClock};
use crate::error::Error;
use crate::integrations::SessionRequest;
use crate::model::{
    ApprovalResult, ApproveRequest, PollResult, PullRequest, ResolveRequest, ResolveResult,
};
use crate::protocol::fixtures::sha;
use crate::protocol::validate::tests::test_config;
use crate::store::{REVIEW_AWAITING, Store};

/// A clock that moves only when a wait ends, so retry tests never sleep.
pub(crate) struct FakeClock {
    /// The instant the clock starts from; only offsets from it are observable.
    start: Instant,
    /// Time that the waits have advanced the clock by.
    elapsed: Mutex<Duration>,
}

impl FakeClock {
    /// Creates a clock at its start, with no time elapsed.
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        }
    }

    /// Returns the time that the waits advanced the clock by.
    pub(crate) fn elapsed(&self) -> Duration {
        *self.elapsed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.start
            .checked_add(self.elapsed())
            .expect("the fake clock stays within the Instant range")
    }

    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send {
        let mut elapsed = self.elapsed.lock().unwrap_or_else(PoisonError::into_inner);
        *elapsed = elapsed.saturating_add(delay);
        ready(())
    }

    fn wall_now(&self) -> OffsetDateTime {
        let elapsed = time::Duration::try_from(self.elapsed())
            .expect("the fake clock stays within the time::Duration range");
        datetime!(2026-01-01 0:00 UTC) + elapsed
    }
}

/// Prompt text that every prompt path of the integration policy reads as.
const INTEGRATION_PROMPT: &[u8] = b"Review the pull request.";

/// Time that [`Harness::wait_for`] and [`Harness::poll_until`] wait before they fail.
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Pause between two checks of [`Harness::wait_for`].
const WAIT_PAUSE: Duration = Duration::from_millis(100);

/// Pause between two polls of [`Harness::poll_until`].
const POLL_PAUSE: Duration = Duration::from_millis(150);

/// Length of the head prefix that the harness resolves and retries reviews by.
pub(crate) const HEAD_PREFIX_CHARS: usize = 12;

/// The engine of a harness: the fake integration and the process clock.
pub(crate) type HarnessEngine = Engine<FakeIntegration, SystemClock>;

/// Runs one engine on a fresh store against the fake integration.
///
/// Discovery runs only when a test polls, because the poll interval is an hour.
pub(crate) struct Harness {
    /// Changes the integration configuration before an engine is built from it.
    configure: Box<dyn Fn(&mut Config)>,
    /// The fake integration, shared by every engine of the harness.
    pub(crate) fake: Arc<FakeIntegration>,
    /// The store, shared by every engine of the harness.
    pub(crate) store: Arc<Store>,
    /// The engine started last.
    pub(crate) engine: Arc<HarnessEngine>,
    /// Stops the running engine; `None` once it is stopped.
    cancel: Option<CancellationToken>,
    /// Result of the running engine's `serve`; `None` once it is stopped.
    done: Option<JoinHandle<Result<(), Error>>>,
    /// Directory of the store, removed when the harness is dropped.
    dir: TempDir,
}

/// Returns the configuration of the integration tests with the store in `dir`.
pub(crate) fn integration_config(dir: &Path) -> Config {
    let parsed = autoreview_policy::parse(INTEGRATION_POLICY.as_bytes(), |_| {
        Ok(INTEGRATION_PROMPT.to_vec())
    })
    .expect("the integration policy parses");
    let mut cfg = test_config();
    cfg.policy = Some(parsed);
    cfg.state_db_path = dir.join("state.db").display().to_string();
    cfg.state_entry_limit = 5000;
    cfg.schedule_max_bytes = 32 << 20;
    cfg.published_retention = Duration::from_secs(24 * 60 * 60);
    cfg.session_timeout = Duration::from_secs(20);
    cfg.output_poll_interval = Duration::from_millis(100);
    cfg.session_start_attempts = 2;
    cfg.session_start_retry = Duration::from_millis(100);
    cfg.session_cleanup_attempts = 2;
    cfg.session_cleanup_retry = Duration::from_millis(50);
    cfg.reviewer_run_attempts = 2;
    cfg.reviewer_run_retry = Duration::from_millis(100);
    cfg.step_retry = Duration::from_secs(5);
    cfg.step_retry_initial = Duration::from_millis(10);
    cfg.step_retry_max_interval = Duration::from_millis(100);
    cfg.poll_interval = Duration::from_secs(60 * 60);
    cfg.worker_tick = Duration::from_millis(20);
    cfg.discovery_lock_timeout = Duration::from_secs(5);
    cfg.discovery_lock_poll = Duration::from_millis(10);
    cfg.review_retention = Duration::from_secs(48 * 60 * 60);
    cfg.max_concurrent_reviews = 8;
    cfg.review_list_limit = 50;
    cfg.schedule.debounce = time::Duration::milliseconds(100);
    cfg.schedule.retry_delays = vec![time::Duration::milliseconds(200)];
    cfg
}

/// Opens a fresh store and starts an engine on it.
///
/// `configure` may change the configuration before each engine is built.
pub(crate) async fn start_harness(configure: impl Fn(&mut Config) + 'static) -> Harness {
    let dir = TempDir::new().expect("create the store directory");
    let mut cfg = integration_config(dir.path());
    configure(&mut cfg);
    let store = Store::open(
        Path::new(&cfg.state_db_path),
        Box::new(OffsetDateTime::now_utc),
    )
    .expect("open the store");
    let fake = Arc::new(FakeIntegration::new());
    fake.lock().output = Some(Arc::new(reviewer_output(Vec::new())));
    let store = Arc::new(store);
    let engine = Arc::new(Engine::new(
        cfg,
        Arc::clone(&store),
        Arc::clone(&fake),
        SystemClock,
    ));
    let mut harness = Harness {
        configure: Box::new(configure),
        fake,
        store,
        engine,
        cancel: None,
        done: None,
        dir,
    };
    harness.serve();
    // The start-up pass lists the searches before any test opens a pull request, so it never
    // sees one half-registered.
    harness
        .wait_for("the start-up discovery pass", || {
            harness.fake.listing_count() >= 1
        })
        .await;
    harness
}

impl Harness {
    /// Returns the configuration that every engine of the harness is built with.
    pub(crate) fn config(&self) -> Config {
        let mut cfg = integration_config(self.dir.path());
        (self.configure)(&mut cfg);
        cfg
    }

    /// Runs a new engine on the harness store until [`Harness::stop_engine`].
    pub(crate) fn start(&mut self) {
        self.engine = Arc::new(Engine::new(
            self.config(),
            Arc::clone(&self.store),
            Arc::clone(&self.fake),
            SystemClock,
        ));
        self.serve();
    }

    /// Serves the current engine in its own task until [`Harness::stop_engine`].
    fn serve(&mut self) {
        let cancel = CancellationToken::new();
        let engine = Arc::clone(&self.engine);
        let token = cancel.clone();
        self.done = Some(tokio::spawn(async move { engine.serve(&token).await }));
        self.cancel = Some(cancel);
    }

    /// Stops the running engine and waits until `serve` returned, which releases its lock.
    pub(crate) async fn stop_engine(&mut self) {
        let (Some(cancel), Some(done)) = (self.cancel.take(), self.done.take()) else {
            return;
        };
        cancel.cancel();
        let result = done.await.expect("the engine task finishes");
        assert!(result.is_ok(), "engine stopped with error: {result:?}");
    }

    /// Replaces the output of every reviewer session.
    pub(crate) fn set_output(
        &self,
        output: impl Fn(&SessionRequest) -> String + Send + Sync + 'static,
    ) {
        self.fake.lock().output = Some(Arc::new(output));
    }

    /// Runs one discovery poll and fails the test on its error.
    pub(crate) async fn poll(&self) -> PollResult {
        self.engine
            .poll(&CancellationToken::new())
            .await
            .expect("the discovery poll succeeds")
    }

    /// Polls discovery until `condition` holds.
    pub(crate) async fn poll_until(&self, description: &str, condition: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        while !condition() {
            if tokio::time::Instant::now() > deadline {
                self.time_out(description);
            }
            self.poll().await;
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// Waits until `condition` holds.
    pub(crate) async fn wait_for(&self, description: &str, condition: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        while !condition() {
            if tokio::time::Instant::now() > deadline {
                self.time_out(description);
            }
            tokio::time::sleep(WAIT_PAUSE).await;
        }
    }

    /// Waits until `condition` holds, running a discovery poll before every check.
    ///
    /// A failed poll is ignored, so a test can wait through an outage of discovery.
    pub(crate) async fn wait_for_polling(&self, description: &str, condition: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            let _ = self.engine.poll(&CancellationToken::new()).await;
            if condition() {
                return;
            }
            if tokio::time::Instant::now() > deadline {
                self.time_out(description);
            }
            tokio::time::sleep(WAIT_PAUSE).await;
        }
    }

    /// Fails the test with the state of the fake.
    fn time_out(&self, description: &str) -> ! {
        let (published, notes, started, open) = self.fake.snapshot();
        panic!(
            "timed out waiting for {description}: published={} notes={notes:?} started={} \
             open sessions={open}",
            published.len(),
            started.len()
        );
    }

    /// Returns the number of published reviews.
    pub(crate) fn publications(&self) -> usize {
        self.fake.snapshot().0.len()
    }

    /// Returns the number of started reviewer sessions.
    pub(crate) fn started_count(&self) -> usize {
        self.fake.snapshot().2.len()
    }

    /// Resolves the waiting review of `pr` by the prefix of its head.
    pub(crate) fn resolve(&self, pr: &PullRequest) -> Result<ResolveResult, Error> {
        let request = ResolveRequest {
            repository: pr.repository.clone(),
            number: pr.number,
            head_prefix: pr.head_sha[..HEAD_PREFIX_CHARS].to_owned(),
        };
        self.engine.resolve(&CancellationToken::new(), &request)
    }

    /// Approves the review `workflow_id` of `pr` at its full head.
    pub(crate) fn approve(
        &self,
        workflow_id: &str,
        pr: &PullRequest,
    ) -> Result<ApprovalResult, Error> {
        let request = ApproveRequest {
            repository: pr.repository.clone(),
            number: pr.number,
            head_sha: pr.head_sha.clone(),
        };
        self.engine
            .approve(&CancellationToken::new(), workflow_id, &request)
    }

    /// Approves the review of `pr` once it is stored as awaiting.
    ///
    /// The approval request is sent before that status is stored, and `approve` refuses a
    /// review that is not awaiting yet.
    pub(crate) async fn approve_when_awaiting(
        &self,
        pr: &PullRequest,
    ) -> Result<ApprovalResult, Error> {
        let resolved = self
            .resolve(pr)
            .expect("the review of the pull request resolves");
        self.wait_for("the stored approval wait", || {
            matches!(
                self.store.get_review(&resolved.workflow_id),
                Ok(Some(stored)) if stored.status == REVIEW_AWAITING
            )
        })
        .await;
        self.approve(&resolved.workflow_id, pr)
    }

    /// Reports whether the review of the first started session has stored the step `name`.
    pub(crate) fn step_recorded(&self, name: &str) -> bool {
        let (_, _, started, _) = self.fake.snapshot();
        let Some(first) = started.first() else {
            return false;
        };
        matches!(
            self.store.load_step::<bool>(&first.workflow_id, name),
            Ok(Some(_))
        )
    }

    /// Returns the rule of the integration policy for `repository`.
    pub(crate) fn rule(&self, repository: &str) -> Rule {
        self.engine
            .config
            .policy
            .as_ref()
            .and_then(|policy| policy.resolve(repository))
            .cloned()
            .unwrap_or_else(|| panic!("{repository} is not covered by the integration policy"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // A test that fails before `stop_engine` still stops its engine.
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }
}

/// Returns a pull request number that differs between tests.
pub(crate) fn unique_number() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    i64::try_from(nanos % 900_000_000).expect("the remainder fits in i64") + 1
}

/// Returns an open pull request of `repository` at `head` with review request 11.
pub(crate) fn integration_pull_request(repository: &str, head: &str) -> PullRequest {
    let number = unique_number();
    PullRequest {
        repository: repository.to_owned(),
        number,
        head_sha: head.to_owned(),
        review_request_id: 11,
        title: "Change".to_owned(),
        url: format!("https://github.com/{repository}/pull/{number}"),
        author: "colleague".to_owned(),
        head_branch: "feature".to_owned(),
        base_branch: "main".to_owned(),
        base_sha: sha("b"),
        ..PullRequest::default()
    }
}
