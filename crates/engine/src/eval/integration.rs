//! The integrations of an eval trial: reads reach the real services, writes never do.
//!
//! The trait asks for every method, so a method added later does not compile here until it is
//! decided, in this one file, whether it reads or writes.

// Rust guideline compliant 2026-06-26

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use autoreview_guard::FieldKind;
use autoreview_policy::{Permission, Protocol, Trigger};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::config::Credential;
use crate::error::Error;
use crate::integrations::{
    ChangedFiles, CommentableLines, CommitRead, GitHubReview, GuardTexts, HeadCheckoutHooks,
    Integrations, PullRequestSignals, PullRequestState, RangeFiles, ReadiedDraft, RepositoryFile,
    ReviewRequestList, SessionRequest, TokenAnswer,
};
use crate::model::{
    CandidateList, Discussion, HostCapacity, LinkedIssues, PriorFinding, ProjectRoute,
    PublishResult, PullRequest, SessionIdentity,
};
use crate::notify::Notification;

/// Code of a call that an eval never makes.
pub const INTEGRATION_REFUSED: &str = "eval_integration_refused";

/// Code of a session start that would not fetch the pinned head.
pub const HEAD_NOT_PINNED: &str = "eval_head_not_pinned";

/// Code of a session start past the session budget of the run.
pub const SESSION_BUDGET_EXHAUSTED: &str = "eval_session_budget_exhausted";

/// What the trials of one run did, counted across trials.
#[derive(Debug, Default)]
pub struct RunCounters {
    /// Session starts that the budget admitted, retries included.
    pub sessions: AtomicUsize,
    /// Reviews that would have been published.
    pub would_publish: AtomicUsize,
    /// The review host and branch of every admitted session start.
    branches: Mutex<BTreeSet<(String, String)>>,
    /// The review host and session ID of every session that could not be removed.
    left: Mutex<BTreeSet<(String, String)>>,
}

impl RunCounters {
    /// Counts one session start, or returns `false` when `limit` starts were admitted already.
    fn admit(&self, limit: usize) -> bool {
        let count = |started: usize| (started < limit).then_some(started + 1);
        (self.sessions)
            .try_update(Ordering::SeqCst, Ordering::SeqCst, count)
            .is_ok()
    }

