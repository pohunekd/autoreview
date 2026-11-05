//! In-memory fake of the [`Integrations`] boundary.
//!
//! Tests change the pull requests, sessions and reviewer outputs while a review runs. Each
//! method answers from the state under the lock and never touches a network or a process.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, ready};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use autoreview_policy::{Level, Protocol, Rule, Trigger};

use crate::error::Error;
use crate::integrations::{
    CommentableLines, GitHubReview, Integrations, PullRequestState, ReadiedDraft, ReviewRequest,
    ReviewRequestList, SessionRequest,
};
use crate::model::{
    Candidate, CandidateList, Discussion, PriorFinding, ProjectRoute, PublishResult, PullRequest,
    RejectedCandidate, SessionIdentity,
};

/// Produces the output of a reviewer session from its request.
type OutputFn = Arc<dyn Fn(&SessionRequest) -> String + Send + Sync>;

/// Decides whether a session start fails.
type StartFailFn = Arc<dyn Fn(&SessionRequest) -> Result<(), Error> + Send + Sync>;

/// A notification the engine sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Note {
    /// Room the notification went to.
    pub(crate) level: Level,
    /// Text of the notification.
    pub(crate) message: String,
}

/// A queue digest the engine posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedDigest {
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
    /// Current state of each pull request by `repository#number`.
    pub(crate) current: BTreeMap<String, PullRequest>,
    /// Read errors of pull requests that left the search, by `repository#number`.
    pub(crate) lapsed: BTreeMap<String, String>,
    /// Pull requests that were closed, by `repository#number`.
    pub(crate) closed: BTreeSet<String>,
    /// Output of each reviewer session; `None` until a test sets it.
    pub(crate) output: Option<OutputFn>,
    /// Live sessions by session ID.
    pub(crate) sessions: BTreeMap<String, SessionRequest>,
    /// Every session that was started, in order.
    pub(crate) started: Vec<SessionRequest>,
    /// Fails a session start when it returns an error; `None` lets every start succeed.
    pub(crate) start_fail: Option<StartFailFn>,
    /// Every start request, failed or not.
    pub(crate) attempts: Vec<SessionRequest>,
    /// Session IDs whose removal fails.
    pub(crate) remove_failing: BTreeSet<String>,
    /// Session IDs that were removed, in order.
    pub(crate) removed: Vec<String>,
    /// Commentable lines that the fake returns.
    pub(crate) commentable: CommentableLines,
    /// Deleted files that the fake returns.
    pub(crate) deleted: Vec<String>,
    /// Number of reads of the deleted files.
    pub(crate) deleted_reads: usize,
    /// Discussion that the fake returns.
    pub(crate) discussion: Discussion,
    /// Prior findings of each discussion read, in order.
    pub(crate) discussion_reads: Vec<Vec<PriorFinding>>,
    /// Pull requests whose newer head is a rewritten history, by head SHA.
    pub(crate) rewritten: BTreeSet<String>,
    /// Error code that every publication fails with; `None` lets publications succeed.
    pub(crate) publish_fail: Option<String>,
    /// Until this instant, reads and searches fail transiently; `None` never fails them.
    pub(crate) transient_until: Option<Instant>,
    /// Refused candidates that the candidate search returns.
    pub(crate) rejected: Vec<RejectedCandidate>,
    /// Every review submission that was published.
    pub(crate) published: Vec<GitHubReview>,
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
    /// Number of queue digest send attempts.
    pub(crate) queue_sends: usize,
    /// Transaction ID of every send attempt.
    pub(crate) queue_transactions: Vec<String>,
    /// Number of leading send attempts that fail transiently.
    pub(crate) queue_failures: usize,
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
    pub(crate) fn open(&self, pr: PullRequest, rule: Rule) {
        let mut state = self.lock();
        let key = pr.key();
        state.candidates.insert(
            key.clone(),
            Candidate {
                pull_request: pr.clone(),
                rule,
            },
        );
        state.current.insert(key.clone(), pr);
        state.lapsed.remove(&key);
    }

    /// Marks a pull request as lapsed with an error code.
    pub(crate) fn lapse(&self, pr: PullRequest, code: &str) {
        let mut state = self.lock();
        let key = pr.key();
        state.candidates.remove(&key);
        state.lapsed.insert(key, code.to_string());
    }

    /// Marks a pull request as closed.
    pub(crate) fn close(&self, pr: PullRequest) {
        let mut state = self.lock();
        let key = pr.key();
        state.candidates.remove(&key);
        state.closed.insert(key);
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
    fn transient(&self) -> bool {
        match self.transient_until {
            Some(until) => Instant::now() < until,
            None => false,
        }
    }
}

