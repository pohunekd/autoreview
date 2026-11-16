//! In-memory fake of the [`Integrations`] boundary.
//!
//! Tests change the pull requests, sessions and reviewer outputs while a review runs. Each
//! method answers from the state under the lock and never touches a network or a process.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::{Future, Ready, ready};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use autoreview_guard::FieldKind;
use autoreview_policy::TriggerKind;
use autoreview_policy::{
    Checkout, Level, Permission, Policy, Protocol, Rule, Trigger, VERIFIER_REVIEWER_ID,
};
use autoreview_schedule::scheduler::HEAD_FETCH_MISMATCH;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::Credential;
use crate::error::{BoxError, Error, WORKFLOW_FAILED};
use crate::integrations::{
    ChangedFile, ChangedFiles, CommentableLines, GitHubReview, GuardText, GuardTexts, Integrations,
    PullRequestState, ReadiedDraft, ReviewRequest, ReviewRequestList, SessionRequest, TokenAnswer,
};
use crate::model::review::PRIOR_FIXED;
use crate::model::{
    Candidate, CandidateList, Discussion, HostCapacity, LinkedIssues, PriorFinding, ProjectRoute,
    PublishResult, PullRequest, RejectedCandidate, SessionIdentity,
};
use crate::notify::Notification;
use crate::protocol::claude::CLAUDE_RESULT_PROTOCOL;
use crate::protocol::codex::request_nonce;
use crate::protocol::request::{checkout_name, go_json};
use crate::publish::marker_forms;
use crate::verify::{
    ClaimFinding, ClaimPrior, VERDICT_CONFIRMED, VERDICT_UPHELD, VerificationClaims,
};

/// Produces the output of a reviewer session from its request.
type OutputFn = Arc<dyn Fn(&SessionRequest) -> String + Send + Sync>;

/// A notification the engine sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Note {
    /// Level of the notification.
    pub(crate) level: Level,
    /// Name of the backend that received the notification.
    pub(crate) backend: String,
    /// Text of the notification.
    pub(crate) message: String,
}

/// A queue digest the engine posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedDigest {
    /// Name of the backend that received the digest.
    pub(crate) backend: String,
    /// Transaction ID of the post.
    pub(crate) transaction: String,
    /// Plain-text body of the digest.
    pub(crate) plain: String,
    /// Formatted body of the digest.
    pub(crate) formatted: String,
}