    /// Returns the `(host, branch)` of every session start of the run.
    pub fn branches(&self) -> BTreeSet<(String, String)> {
        let branches = self.branches.lock();
        branches.unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Returns the `(host, session ID)` of every session whose latest removal failed.
    pub fn sessions_left(&self) -> BTreeSet<(String, String)> {
        let left = self.left.lock();
        left.unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The session starts of one trial.
#[derive(Debug, Default)]
struct Recorded {
    /// Admitted session starts.
    starts: u32,
    /// Every session that `ensure_session` returned.
    sessions: Vec<SessionIdentity>,
    /// The requests whose start has not returned a session.
    pending: Vec<SessionRequest>,
}

/// The integrations of one trial: the case answers for the pull request.
pub struct EvalIntegration<I> {
    inner: Arc<I>,
    pull_request: PullRequest,
    head_ref: String,
    counters: Arc<RunCounters>,
    max_sessions: usize,
    recorded: Mutex<Recorded>,
    range: OnceCell<RangeFiles>,
}

fn refused<T>() -> Result<T, Error> {
    Err(Error::workflow(INTEGRATION_REFUSED, true, None))
}

impl<I> EvalIntegration<I> {
    /// Creates the integrations of a trial over `inner`, which only reads.
    ///
    /// Every session start fetches the head from `head_ref`, the case's ref.
    pub fn new(
        inner: Arc<I>,
        pull_request: PullRequest,
        head_ref: String,
        counters: Arc<RunCounters>,
        max_sessions: usize,
    ) -> Self {
        Self {
            inner,
            pull_request,
            head_ref,
            counters,
            max_sessions,
            recorded: Mutex::default(),
            range: OnceCell::new(),
        }
    }

    /// Returns the pull request that the case describes.
    pub fn pull_request(&self) -> &PullRequest {
        &self.pull_request
    }

    /// Returns the sessions this trial started.
    pub fn trial_sessions(&self) -> u32 {
        self.recorded().starts
    }

    fn recorded(&self) -> MutexGuard<'_, Recorded> {
        self.recorded.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<I: Integrations + Send + Sync> EvalIntegration<I> {
    /// Removes every session this trial started, also one whose start did not return.
    ///
    /// It uses a token that is not cancelled and gives each call at most `limit`. A session
    /// that is gone already is a no-op and is not listed; one that stays is logged and listed
    /// in [`RunCounters::sessions_left`] until a later call removes it.
    pub async fn remove_sessions(&self, limit: Duration) {
        let fresh = CancellationToken::new();
        let (mut sessions, pending) = {
            let recorded = self.recorded();
            (recorded.sessions.clone(), recorded.pending.clone())
        };
        for request in &pending {
            let lookup = self.inner.find_session(&fresh, request);
            match tokio::time::timeout(limit, lookup).await {
                Ok(Ok(Some(found))) if !sessions.contains(&found) => sessions.push(found),
                Ok(Ok(_)) => {}
                _ => tracing::warn!(session = %request.name(), "eval session lookup failed"),
            }
        }
        for session in &sessions {
            let removal = self.inner.remove_session_confirmed(&fresh, session);
            let key = (session.host.clone(), session.session_id.clone());
            match tokio::time::timeout(limit, removal).await {
                Ok(Ok(())) => {
                    let left = self.counters.left.lock();
                    left.unwrap_or_else(PoisonError::into_inner).remove(&key);
                }
                _ => {
                    tracing::warn!(session = %session.session_id, "eval session not removed");
                    let left = self.counters.left.lock();
                    left.unwrap_or_else(PoisonError::into_inner).insert(key);
                }
            }
        }
    }

    /// Reads the files of the case's commit range once.
    async fn range(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
    ) -> Result<&RangeFiles, Error> {
        let pr = &self.pull_request;
        self.range
            .get_or_try_init(|| {
                self.inner.read_range_files(
                    cancel,
                    credential,
                    &pr.repository,
                    &pr.base_sha,
                    &pr.head_sha,
                )
            })
            .await
    }
}

impl<I: Integrations + Send + Sync> Integrations for EvalIntegration<I> {
    async fn list_candidates(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
    ) -> Result<CandidateList, Error> {
        refused()
    }

    async fn ready_drafts(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
    ) -> Result<Vec<ReadiedDraft>, Error> {
        refused()
    }

    async fn get_pull_request(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        repository: &str,
        number: i64,
        _trigger: &Trigger,
    ) -> Result<PullRequest, Error> {
        let pr = &self.pull_request;
        if pr.repository == repository && pr.number == number {
            Ok(pr.clone())
        } else {
            refused()
        }
    }

    async fn read_pull_request_state(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _repository: &str,
        _number: i64,
    ) -> Result<PullRequestState, Error> {
        refused()
    }

    async fn author_permission(
        &self,
        _cancel: &CancellationToken,
        _repository: &str,
        _login: &str,
    ) -> Result<Option<Permission>, Error> {
        refused()
    }

    async fn check_token(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
    ) -> Result<TokenAnswer, Error> {
        refused()
    }

    async fn host_routes(
        &self,
        cancel: &CancellationToken,
        host: &str,
        repository: &str,
        sources: &[String],
    ) -> Result<Vec<ProjectRoute>, Error> {
        self.inner
            .host_routes(cancel, host, repository, sources)
            .await
    }

    async fn host_capacity(
        &self,
        cancel: &CancellationToken,
        host: &str,
        timeout: Duration,
    ) -> Result<HostCapacity, Error> {
        self.inner.host_capacity(cancel, host, timeout).await
    }

    async fn fetch_ref_supported(
        &self,
        cancel: &CancellationToken,
        host: &str,
    ) -> Result<bool, Error> {
        self.inner.fetch_ref_supported(cancel, host).await
    }

    async fn head_checkout_hooks(
        &self,
        cancel: &CancellationToken,
        host: &str,
    ) -> Result<HeadCheckoutHooks, Error> {
        self.inner.head_checkout_hooks(cancel, host).await
    }

    async fn ensure_session(
        &self,
        cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> Result<SessionIdentity, Error> {
        // A session that does not fetch the head would review whatever the host holds.
        if !request.fetch_head {
            return Err(Error::workflow(HEAD_NOT_PINNED, true, None));
        }
        if !self.counters.admit(self.max_sessions) {
            return Err(Error::workflow(SESSION_BUDGET_EXHAUSTED, true, None));
        }
        let mut request = request.clone();
        request.fetch_ref = Some(self.head_ref.clone());
        let branch = (request.route.host.clone(), request.branch());
        self.counters
            .branches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(branch);
        {
            let mut recorded = self.recorded();
            recorded.starts += 1;
            recorded.pending.push(request.clone());
        }
        let session = self.inner.ensure_session(cancel, &request).await?;
        let mut recorded = self.recorded();
        recorded
            .pending
            .retain(|pending| pending.name() != request.name());
        if !recorded.sessions.contains(&session) {
            recorded.sessions.push(session.clone());
        }
        Ok(session)
    }

    async fn find_session(
        &self,
        cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> Result<Option<SessionIdentity>, Error> {
        self.inner.find_session(cancel, request).await
    }

    async fn session_output(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
        protocol: Protocol,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.inner.session_output(cancel, session, protocol).await
    }

    async fn remove_session_confirmed(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
    ) -> Result<(), Error> {
        self.inner.remove_session_confirmed(cancel, session).await
    }

    async fn list_commentable_lines(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        _pull_request: &PullRequest,
    ) -> Result<CommentableLines, Error> {
        Ok(self.range(cancel, credential).await?.commentable.clone())
    }

    async fn list_changed_files(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        _pull_request: &PullRequest,
    ) -> Result<ChangedFiles, Error> {
        Ok(self.range(cancel, credential).await?.changed.clone())
    }

    async fn list_discussion(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
        _prior: &[PriorFinding],
    ) -> Result<Discussion, Error> {
        refused()
    }

    async fn list_closing_issues(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
    ) -> Result<LinkedIssues, Error> {
        self.inner
            .list_closing_issues(cancel, credential, pull_request)
            .await
    }

    async fn read_range_files(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        base: &str,
        head: &str,
    ) -> Result<RangeFiles, Error> {
        self.inner
            .read_range_files(cancel, credential, repository, base, head)
            .await
    }

    async fn read_commit(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _repository: &str,
        _sha: &str,
    ) -> Result<CommitRead, Error> {
        refused()
    }

    async fn read_blob_ids(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _repository: &str,
        _first: (&str, &str),
        _second: (&str, &str),
    ) -> Result<[Option<String>; 2], Error> {
        refused()
    }

    async fn read_repository_file(
        &self,
        cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        sha: &str,
        path: &str,
        max_bytes: u64,
    ) -> Result<RepositoryFile, Error> {
        self.inner
            .read_repository_file(cancel, credential, repository, sha, path, max_bytes)
            .await
    }

    async fn read_guard_texts(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
        _fields: &BTreeSet<FieldKind>,
        _max_bytes: usize,
        _publisher: &str,
    ) -> Result<GuardTexts, Error> {
        refused()
    }

    async fn linear_history(
        &self,
        _cancel: &CancellationToken,
        _repository: &str,
        _previous: &str,
        _head: &str,
    ) -> Result<bool, Error> {
        refused()
    }

    async fn publish_review(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> Result<PublishResult, Error> {
        self.counters.would_publish.fetch_add(1, Ordering::SeqCst);
        Ok(PublishResult {
            decision: review.decision.clone(),
            github_review_id: 1,
            review_url: format!("{}#eval", pull_request.url),
            reused: false,
        })
    }

    async fn find_published_review(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
        _review: &GitHubReview,
    ) -> Result<Option<PublishResult>, Error> {
        Ok(None)
    }

    async fn read_pull_request_signals(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _repository: &str,
        _number: i64,
    ) -> Result<PullRequestSignals, Error> {
        refused()
    }

    async fn notify(
        &self,
        _cancel: &CancellationToken,
        _backend: &str,
        _notification: &Notification,
    ) -> Result<(), Error> {
        Ok(())
    }

    async fn notify_unmuted(
        &self,
        _cancel: &CancellationToken,
        _backend: &str,
        _notification: &Notification,
    ) -> Result<(), Error> {
        Ok(())
    }

    async fn list_review_requests(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _max: i64,
    ) -> Result<ReviewRequestList, Error> {
        refused()
    }

    async fn send_queue_digest(
        &self,
        _cancel: &CancellationToken,
        _backend: &str,
        _transaction: &str,
        _plain: &str,
        _formatted: &str,
    ) -> Result<(), Error> {
        Ok(())
    }
}