impl Integrations for FakeIntegration {
    fn list_candidates(&self) -> impl Future<Output = Result<CandidateList, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.listings += 1;
            if state.transient() {
                Err(transient_error())
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

    fn ready_drafts(&self) -> impl Future<Output = Result<Vec<ReadiedDraft>, Error>> + Send {
        ready(Ok(Vec::new()))
    }

    fn get_pull_request(
        &self,
        repository: &str,
        number: i64,
        _trigger: &Trigger,
    ) -> impl Future<Output = Result<PullRequest, Error>> + Send {
        let key = format!("{repository}#{number}");
        let result = {
            let state = self.lock();
            if state.transient() {
                Err(transient_error())
            } else if let Some(code) = state.lapsed.get(&key) {
                Err(Error::workflow(code.clone(), true, None))
            } else if let Some(pr) = state.current.get(&key) {
                Ok(pr.clone())
            } else {
                Err(Error::workflow("pull_request_not_open", true, None))
            }
        };
        ready(result)
    }

    fn read_pull_request_state(
        &self,
        repository: &str,
        number: i64,
    ) -> impl Future<Output = Result<PullRequestState, Error>> + Send {
        let key = format!("{repository}#{number}");
        let result = {
            let state = self.lock();
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

    fn resolve_project(
        &self,
        _repository: &str,
        _sources: &[String],
    ) -> impl Future<Output = Result<ProjectRoute, Error>> + Send {
        ready(Ok(ProjectRoute {
            host: "review-host".to_string(),
            project_id: "project".to_string(),
        }))
    }

    fn ensure_session(
        &self,
        request: &SessionRequest,
    ) -> impl Future<Output = Result<SessionIdentity, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.attempts.push(request.clone());
            let fail = state.start_fail.clone();
            let existing = state
                .sessions
                .iter()
                .find(|(_, existing)| existing.name() == request.name())
                .map(|(id, _)| id.clone());
            if let Some(fail) = fail {
                if let Err(error) = fail(request) {
                    Err(error)
                } else if let Some(id) = existing {
                    Ok(SessionIdentity {
                        host: request.route.host.clone(),
                        session_id: id,
                        worker_instance_id: "worker".to_string(),
                        runtime_generation: "1".to_string(),
                        reviewer_id: request.reviewer.id.clone(),
                    })
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
            } else if let Some(id) = existing {
                Ok(SessionIdentity {
                    host: request.route.host.clone(),
                    session_id: id,
                    worker_instance_id: "worker".to_string(),
                    runtime_generation: "1".to_string(),
                    reviewer_id: request.reviewer.id.clone(),
                })
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
        _request: &SessionRequest,
    ) -> impl Future<Output = Result<Option<SessionIdentity>, Error>> + Send {
        ready(Ok(None))
    }

    fn session_output(
        &self,
        session: &SessionIdentity,
        _protocol: Protocol,
    ) -> impl Future<Output = Result<String, Error>> + Send {
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
                    Ok((request, output))
                }
            }
        };
        let result = match outcome {
            Err(error) => Err(error),
            Ok((request, output)) => Ok(output(&request)),
        };
        ready(result)
    }

    fn remove_session_confirmed(
        &self,
        session: &SessionIdentity,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        let result = {
            let mut state = self.lock();
            if state.remove_failing.contains(&session.session_id) {
                Err(Error::pohunek_code(
                    "runtime_supervision_ambiguous",
                    Box::new(Error::workflow("pohunek_request_failed", false, None)),
                ))
            } else {
                if state.sessions.remove(&session.session_id).is_some() {
                    state.removed.push(session.session_id.clone());
                }
                Ok(())
            }
        };
        ready(result)
    }

    fn list_commentable_lines(
        &self,
        _pull_request: &PullRequest,
    ) -> impl Future<Output = Result<CommentableLines, Error>> + Send {
        let result = {
            let state = self.lock();
            state.commentable.clone()
        };
        ready(Ok(result))
    }

    fn list_deleted_files(
        &self,
        _pull_request: &PullRequest,
    ) -> impl Future<Output = Result<Vec<String>, Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.deleted_reads += 1;
            state.deleted.clone()
        };
        ready(Ok(result))
    }

    fn list_discussion(
        &self,
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

    fn linear_history(
        &self,
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
        pull_request: &PullRequest,
        review: &GitHubReview,
    ) -> impl Future<Output = Result<PublishResult, Error>> + Send {
        let result = {
            let mut state = self.lock();
            if let Some(code) = state.publish_fail.clone() {
                Err(Error::workflow(code, true, None))
            } else if let Some(index) = state
                .published
                .iter()
                .position(|existing| existing.marker == review.marker)
            {
                Ok(PublishResult {
                    decision: review.decision,
                    github_review_id: (index + 1) as i64,
                    review_url: format!("{}#review", pull_request.url),
                    reused: true,
                })
            } else {
                state.published.push(review.clone());
                Ok(PublishResult {
                    decision: review.decision,
                    github_review_id: state.published.len() as i64,
                    review_url: format!("{}#review", pull_request.url),
                    reused: false,
                })
            }
        };
        ready(result)
    }

    fn list_review_requests(
        &self,
        max: i64,
    ) -> impl Future<Output = Result<ReviewRequestList, Error>> + Send {
        let result = {
            let state = self.lock();
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
        transaction: &str,
        plain: &str,
        formatted: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        let result = {
            let mut state = self.lock();
            state.queue_sends += 1;
            state.queue_transactions.push(transaction.to_string());
            if state.queue_sends <= state.queue_failures {
                Err(Error::workflow("matrix_unavailable", false, None))
            } else {
                state.queued.push(QueuedDigest {
                    transaction: transaction.to_string(),
                    plain: plain.to_string(),
                    formatted: formatted.to_string(),
                });
                Ok(())
            }
        };
        ready(result)
    }

    fn notify(
        &self,
        level: Level,
        message: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        {
            let mut state = self.lock();
            state.notes.push(Note {
                level,
                message: message.to_string(),
            });
        }
        ready(Ok(()))
    }
}