/// The mutable state of the fake, guarded by one lock.
#[derive(Default)]
pub(crate) struct FakeState {
    /// Candidates by `repository#number`.
    pub(crate) candidates: BTreeMap<String, Candidate>,
    /// The policy whose authors section `open` gives each candidate; `None` gives none.
    pub(crate) policy: Option<Policy>,
    /// Current state of each pull request by `repository#number`.
    pub(crate) current: BTreeMap<String, PullRequest>,
    /// Pull requests that were closed, by `repository#number`.
    pub(crate) closed: BTreeSet<String>,
    /// Permanent error code of the direct read of a pull request, by `repository#number`.
    pub(crate) read_errors: BTreeMap<String, String>,
    /// Output of each reviewer session; `None` until a test sets it.
    pub(crate) output: Option<OutputFn>,
    /// Live sessions by session ID.
    pub(crate) sessions: BTreeMap<String, SessionRequest>,
    /// Every session that was started, in order.
    pub(crate) started: Vec<SessionRequest>,
    /// Commentable lines that the fake returns.
    pub(crate) commentable: CommentableLines,
    /// Changed files that the fake returns.
    pub(crate) changed: Vec<ChangedFile>,
    /// Discussion that the fake returns.
    pub(crate) discussion: Discussion,
    /// Prior findings of each discussion read, in order.
    pub(crate) discussion_reads: Vec<Vec<PriorFinding>>,
    /// Issues that the closing-issue read returns; `None` makes GitHub refuse the read.
    pub(crate) closing_issues: Option<LinkedIssues>,
    /// Pull request of each closing-issue read, as `repository#number`, in order.
    pub(crate) closing_reads: Vec<String>,
    /// Texts that the guard read returns, filtered by the fields it asks for.
    pub(crate) guard_texts: Vec<GuardText>,
    /// Number of guard reads.
    pub(crate) guard_reads: usize,
    /// Pull requests whose newer head is a rewritten history, by head SHA.
    pub(crate) rewritten: BTreeSet<String>,
    /// Refused candidates that the candidate search returns.
    pub(crate) rejected: Vec<RejectedCandidate>,
    /// Every review submission that was published.
    pub(crate) published: Vec<GitHubReview>,
    /// Head SHA of each review in `published`, in the same order.
    pub(crate) published_heads: Vec<String>,
    /// Login of the credential that posted each review in `published`, in the same order.
    pub(crate) publisher_logins: Vec<String>,
    /// Indexes into `published` of the reviews that GitHub dismissed.
    pub(crate) dismissed: BTreeSet<usize>,
    /// Number of lookups of a published review without a post.
    pub(crate) lookups: usize,
    /// Pull requests whose review request a posted review answered, by `repository#number`.
    pub(crate) answered: BTreeSet<String>,
    /// Every notification that was sent.
    pub(crate) notes: Vec<Note>,
    /// Counter of session IDs.
    pub(crate) next_session: usize,
    /// Number of candidate searches.
    pub(crate) listings: usize,
    /// Review requests that the queue search returns, in order.
    pub(crate) requests: Vec<ReviewRequest>,
    /// Every queue digest that was posted.
    pub(crate) queued: Vec<QueuedDigest>,
    /// Error code of each failing credential's searches, keyed by credential name.
    ///
    /// Its reads fail transiently as `github_request_failed`.
    pub(crate) failing_credentials: BTreeMap<String, String>,
    /// Every pull request state read, as `repository#number credential`, in order.
    pub(crate) state_reads: Vec<String>,
    /// Hosts whose daemon advertises the fetch of an extra ref at session start.
    pub(crate) fetch_ref_hosts: BTreeSet<String>,
    /// Number of the next capability reads that fail as a transient `pohunek_request_failed`.
    pub(crate) fetch_ref_failures: usize,
    /// Failures of the next session starts, in order, as error code and permanence.
    pub(crate) start_failures: VecDeque<(String, bool)>,
    /// Commit that a failed start with a head fetch mismatch reports, or empty for none.
    pub(crate) fetched_head: String,
}

impl FakeState {
    /// Returns the index of the first published review that carries `marker`, under any marker
    /// name as the real lookup reads it, and stands.
    ///
    /// With `head`, the review must also have been posted on that head. Only a review posted
    /// with `login` matches, as GitHub's lookup reads the reviews of its own login.
    fn standing_review(&self, marker: &str, head: Option<&str>, login: &str) -> Option<usize> {
        let forms = marker_forms(marker);
        self.published
            .iter()
            .zip(&self.published_heads)
            .enumerate()
            .find(|(index, (existing, published_head))| {
                !self.dismissed.contains(index)
                    && forms
                        .iter()
                        .any(|form| existing.body.contains(form.as_str()))
                    && head.is_none_or(|head| head == published_head.as_str())
                    && (self.publisher_logins.get(*index))
                        .is_some_and(|posted| posted.eq_ignore_ascii_case(login))
            })
            .map(|(index, _)| index)
    }
}

/// The fake of the [`Integrations`] boundary.
pub(crate) struct FakeIntegration {
    /// State of the fake; every method locks it for its whole body.
    state: Mutex<FakeState>,
}

