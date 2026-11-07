//! In-memory fake of the [`Integrations`] boundary.
//!
//! Tests change the pull requests, sessions and reviewer outputs while a review runs. Each
//! method answers from the state under the lock and never touches a network or a process.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, ready};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use autoreview_policy::{Level, Protocol, Rule, Trigger, VERIFIER_REVIEWER_ID};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, WORKFLOW_FAILED};
use crate::integrations::{
    CommentableLines, GitHubReview, Integrations, PullRequestState, ReadiedDraft, ReviewRequest,
    ReviewRequestList, SessionRequest,
};
use crate::model::review::PRIOR_FIXED;
use crate::model::{
    Candidate, CandidateList, Discussion, PriorFinding, ProjectRoute, PublishResult, PullRequest,
    RejectedCandidate, SessionIdentity,
};
use crate::protocol::claude::CLAUDE_RESULT_PROTOCOL;
use crate::protocol::request::go_json;
use crate::verify::{
    ClaimFinding, ClaimPrior, VERDICT_CONFIRMED, VERDICT_UPHELD, VerificationClaims,
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
    /// Text of the notifications to hold, and the gate they wait on. `None` holds none.
    pub(crate) held_notes: Option<(String, Arc<Semaphore>)>,
    /// Error code that every notification fails with; `None` lets notifications succeed.
    pub(crate) notify_fail: Option<String>,
    /// Number of notification attempts, failed or not.
    pub(crate) notify_attempts: usize,
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

    /// Holds every notification that contains `fragment` after it is recorded.
    ///
    /// A held notification returns once the returned gate has a permit.
    pub(crate) fn hold_notes(&self, fragment: &str) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        self.lock().held_notes = Some((fragment.to_owned(), Arc::clone(&gate)));
        gate
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
    fn list_candidates(
        &self,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<CandidateList, Error>> + Send {
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

    fn ready_drafts(
        &self,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Vec<ReadiedDraft>, Error>> + Send {
        ready(Ok(Vec::new()))
    }

    fn get_pull_request(
        &self,
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
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
                    Ok((request, output))
                }
            }
        };
        let result = match outcome {
            Err(error) => Err(error),
            // An empty output stands for Go's nil output: the session has not finished yet.
            Ok((request, output)) => {
                Ok(Some(output(&request).into_bytes()).filter(|bytes: &Vec<u8>| !bytes.is_empty()))
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
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
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
                    decision: review.decision.clone(),
                    github_review_id: (index + 1) as i64,
                    review_url: format!("{}#review", pull_request.url),
                    reused: true,
                })
            } else {
                state.published.push(review.clone());
                Ok(PublishResult {
                    decision: review.decision.clone(),
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
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
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
        _cancel: &CancellationToken,
        level: Level,
        message: &str,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        let (fail, gate) = {
            let mut state = self.lock();
            state.notify_attempts += 1;
            let fail = state.notify_fail.clone();
            if fail.is_none() {
                state.notes.push(Note {
                    level,
                    message: message.to_string(),
                });
            }
            let gate = state
                .held_notes
                .as_ref()
                .filter(|(fragment, _)| message.contains(fragment.as_str()))
                .map(|(_, gate)| Arc::clone(gate));
            (fail, gate)
        };
        async move {
            if let Some(code) = fail {
                return Err(Error::workflow(code, false, None));
            }
            if let Some(gate) = gate {
                // The permit goes back when it drops, so one permit releases every held note.
                let _permit = gate.acquire().await;
            }
            Ok(())
        }
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

/// Returns the output of a Codex reviewer run.
fn codex_output(pull_request: &PullRequest, findings: &[Value], status: i64) -> String {
    let report = go_json(&json!({
        "findings": findings,
        "summary": "Full review of the pull request diff; details follow.",
        "tests": ["go test ./..."],
        "status": "completed",
        "reviewed_head_sha": pull_request.head_sha,
    }))
    .expect("the fake encodes the Codex report");
    let encoded = STANDARD.encode(report);
    let prefix = format!("[pohunek-codex-review-result-v1:{encoded}]\n");
    let status_line = format!("[pohunek-codex-review exited with status {status}]\n");
    format!("{prefix}{status_line}")
}

/// Returns the output of a finished Claude session with `structured` as its structured output.
fn claude_structured_output(structured: Value) -> String {
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
    }))
    .expect("the fake encodes the Claude wrapper");
    format!("{wrapper}\n")
}

/// Returns the output of a finished Claude reviewer session.
///
/// The prior verdicts are included only when `prior` is `Some`.
fn claude_output_with_prior(
    pull_request: &PullRequest,
    findings: Vec<Value>,
    prior: Option<Vec<Value>>,
) -> String {
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
    claude_structured_output(structured)
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
        if request.reviewer.protocol == Protocol::CodexEnvelope {
            return codex_output(&request.pull_request, &findings, 0);
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
        claude_output_with_prior(&request.pull_request, claude_findings, Some(verdicts))
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
    claude_structured_output(json!({
        "repository": pr.repository,
        "number": pr.number,
        "head_sha": pr.head_sha,
        "findings": findings,
        "prior_findings": priors,
    }))
}

/// Returns a session output that answers verifier requests by the claim verdicts.
///
/// Every other request goes to `reviewers`.
pub(crate) fn verifying(
    reviewers: impl Fn(&SessionRequest) -> String + Send + Sync + 'static,
    finding: impl Fn(&ClaimFinding) -> &'static str + Send + Sync + 'static,
    prior: impl Fn(&ClaimPrior) -> &'static str + Send + Sync + 'static,
) -> impl Fn(&SessionRequest) -> String + Send + Sync + 'static {
    move |request: &SessionRequest| {
        if request.reviewer.id == VERIFIER_REVIEWER_ID {
            verifier_output(request, &finding, &prior)
        } else {
            reviewers(request)
        }
    }
}

/// Policy document of the integration tests: pipelines, repositories and notifications.
pub(crate) const INTEGRATION_POLICY: &str = include_str!("integration-policy.yaml");
