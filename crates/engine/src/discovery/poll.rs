//! Discovery: the poll that plans every candidate and queues the reviews that are due.
//!
//! Mirrors `cmd/autoreview/discovery.go:204-505`.

// Rust guideline compliant 2026-06-26

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use autoreview_policy::Event;
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::{self, Record, Run};
use time::{OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use super::route::{HostAnswers, HostFailures, host_unavailable};
use super::{Notice, record_event};
use crate::engine::{Clock, Engine, retry_step};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::messages::{go_elapsed_nanos, handoff_reminder_message};
use crate::model::review::APPROVAL_KIND_SUPERSEDED;
use crate::model::{
    Candidate, PollResult, PublishedRecord, PullRequest, RetryRequest, ReviewInput,
};
use crate::store::discovery::save_discovery;
use crate::store::{DiscoveryState, RESULT_SUCCESS, REVIEW_COMPLETED};

/// Start value of the latest attempt, below every stored attempt.
const NO_ATTEMPT: i64 = -1;

/// Error code of a poll that would store more schedule records than the limit.
const STATE_ENTRY_LIMIT_EXCEEDED: &str = "state_entry_limit_exceeded";

/// Error code of a retry for a pull request that is not a candidate.
const REVIEW_REQUEST_NOT_ACTIVE: &str = "review_request_not_active";

/// Error code of a retry whose head is not the current head.
const HEAD_CHANGED: &str = "head_changed";

/// Error code of a retry while a run of the pull request is active.
const REVIEW_ALREADY_RUNNING: &str = "review_already_running";

/// Error code of a retry of a head whose review succeeded or was released.
const REVIEW_ALREADY_COMPLETED: &str = "review_already_completed";

/// Error code of a schedule time that the store cannot write.
const STORE_STATE_INVALID: &str = "store_state_invalid";

/// Error code of a store call that Go makes on a cancelled context.
const STORE_UNAVAILABLE: &str = "store_unavailable";

/// Cause text of a Go store call on a cancelled context, Go's `context.Canceled`.
const CONTEXT_CANCELED: &str = "context canceled";

/// Step name of the candidate search in the retry log.
const STEP_LIST_CANDIDATES: &str = "list_candidates";

/// Step name of the read of a pull request that left the search, in the retry log.
const STEP_READ_PULL_REQUEST: &str = "read_pull_request";

/// Step name of the read of a handed-off pull request's state, in the retry log.
pub(crate) const STEP_READ_PULL_REQUEST_STATE: &str = "read_pull_request_state";

/// Read error codes of a pull request that left the candidates after the search.
const SKIPPABLE_CANDIDATE_CODES: [&str; 4] = [
    "pull_request_not_open",
    "pull_request_draft",
    "review_request_not_active",
    "review_label_removed",
];

/// A review that the poll queues once the discovery state is saved.
struct Dispatch {
    /// Workflow ID of the queued attempt.
    workflow_id: String,
    /// Input of the queued attempt.
    input: ReviewInput,
}

/// One saved discovery pass: its counts and the notices it leaves to send.
pub(crate) struct Pass {
    /// Counts of the pass, as `poll` returns them.
    pub(crate) result: PollResult,
    /// Notifications of the pass, sent after the discovery lock is released.
    pub(crate) notices: Vec<Notice>,
}

impl<I: Integrations, C: Clock> Engine<I, C> {
    /// Plans every candidate under the discovery lock and queues the reviews that are due.
    ///
    /// The notifications of the pass are sent after the lock is released; a failed one is
    /// logged and does not fail the poll.
    ///
    /// # Errors
    ///
    /// Returns `discovery_busy` or `store_unavailable` from the lock, or the error of the pass.
    pub async fn poll(&self, cancel: &CancellationToken) -> Result<PollResult, Error> {
        let pass = self.with_discovery(|| self.poll_pass(cancel, None)).await?;
        self.send_notices(cancel, pass.notices).await;
        Ok(pass.result)
    }

    /// Runs one discovery pass; `force` asks for a new attempt of one failed head.
    ///
    /// The caller holds the discovery lock. The queued reviews, the superseded waits and the
    /// discovery state are written in one transaction at the end. The pass sends no
    /// notification; it returns them for the caller to send after the lock is released.
    ///
    /// # Errors
    ///
    /// Returns the error of the candidate search, of the store, of the review planning or of
    /// `force`, and `state_entry_limit_exceeded` when the schedule is full.
    pub(crate) async fn poll_pass(
        &self,
        cancel: &CancellationToken,
        force: Option<&RetryRequest>,
    ) -> Result<Pass, Error> {
        // A failure here leaves the drafts for the next pass and holds up no other review.
        if let Err(err) = self.integration.ready_drafts(cancel).await
            && !cancel.is_cancelled()
        {
            tracing::warn!(
                error_code = error_code(&err),
                "marking drafts ready for review failed"
            );
        }
        let listed = retry_step(
            STEP_LIST_CANDIDATES,
            self.step_policy(),
            &self.clock,
            cancel,
            || self.list_classified(cancel),
        )
        .await?;
        live(cancel)?;
        let history = self.store.run_history(self.config.state_entry_limit)?;
        let now = self.clock.wall_now().to_offset(UtcOffset::UTC);
        live(cancel)?;
        let mut state = self.store.load_discovery()?;
        let open = i64::try_from(listed.candidates.len()).unwrap_or(i64::MAX);
        let mut present: BTreeMap<String, Candidate> = BTreeMap::new();
        for candidate in listed.candidates {
            present.insert(candidate.pull_request.key(), candidate);
        }
        let mut notices = Vec::new();
        self.report_rejected(&mut state, &listed.rejected, &mut notices)?;
        // The review queue and `review` read the skips of the last pass; a skip sends no notice.
        state.skipped = listed
            .skipped
            .iter()
            .map(|item| (item.key(), item.reason.clone()))
            .collect();
        let mut result = PollResult {
            open,
            ..PollResult::default()
        };
        let supersede = self
            .supersede_awaiting(cancel, &mut state, &present, &history)
            .await?;
        result.superseded = i64::try_from(supersede.len()).unwrap_or(i64::MAX);
        self.remind_handoffs(cancel, &mut state, &present, now, &mut notices)
            .await?;
        if let Some(force) = force {
            self.apply_force(&mut state, &present, &history, force, now)?;
        }
        let mut dispatches = Vec::new();
        // A host that fails once is not asked again in this pass, so it costs one timeout.
        let mut host_failures = HostFailures::new();
        let mut host_answers = HostAnswers::new();
        for (key, candidate) in &present {
            let pr = &candidate.pull_request;
            let entries = i64::try_from(state.schedule.len()).unwrap_or(i64::MAX);
            if !state.schedule.contains_key(key) && entries >= self.config.state_entry_limit {
                return Err(Error::workflow(STATE_ENTRY_LIMIT_EXCEEDED, true, None));
            }
            if let Some(published) = state.published.get(key)
                && published.covers(pr)
                && !published.retargeted(pr)
            {
                state.schedule.insert(
                    key.clone(),
                    Record {
                        identity: pr.target().identity(),
                        phase: phase::COMPLETED.to_owned(),
                        workflow_id: published.workflow_id.clone(),
                        ..Record::default()
                    },
                );
                continue;
            }
            // The head already has a review that waits for the user's approval.
            if state
                .awaiting
                .get(key)
                .is_some_and(|waiting| waiting.matches(pr))
            {
                continue;
            }
            let target = pr.target();
            // A key without a record plans from Go's zero record, which `Record::default` is.
            let mut previous = state.schedule.get(key).cloned().unwrap_or_default();
            let retarget = match state.published.get(key) {
                Some(published) if published.covers(pr) => {
                    self.retarget_attempt(cancel, published, pr, &history)?
                }
                _ => None,
            };
            let mut runs = Cow::Borrowed(history.as_slice());
            if retarget.is_some() {
                // The ended runs of this head reviewed another base branch, so none of them
                // completes the review. The record of the retarget names no workflow until it
                // dispatches; it is kept, so its debounce runs once.
                let identity = target.identity();
                runs = Cow::Owned(
                    history
                        .iter()
                        .filter(|run| run.active() || run.target.identity() != identity)
                        .cloned()
                        .collect(),
                );
                if previous.identity != identity || !previous.workflow_id.is_empty() {
                    previous = Record {
                        blocking_workflows: previous.blocking_workflows,
                        ..Record::default()
                    };
                }
            }
            let mut decision =
                scheduler::plan(previous, &target, &runs, now, &self.config.schedule)
                    .map_err(schedule_error)?;
            if let Some(attempt) = retarget {
                decision.record.attempt = decision.record.attempt.max(attempt);
                if decision.dispatch {
                    tracing::info!(
                        review_id = %pr.review_id(),
                        reason = "base_branch_changed",
                        base_branch = %pr.base_branch,
                        "reviewed head scheduled again for a full review"
                    );
                }
            }
            if decision.notify {
                let message = scheduler::notification(&target, &decision.record);
                notices.push(self.notice(record_event(&decision.record), key, message)?);
            }
            if decision.dispatch {
                // A route failure is not retried here. A failed host leaves the review due for
                // the next pass; any other route failure follows the scheduler's retry policy.
                let sources = &candidate.rule.pipeline.project_sources;
                match self
                    .resolve_route(
                        cancel,
                        &pr.repository,
                        sources,
                        &mut host_failures,
                        &mut host_answers,
                    )
                    .await
                {
                    Err(err) if cancel.is_cancelled() => return Err(err),
                    Err(err) if host_unavailable(&err) => {
                        decision.record =
                            scheduler::wait_for_host(decision.record, error_code(&err));
                        // Each host outage is announced once, by the first review that waits.
                        let failed = self.failed_route_hosts(&pr.repository, &host_failures);
                        let new: Vec<&str> = failed
                            .iter()
                            .filter(|host| !state.host_outages.contains_key(*host))
                            .map(String::as_str)
                            .collect();
                        if !new.is_empty() {
                            let message =
                                scheduler::host_wait_notification(&target, &decision.record, &new);
                            notices.push(self.notice(Event::ReviewStopped, key, message)?);
                        }
                        for host in failed {
                            if let Some(failure) = host_failures.get(&host) {
                                state.host_outages.insert(host, failure.code.clone());
                            }
                        }
                    }
                    Err(err) => {
                        decision.record = scheduler::after_failure(
                            decision.record,
                            error_code(&err),
                            now,
                            &self.config.schedule,
                        )
                        .map_err(schedule_error)?;
                        let message = scheduler::notification(&target, &decision.record);
                        notices.push(self.notice(record_event(&decision.record), key, message)?);
                    }
                    Ok(route) => {
                        decision.record.awaits_host = false;
                        let published = state.published.get(key);
                        let (scope, prior) = self.plan_review(cancel, candidate, published).await?;
                        let handoff = self.notes_handoff(published, pr)?;
                        let mode = scope.mode.clone();
                        let prior_findings = prior.len();
                        let attempt = decision.record.attempt;
                        let input =
                            self.build_input(candidate, route, scope, prior, handoff, attempt)?;
                        let author_class = input.author_class.as_str().to_owned();
                        decision.record.workflow_id = pr.workflow_id(attempt);
                        dispatches.push(Dispatch {
                            workflow_id: decision.record.workflow_id.clone(),
                            input,
                        });
                        result.dispatched = result.dispatched.wrapping_add(1);
                        tracing::info!(
                            review_id = %pr.review_id(),
                            rule = %candidate.rule.match_glob,
                            pipeline = %candidate.rule.pipeline.name,
                            author_class = %author_class,
                            scope = %mode,
                            prior_findings,
                            attempt = attempt.wrapping_add(1),
                            "review scheduled"
                        );
                    }
                }
            }
            state.schedule.insert(key.clone(), decision.record);
        }
        self.probe_outage_hosts(
            cancel,
            &state.host_outages,
            &host_failures,
            &mut host_answers,
        )
        .await?;
        // An outage ends when its host answers or leaves `pohunek.hosts`; a host that answered
        // and then failed in this pass is still out.
        state.host_outages.retain(|host, _| {
            self.config.pohunek_hosts.contains(host)
                && (host_failures.contains_key(host) || !host_answers.contains(host))
        });
        self.prune_discovery(&mut state, &present, now);
        live(cancel)?;
        self.store.in_tx(|transaction| {
            for item in &dispatches {
                self.store
                    .enqueue_review(transaction, &item.workflow_id, &item.input)?;
            }
            for workflow_id in &supersede {
                self.store.decide_approval_tx(
                    transaction,
                    workflow_id,
                    APPROVAL_KIND_SUPERSEDED,
                )?;
            }
            save_discovery(transaction, &state)
        })?;
        Ok(Pass { result, notices })
    }

    /// Returns the first attempt of the review that a retarget of a reviewed head asks for.
    ///
    /// Returns `None` when a run of this head that was not superseded already reviewed the
    /// current base branch. The attempt follows every run of the head and never repeats the
    /// workflow of `published`.
    ///
    /// # Errors
    ///
    /// Returns the error of the store.
    fn retarget_attempt(
        &self,
        cancel: &CancellationToken,
        published: &PublishedRecord,
        pr: &PullRequest,
        history: &[Run],
    ) -> Result<Option<i64>, Error> {
        let identity = pr.target().identity();
        let mut attempt = 0_i64;
        for run in history
            .iter()
            .filter(|run| run.target.identity() == identity)
        {
            attempt = attempt.max(run.attempt.wrapping_add(1));
            // A superseded run reviewed nothing, so its base branch still needs a review.
            if !run.active() && scheduler::superseded(&scheduler::failure_code(&run.failure)) {
                continue;
            }
            live(cancel)?;
            let stored = self.store.get_review(&run.workflow_id)?;
            if stored.is_some_and(|review| review.input.pull_request.base_branch == pr.base_branch)
            {
                return Ok(None);
            }
        }
        // The run of the published review may be pruned while its record is kept.
        if pr.workflow_id(attempt) == published.workflow_id {
            attempt = attempt.wrapping_add(1);
        }
        Ok(Some(attempt))
    }

    /// Returns the waits that no longer match their pull request and drops ended waits.
    ///
    /// A pull request missing from search is read directly first, because search lags behind.
    ///
    /// # Errors
    ///
    /// Returns the read error of a pull request unless it is one that ends the candidate.
    async fn supersede_awaiting(
        &self,
        cancel: &CancellationToken,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        own: &[Run],
    ) -> Result<Vec<String>, Error> {
        let finished: BTreeSet<&str> = own
            .iter()
            .filter(|run| run.status == REVIEW_COMPLETED)
            .map(|run| run.workflow_id.as_str())
            .collect();
        let mut superseded = Vec::new();
        let awaiting = state.awaiting.clone();
        for (key, record) in &awaiting {
            if finished.contains(record.workflow_id.as_str()) {
                state.awaiting.remove(key);
                continue;
            }
            if let Some(candidate) = present.get(key) {
                if record.matches(&candidate.pull_request) {
                    continue;
                }
            } else {
                let read = retry_step(
                    STEP_READ_PULL_REQUEST,
                    self.step_policy(),
                    &self.clock,
                    cancel,
                    || {
                        self.integration.get_pull_request(
                            cancel,
                            &record.repository,
                            record.number,
                            &record.trigger,
                        )
                    },
                )
                .await;
                match read {
                    Ok(pr) if record.matches(&pr) => continue,
                    Ok(_) => {}
                    Err(err) if !skippable_candidate_error(&err) => return Err(err),
                    Err(_) => {}
                }
            }
            superseded.push(record.workflow_id.clone());
            state.awaiting.remove(key);
        }
        Ok(superseded)
    }

    /// Reminds the user once of handed-off notes that the author left waiting.
    ///
    /// The reminder is due when the author did not request the review again within the policy's
    /// reminder time. The hand-off of a pull request that is no longer open ends. A failed read
    /// is retried on the next pass, so it holds up no review. The reminders go to `notices`.
    ///
    /// # Errors
    ///
    /// Returns `configuration_invalid` without a policy, or the read error when `cancel` is
    /// cancelled.
    async fn remind_handoffs(
        &self,
        cancel: &CancellationToken,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        now: OffsetDateTime,
        notices: &mut Vec<Notice>,
    ) -> Result<(), Error> {
        let after = self.policy()?.handoff_reminder();
        if after.is_zero() {
            return Ok(());
        }
        let mut due = Vec::new();
        for (key, record) in &state.published {
            if !record.awaits_author || record.reminded || present.contains_key(key) {
                continue;
            }
            let is_due = u128::try_from(go_elapsed_nanos(now, record.published_at))
                .is_ok_and(|elapsed| elapsed >= after.as_nanos());
            if is_due {
                due.push(key.clone());
            }
        }
        for key in due {
            let Some(mut record) = state.published.get(&key).cloned() else {
                continue;
            };
            let read = retry_step(
                STEP_READ_PULL_REQUEST_STATE,
                self.step_policy(),
                &self.clock,
                cancel,
                || {
                    self.integration.read_pull_request_state(
                        cancel,
                        &record.repository,
                        record.number,
                    )
                },
            )
            .await;
            match read {
                Err(err) if cancel.is_cancelled() => return Err(err),
                Err(err) => {
                    tracing::warn!(
                        repository = %record.repository,
                        pull_request = record.number,
                        error_code = error_code(&err),
                        "notes hand-off unreadable"
                    );
                    continue;
                }
                Ok(current) if !current.open => {
                    record.awaits_author = false;
                }
                Ok(current) if current.requested => {
                    // Search lags behind; the next pass reviews it.
                    continue;
                }
                Ok(current) => {
                    let message = handoff_reminder_message(
                        self.config.excerpt_characters,
                        &record,
                        &current,
                        now,
                    );
                    notices.push(self.notice(Event::HandoffReminder, &key, message)?);
                    record.reminded = true;
                }
            }
            state.published.insert(key, record);
        }
        Ok(())
    }

    /// Turns the failed latest attempt of a pull request head into a due retry.
    ///
    /// # Errors
    ///
    /// Returns `review_request_not_active`, `head_changed`, `review_already_running` or
    /// `review_already_completed`, all permanent.
    fn apply_force(
        &self,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        own: &[Run],
        force: &RetryRequest,
        now: OffsetDateTime,
    ) -> Result<(), Error> {
        let key = format!("{}#{}", force.repository, force.number);
        let Some(candidate) = present.get(&key) else {
            return Err(Error::workflow(REVIEW_REQUEST_NOT_ACTIVE, true, None));
        };
        let target = candidate.pull_request.target();
        if !target.head_sha.starts_with(&force.head_sha) {
            return Err(Error::workflow(HEAD_CHANGED, true, None));
        }
        // The run history decides, not the schedule record: the record may still say running
        // from a poll that saw the run before it ended.
        let mut forced = Record {
            identity: target.identity(),
            phase: phase::RETRY_WAIT.to_owned(),
            due_at: now,
            ..Record::default()
        };
        // The forced record names the latest attempt of this head, so the planner treats its
        // failure as handled. The schedule record keeps the attempt counter when the history
        // no longer holds that attempt, so a retry never reuses an earlier workflow ID.
        if let Some(previous) = state.schedule.get(&key)
            && previous.identity == forced.identity
        {
            forced.attempt = previous.attempt.wrapping_add(1);
            forced.workflow_id = previous.workflow_id.clone();
        }
        let mut latest_attempt = NO_ATTEMPT;
        for run in own {
            if run.target.key() != key {
                continue;
            }
            if run.active() {
                return Err(Error::workflow(REVIEW_ALREADY_RUNNING, true, None));
            }
            if run.target.identity() != forced.identity {
                continue;
            }
            if run.result == RESULT_SUCCESS || run.released {
                return Err(Error::workflow(REVIEW_ALREADY_COMPLETED, true, None));
            }
            if run.attempt > latest_attempt {
                latest_attempt = run.attempt;
                forced.workflow_id = run.workflow_id.clone();
                forced.attempt = forced.attempt.max(run.attempt.wrapping_add(1));
            }
        }
        state.schedule.insert(key, forced);
        Ok(())
    }

    /// Keeps the discovery state bounded.
    ///
    /// Records of pull requests that left every trigger are withdrawn, and dropped once they
    /// ended and block nothing. Published decisions older than the retention are dropped.
    fn prune_discovery(
        &self,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        now: OffsetDateTime,
    ) {
        state.schedule.retain(|key, record| {
            if present.contains_key(key) {
                return true;
            }
            match record.phase.as_str() {
                phase::WAITING | phase::RETRY_WAIT => {
                    record.phase = phase::WITHDRAWN.to_owned();
                    true
                }
                phase::WITHDRAWN | phase::COMPLETED | phase::FAILED => {
                    !record.blocking_workflows.is_empty()
                }
                _ => true,
            }
        });
        let retention = self.config.published_retention.as_nanos();
        state.published.retain(|_, record| {
            !u128::try_from(go_elapsed_nanos(now, record.published_at))
                .is_ok_and(|elapsed| elapsed > retention)
        });
    }
}

/// Reports a pull request that changed between the search and its detail read.
///
/// It is simply not a candidate in this poll (`github.go:507-515`).
fn skippable_candidate_error(err: &Error) -> bool {
    SKIPPABLE_CANDIDATE_CODES.contains(&error_code(err))
}

/// Fails as Go's store does on a cancelled context, so a cancelled pass writes nothing.
///
/// The Rust store ignores `cancel`; Go's `database/sql` refuses every call once the context ends.
pub(crate) fn live(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        return Err(Error::workflow(
            STORE_UNAVAILABLE,
            false,
            Some(CONTEXT_CANCELED.into()),
        ));
    }
    Ok(())
}

/// Wraps a schedule time outside the range of `time`; Go fails writing it as JSON.
fn schedule_error(err: autoreview_schedule::Error) -> Error {
    Error::workflow(STORE_STATE_INVALID, true, Some(Box::new(err)))
}