impl FakeIntegration {
    /// Creates a fake with empty state.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(FakeState {
                discussion: Discussion::default(),
                ..FakeState::default()
            }),
        }
    }

    /// Locks the state so a test can read or change it.
    pub(crate) fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a pull request as a candidate.
    ///
    /// Its authors section is the one of the policy rule that covers the repository. It also
    /// requests a review of the login again, which a posted review answered.
    pub(crate) fn open(&self, pr: PullRequest, rule: Rule) {
        let mut state = self.lock();
        let key = pr.key();
        let authors = state
            .policy
            .as_ref()
            .and_then(|policy| policy.resolve_with_authors(&pr.repository))
            .map(|(_, authors)| authors.clone());
        state.candidates.insert(
            key.clone(),
            Candidate {
                pull_request: pr.clone(),
                rule,
                author_class: None,
                authors,
            },
        );
        state.current.insert(key.clone(), pr);
        state.answered.remove(&key);
    }

    /// Marks a pull request as closed.
    pub(crate) fn close(&self, pr: PullRequest) {
        let mut state = self.lock();
        let key = pr.key();
        state.candidates.remove(&key);
        state.closed.insert(key);
    }

    /// Dismisses the published review at `index`, as GitHub dismisses a stale approval.
    pub(crate) fn dismiss(&self, index: usize) {
        self.lock().dismissed.insert(index);
    }

    /// Returns how many candidate searches ran.
    pub(crate) fn listing_count(&self) -> usize {
        self.lock().listings
    }

    /// Returns published reviews, notes, started sessions and open count.
    pub(crate) fn snapshot(&self) -> (Vec<GitHubReview>, Vec<Note>, Vec<SessionRequest>, usize) {
        let state = self.lock();
        (
            state.published.clone(),
            state.notes.clone(),
            state.started.clone(),
            state.sessions.len(),
        )
    }

    /// Reports whether a note with level and fragment exists.
    pub(crate) fn has_note(&self, level: Level, fragment: &str) -> bool {
        let (_, notes, _, _) = self.snapshot();
        count_notes(&notes, level, fragment) > 0
    }

    /// Records one notification on `backend`; every notification is delivered.
    fn send_note(&self, backend: &str, notification: &Notification) -> Ready<Result<(), Error>> {
        self.lock().notes.push(Note {
            level: notification.level.clone(),
            backend: backend.to_owned(),
            message: notification.text.clone(),
        });
        ready(Ok(()))
    }
}

/// Counts notes of a level whose message contains a fragment.
pub(crate) fn count_notes(notes: &[Note], level: Level, fragment: &str) -> usize {
    notes
        .iter()
        .filter(|entry| entry.level == level && entry.message.contains(fragment))
        .count()
}

fn transient_error() -> Error {
    Error::workflow("github_request_failed", false, None)
}

impl FakeState {
    /// Returns the search error of `credential`, if it fails.
    fn search_error(&self, credential: &Credential) -> Option<Error> {
        let code = self.failing_credentials.get(&credential.name)?;
        Some(Error::workflow(code.clone(), true, None))
    }
}

impl Integrations for FakeIntegration {
    fn list_candidates(
        &self,
        _cancel: &CancellationToken,
        credential: &Credential,
    ) -> impl Future<Output = Result<CandidateList, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.listings += 1;
            if let Some(err) = state.search_error(credential) {
                Err(err)
            } else {
                let candidates = state.candidates.values().cloned().collect();
                let rejected = state.rejected.clone();
                Ok(CandidateList {
                    candidates,
                    rejected,
                })
            }
        };
        ready(result)
    }

    fn ready_drafts(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
    ) -> impl Future<Output = Result<Vec<ReadiedDraft>, Error>> + Send {
        ready(Ok(Vec::new()))
    }

    fn get_pull_request(
        &self,
        _cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        number: i64,
        trigger: &Trigger,
    ) -> impl Future<Output = Result<PullRequest, Error>> + Send {
        let key = format!("{repository}#{number}");
        let result = {
            let state = self.lock();
            if state.failing_credentials.contains_key(&credential.name) {
                Err(transient_error())
            } else if let Some(code) = state.read_errors.get(&key) {
                Err(Error::workflow(code.clone(), true, None))
            } else if let Some(pr) = state.current.get(&key) {
                // The open state is checked before the trigger, as `decode_pull_request` does.
                if trigger.kind == TriggerKind::ReviewRequested && state.answered.contains(&key) {
                    Err(Error::workflow("review_request_not_active", true, None))
                } else {
                    Ok(pr.clone())
                }
            } else {
                Err(Error::workflow("pull_request_not_open", true, None))
            }
        };
        ready(result)
    }

    fn read_pull_request_state(
        &self,
        _cancel: &CancellationToken,
        credential: &Credential,
        repository: &str,
        number: i64,
    ) -> impl Future<Output = Result<PullRequestState, Error>> + Send {
        let key = format!("{repository}#{number}");
        let result = {
            let mut state = self.lock();
            state.state_reads.push(format!("{key} {}", credential.name));
            if state.failing_credentials.contains_key(&credential.name) {
                return ready(Err(transient_error()));
            }
            match state.current.get(&key) {
                None => Ok(PullRequestState::default()),
                Some(_) if state.closed.contains(&key) => Ok(PullRequestState::default()),
                Some(pr) => Ok(PullRequestState {
                    open: true,
                    requested: state.candidates.contains_key(&key),
                    title: pr.title.clone(),
                    url: pr.url.clone(),
                    author: pr.author.clone(),
                    head_sha: pr.head_sha.clone(),
                }),
            }
        };
        ready(result)
    }

    fn author_permission(
        &self,
        _cancel: &CancellationToken,
        _repository: &str,
        _login: &str,
    ) -> impl Future<Output = Result<Option<Permission>, Error>> + Send {
        // The fake holds no permission, so every author reads as below write.
        ready(Ok(None))
    }

    fn check_token(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
    ) -> impl Future<Output = Result<TokenAnswer, Error>> + Send {
        // The fake holds no token answer, so every token check fails as a request error.
        ready(Err(Error::workflow("github_request_failed", false, None)))
    }

    fn host_routes(
        &self,
        _cancel: &CancellationToken,
        host: &str,
        _repository: &str,
        _sources: &[String],
    ) -> impl Future<Output = Result<Vec<ProjectRoute>, Error>> + Send {
        ready(Ok(vec![ProjectRoute {
            host: host.to_owned(),
            project_id: "project".to_owned(),
        }]))
    }

    fn host_capacity(
        &self,
        _cancel: &CancellationToken,
        _host: &str,
        _timeout: Duration,
    ) -> impl Future<Output = Result<HostCapacity, Error>> + Send {
        // The fake holds no host state, so every read fails and the state stays unknown.
        ready(Err(Error::workflow("pohunek_request_failed", false, None)))
    }

    fn fetch_ref_supported(
        &self,
        _cancel: &CancellationToken,
        host: &str,
    ) -> impl Future<Output = Result<bool, Error>> + Send {
        let mut state = self.lock();
        if state.fetch_ref_failures > 0 {
            state.fetch_ref_failures -= 1;
            return ready(Err(Error::workflow("pohunek_request_failed", false, None)));
        }
        ready(Ok(state.fetch_ref_hosts.contains(host)))
    }

    fn ensure_session(
        &self,
        _cancel: &CancellationToken,
        request: &SessionRequest,
    ) -> impl Future<Output = Result<SessionIdentity, Error>> + Send {
        let result = {
            let mut state = self.lock();
            let existing = state
                .sessions
                .iter()
                .find(|(_, existing)| existing.name() == request.name())
                .map(|(id, _)| id.clone());
            if let Some(id) = existing {
                Ok(SessionIdentity {
                    host: request.route.host.clone(),
                    session_id: id,
                    worker_instance_id: "worker".to_string(),
                    runtime_generation: "1".to_string(),
                    reviewer_id: request.reviewer.id.clone(),
                })
            } else if let Some((code, permanent)) = state.start_failures.pop_front() {
                let fetched = (code == HEAD_FETCH_MISMATCH && !state.fetched_head.is_empty()).then(
                    || -> BoxError {
                        let start =
                            Box::new(Error::workflow("fetch_ref_commit_mismatch", true, None));
                        Box::new(Error::fetched_head(state.fetched_head.clone(), start))
                    },
                );
                Err(Error::workflow(code, permanent, fetched))
            } else {
                state.next_session += 1;
                let id = format!("s-{}", state.next_session);
                state.sessions.insert(id.clone(), request.clone());
                state.started.push(request.clone());
                Ok(SessionIdentity {
                    host: request.route.host.clone(),
                    session_id: id,
                    worker_instance_id: "worker".to_string(),
                    runtime_generation: "1".to_string(),
                    reviewer_id: request.reviewer.id.clone(),
                })
            }
        };
        ready(result)
    }

    fn find_session(
        &self,
        _cancel: &CancellationToken,
        _request: &SessionRequest,
    ) -> impl Future<Output = Result<Option<SessionIdentity>, Error>> + Send {
        ready(Ok(None))
    }

    fn session_output(
        &self,
        _cancel: &CancellationToken,
        session: &SessionIdentity,
        _protocol: Protocol,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Error>> + Send {
        let outcome = {
            let state = self.lock();
            match state.sessions.get(&session.session_id) {
                None => Err(Error::workflow("pohunek_session_not_found", true, None)),
                Some(request) => {
                    let request = request.clone();
                    let output = state
                        .output
                        .clone()
                        .expect("a test sets the fake's output before a session runs");
                    let refusal = adapter_refusal(&request);
                    Ok((request, output, refusal))
                }
            }
        };
        let result = match outcome {
            Err(error) => Err(error),
            // An empty output means the session has not finished yet.
            Ok((request, output, refusal)) => {
                let text = refusal.unwrap_or_else(|| output(&request));
                Ok(Some(text.into_bytes()).filter(|bytes: &Vec<u8>| !bytes.is_empty()))
            }
        };
        ready(result)
    }

    fn remove_session_confirmed(
        &self,
        cancel: &CancellationToken,
        session: &SessionIdentity,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        // Like the host, it fails once the token is cancelled, so a stopping engine keeps its
        // reviewer sessions.
        if cancel.is_cancelled() {
            return ready(Err(Error::workflow(
                WORKFLOW_FAILED,
                false,
                Some("context canceled".into()),
            )));
        }
        let mut state = self.lock();
        state.sessions.remove(&session.session_id);
        ready(Ok(()))
    }

    fn list_commentable_lines(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
    ) -> impl Future<Output = Result<CommentableLines, Error>> + Send {
        let result = {
            let state = self.lock();
            state.commentable.clone()
        };
        ready(Ok(result))
    }

    fn list_changed_files(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
    ) -> impl Future<Output = Result<ChangedFiles, Error>> + Send {
        let result = ChangedFiles {
            files: self.lock().changed.clone(),
            truncated: false,
        };
        ready(Ok(result))
    }

    fn list_discussion(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
        prior: &[PriorFinding],
    ) -> impl Future<Output = Result<Discussion, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.discussion_reads.push(prior.to_vec());
            state.discussion.clone()
        };
        ready(Ok(result))
    }

    fn list_closing_issues(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        pull_request: &PullRequest,
    ) -> impl Future<Output = Result<LinkedIssues, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.closing_reads.push(pull_request.key());
            (state.closing_issues.clone())
                .ok_or_else(|| Error::workflow("github_request_rejected", true, None))
        };
        ready(result)
    }

    fn read_guard_texts(
        &self,
        _cancel: &CancellationToken,
        _credential: &Credential,
        _pull_request: &PullRequest,
        fields: &BTreeSet<FieldKind>,
        _max_bytes: usize,
        _publisher: &str,
    ) -> impl Future<Output = Result<GuardTexts, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.guard_reads = state.guard_reads.saturating_add(1);
            let texts = (state.guard_texts.iter())
                .filter(|text| fields.contains(&text.kind))
                .cloned()
                .collect();
            GuardTexts {
                texts,
                truncated: false,
            }
        };
        ready(Ok(result))
    }

    fn linear_history(
        &self,
        _cancel: &CancellationToken,
        _repository: &str,
        _previous: &str,
        head: &str,
    ) -> impl Future<Output = Result<bool, Error>> + Send {
        let result = {
            let state = self.lock();
            !state.rewritten.contains(head)
        };
        ready(Ok(result))
    }

    fn publish_review(
        &self,
        _cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> impl Future<Output = Result<PublishResult, Error>> + Send {
        // An earlier review is reused when its body carries the marker and it is not dismissed,
        // as the GitHub client finds it. A posted review answers the review request of the
        // login, as on GitHub.
        let result = {
            let mut state = self.lock();
            if let Some(index) = state.standing_review(&review.marker, None, &credential.login) {
                PublishResult {
                    decision: review.decision.clone(),
                    github_review_id: (index + 1) as i64,
                    review_url: format!("{}#review", pull_request.url),
                    reused: true,
                }
            } else {
                state.published.push(review.clone());
                state.published_heads.push(pull_request.head_sha.clone());
                state.publisher_logins.push(credential.login.clone());
                state.answered.insert(pull_request.key());
                PublishResult {
                    decision: review.decision.clone(),
                    github_review_id: state.published.len() as i64,
                    review_url: format!("{}#review", pull_request.url),
                    reused: false,
                }
            }
        };
        ready(Ok(result))
    }

    fn find_published_review(
        &self,
        _cancel: &CancellationToken,
        credential: &Credential,
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> impl Future<Output = Result<Option<PublishResult>, Error>> + Send {
        // The lookup of `publish_review`, which finds a review by the marker in its body. The
        // head must match too, as GitHub's lookup checks the commit of each review.
        let result = {
            let mut state = self.lock();
            state.lookups += 1;
            state
                .standing_review(
                    &review.marker,
                    Some(&pull_request.head_sha),
                    &credential.login,
                )
                .map(|index| PublishResult {
                    decision: review.decision.clone(),
                    github_review_id: (index + 1) as i64,
                    review_url: format!("{}#review", pull_request.url),
                    reused: true,
                })
        };
        ready(Ok(result))
    }

    fn list_review_requests(
        &self,
        _cancel: &CancellationToken,
        credential: &Credential,
        max: i64,
    ) -> impl Future<Output = Result<ReviewRequestList, Error>> + Send {
        let result = {
            let state = self.lock();
            if let Some(err) = state.search_error(credential) {
                return ready(Err(err));
            }
            let len = state.requests.len() as i64;
            let take = if max < 0 {
                0
            } else {
                std::cmp::min(max + 1, len) as usize
            };
            let truncated = len > max;
            ReviewRequestList {
                requests: state.requests[..take].to_vec(),
                truncated,
            }
        };
        ready(Ok(result))
    }

    fn send_queue_digest(
        &self,
        _cancel: &CancellationToken,
        backend: &str,
        transaction: &str,
        plain: &str,
        formatted: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        self.lock().queued.push(QueuedDigest {
            backend: backend.to_owned(),
            transaction: transaction.to_string(),
            plain: plain.to_string(),
            formatted: formatted.to_string(),
        });
        ready(Ok(()))
    }

    fn notify(
        &self,
        _cancel: &CancellationToken,
        backend: &str,
        notification: &Notification,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        self.send_note(backend, notification)
    }

    fn notify_unmuted(
        &self,
        _cancel: &CancellationToken,
        backend: &str,
        notification: &Notification,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        self.send_note(backend, notification)
    }
}

/// Label of the prior findings in a reviewer prompt.
const PRIOR_LABEL: &str = "Prior findings (JSON):\n";

/// Label of the claims in a verifier prompt.
const CLAIMS_LABEL: &str = "Claims (untrusted JSON):\n";

/// Summary that the fake Claude reviewer reports with every structured output.
const REVIEW_SUMMARY: &str = "Reviewed the change thoroughly and found the listed issues.";

/// Returns the `prompt` field of a structured request, or the text itself.
fn prompt_text(prompt: &str) -> String {
    let parsed: Value = match serde_json::from_str(prompt) {
        Ok(value) => value,
        Err(_) => return prompt.to_owned(),
    };
    match parsed.get("prompt").and_then(Value::as_str) {
        Some(field) if !field.is_empty() => field.to_owned(),
        _ => prompt.to_owned(),
    }
}

/// Reads the prior findings that a reviewer request hands over.
fn request_prior(prompt: &str) -> Vec<PriorFinding> {
    let text = prompt_text(prompt);
    let Some(after) = text.split_once(PRIOR_LABEL).map(|(_, rest)| rest) else {
        return Vec::new();
    };
    let line = after.split_once('\n').map_or(after, |(first, _)| first);
    serde_json::from_str(line).expect("the fake reads the prior findings it wrote")
}

/// Claim of a blocking finding in a verifier prompt, with the fields of `ClaimFinding`.
#[derive(Deserialize)]
struct ClaimFindingLine {
    id: String,
    reviewer: String,
    severity: String,
    path: String,
    line: i64,
    title: String,
    body: String,
}

/// Answered prior finding in a verifier prompt, with the fields of `ClaimPrior`.
#[derive(Deserialize)]
struct ClaimPriorLine {
    key: String,
    severity: String,
    path: String,
    line: i64,
    title: String,
    body: String,
    #[serde(rename = "reviewers_verdict")]
    verdict: String,
    relies_on: String,
}

/// The claims of a verifier prompt, read back as the fake receives them.
#[derive(Deserialize)]
struct ClaimsLine {
    findings: Vec<ClaimFindingLine>,
    prior_findings: Vec<ClaimPriorLine>,
}

/// Reads the claims that a verifier request hands over.
pub(crate) fn request_claims(prompt: &str) -> VerificationClaims {
    let parsed: Value =
        serde_json::from_str(prompt).expect("the fake reads the claims of a verifier request");
    let field = parsed
        .get("prompt")
        .and_then(Value::as_str)
        .expect("a verifier request carries a prompt field");
    let (_, rest) = field
        .split_once(CLAIMS_LABEL)
        .expect("a verifier request carries the claims label");
    let line = rest.split_once('\n').map_or(rest, |(first, _)| first);
    let claims: ClaimsLine =
        serde_json::from_str(line).expect("the fake reads the claims it wrote");
    VerificationClaims {
        findings: claims
            .findings
            .into_iter()
            .map(|line| ClaimFinding {
                id: line.id,
                reviewer: line.reviewer,
                severity: line.severity,
                path: line.path,
                line: line.line,
                title: line.title,
                body: line.body,
            })
            .collect(),
        prior: claims
            .prior_findings
            .into_iter()
            .map(|line| ClaimPrior {
                key: line.key,
                severity: line.severity,
                path: line.path,
                line: line.line,
                title: line.title,
                body: line.body,
                verdict: line.verdict,
                relies_on: line.relies_on,
            })
            .collect(),
    }
}

/// Returns the output of the adapter when it refuses the checkout of `request`.
///
/// Like the Codex adapter in `adapters/`, it reads the checkout from the one trusted checkout
/// line of the prompt, `head` when there is none, and refuses one that differs from the
/// session's: no result frame, the exit line with status 64 bound to the request's nonce.
fn adapter_refusal(request: &SessionRequest) -> Option<String> {
    if request.reviewer.protocol != Protocol::CodexEnvelope {
        return None;
    }
    let session = if request.checkout == Checkout::Head {
        "head"
    } else {
        "base"
    };
    let lines: Vec<&str> = request
        .prompt
        .lines()
        .filter_map(|line| line.strip_prefix("- Trusted checkout: "))
        .collect();
    let named = match lines.as_slice() {
        [] => Some("head"),
        [line] => Some(*line),
        _ => None,
    };
    (named != Some(session))
        .then(|| format!("{}64]\n", codex_marker(request, "exited with status ")))
}

/// Returns the opening of the Codex marker `name` of `request`, bound to its nonce.
///
/// Like the adapter, it reads the nonce from the one nonce line of the prompt.
fn codex_marker(request: &SessionRequest, name: &str) -> String {
    let nonce = request_nonce(&request.prompt).expect("a Codex request carries one nonce");
    format!("[autoreview-codex/v2 {nonce} {name}")
}

/// Returns the output of a Codex reviewer run of `request` with `findings` and the verdicts
/// `prior`.
fn codex_output(
    request: &SessionRequest,
    findings: &[Value],
    prior: &[Value],
    status: i64,
) -> String {
    let report = go_json(&json!({
        "findings": findings,
        "summary": "Full review of the pull request diff; details follow.",
        "tests": ["go test ./..."],
        "status": "completed",
        "reviewed_head_sha": request.pull_request.head_sha,
        "prior_findings": prior,
    }))
    .expect("the fake encodes the Codex report");
    let encoded = STANDARD.encode(report);
    let prefix = format!("{}{encoded}]\n", codex_marker(request, "result:"));
    let status_line = format!(
        "{}{status}]\n",
        codex_marker(request, "exited with status ")
    );
    format!("{prefix}{status_line}")
}

/// Returns the output of a finished Claude session of `request` with `structured` as its
/// structured output.
///
/// Like the adapter in `adapters/claude/`, the wrapper names the checkout of the session.
fn claude_structured_output(request: &SessionRequest, structured: Value) -> String {
    let outer = go_json(&json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "Prose summary.",
        "structured_output": structured,
    }))
    .expect("the fake encodes the Claude session output");
    let wrapper = go_json(&json!({
        "protocol": CLAUDE_RESULT_PROTOCOL,
        "mode": "structured",
        "exit_code": 0,
        "stdout_base64": STANDARD.encode(outer),
        "stderr_base64": "",
        "checked_checkout": checkout_name(&request.checkout),
    }))
    .expect("the fake encodes the Claude wrapper");
    format!("{wrapper}\n")
}

/// Returns the output of a finished Claude reviewer session.
///
/// The prior verdicts are included only when `prior` is `Some`.
fn claude_output_with_prior(
    request: &SessionRequest,
    findings: Vec<Value>,
    prior: Option<Vec<Value>>,
) -> String {
    let pull_request = &request.pull_request;
    let mut structured = json!({
        "repository": pull_request.repository,
        "number": pull_request.number,
        "head_sha": pull_request.head_sha,
        "summary": REVIEW_SUMMARY,
        "findings": findings,
    });
    if let Some(prior) = prior {
        structured
            .as_object_mut()
            .expect("the fake builds the reviewer artifact")
            .insert("prior_findings".to_owned(), Value::Array(prior));
    }
    claude_structured_output(request, structured)
}

/// Returns a reviewer output that gives every prior finding the verdict `status` picks.
///
/// Verifier requests get confirmed claims and upheld prior findings.
pub(crate) fn settling_output(
    findings: Vec<Value>,
    status: impl Fn(&PriorFinding) -> &'static str + Send + Sync + 'static,
) -> impl Fn(&SessionRequest) -> String + Send + Sync + 'static {
    move |request: &SessionRequest| {
        if request.reviewer.id == VERIFIER_REVIEWER_ID {
            return verifier_output(request, |_| VERDICT_CONFIRMED, |_| VERDICT_UPHELD);
        }
        let verdicts: Vec<Value> = request_prior(&request.prompt)
            .iter()
            .map(|prior| {
                json!({
                    "key": prior.key,
                    "status": status(prior),
                    "note": "Checked against the current head.",
                    "relies_on": "",
                })
            })
            .collect();
        if request.reviewer.protocol == Protocol::CodexEnvelope {
            // A finding without the flag blocks, as it does in a Claude result.
            let codex_findings: Vec<Value> = findings
                .iter()
                .map(|finding| {
                    let mut merged = serde_json::Map::new();
                    merged.insert("blocking".to_owned(), Value::Bool(true));
                    if let Some(object) = finding.as_object() {
                        for (key, value) in object {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                    Value::Object(merged)
                })
                .collect();
            return codex_output(request, &codex_findings, &verdicts, 0);
        }
        let claude_findings: Vec<Value> = findings
            .iter()
            .enumerate()
            .map(|(index, finding)| {
                let mut merged = serde_json::Map::new();
                merged.insert("id".to_owned(), Value::String(format!("C{:03}", index + 1)));
                merged.insert("side".to_owned(), Value::String("RIGHT".to_owned()));
                if let Some(object) = finding.as_object() {
                    for (key, value) in object {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                Value::Object(merged)
            })
            .collect();
        claude_output_with_prior(request, claude_findings, Some(verdicts))
    }
}

/// Returns a reviewer output that settles every prior finding as fixed.
pub(crate) fn reviewer_output(
    findings: Vec<Value>,
) -> impl Fn(&SessionRequest) -> String + Send + Sync + 'static {
    settling_output(findings, |_| PRIOR_FIXED)
}

/// Returns the output of a finished verifier session.
///
/// Every claim gets the verdict that `finding` or `prior` picks.
fn verifier_output(
    request: &SessionRequest,
    finding: impl Fn(&ClaimFinding) -> &'static str,
    prior: impl Fn(&ClaimPrior) -> &'static str,
) -> String {
    let claims = request_claims(&request.prompt);
    let pr = &request.pull_request;
    let findings: Vec<Value> = claims
        .findings
        .iter()
        .map(|claim| {
            json!({
                "id": claim.id,
                "verdict": finding(claim),
                "note": "Traced the scenario at the head.",
            })
        })
        .collect();
    let priors: Vec<Value> = claims
        .prior
        .iter()
        .map(|claim| {
            json!({
                "key": claim.key,
                "verdict": prior(claim),
                "note": "Read the code at the head.",
            })
        })
        .collect();
    claude_structured_output(
        request,
        json!({
            "repository": pr.repository,
            "number": pr.number,
            "head_sha": pr.head_sha,
            "findings": findings,
            "prior_findings": priors,
        }),
    )
}

/// Policy document of the integration tests: pipelines, repositories and notifications.
pub(crate) const INTEGRATION_POLICY: &str = include_str!("integration-policy.yaml");
