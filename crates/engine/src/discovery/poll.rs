//! Discovery: the poll that plans every candidate and queues the reviews that are due.

// Rust guideline compliant 2026-06-26

use std::collections::{BTreeMap, BTreeSet};

use autoreview_policy::Event;
use autoreview_schedule::phase;
use autoreview_schedule::scheduler::{self, Record, Retarget, Run, Target};
use time::{OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

use super::route::{HostPass, host_unavailable};
use super::{Notice, record_event};
use crate::config::Credential;
use crate::engine::{Clock, Engine, STEP_PUBLICATION, is_permanent, retry_step};
use crate::error::{Error, error_code};
use crate::integrations::Integrations;
use crate::messages::{go_elapsed_nanos, handoff_reminder_message};
use crate::model::review::APPROVAL_KIND_SUPERSEDED;
use crate::model::{
    Candidate, Decision, PollResult, PublishResult, PullRequest, RetryRequest, ReviewInput,
};
use crate::review::input::{RenderOutcome, STEP_RENDERED};
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

/// Step name of the read of a pull request that left the search, in the retry log.
const STEP_READ_PULL_REQUEST: &str = "read_pull_request";

/// Step name of the read of a handed-off pull request's state, in the retry log.
pub(crate) const STEP_READ_PULL_REQUEST_STATE: &str = "read_pull_request_state";

/// Step name of the lookup of a published approval on GitHub, in the retry log.
const STEP_FIND_APPROVAL: &str = "find_published_approval";

/// Log reason of a head scheduled again because it targets another base branch.
const REASON_BASE_BRANCH_CHANGED: &str = "base_branch_changed";

/// Log reason of a head scheduled again because GitHub dismissed its approval.
const REASON_APPROVAL_DISMISSED: &str = "approval_dismissed";

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

/// What GitHub shows of an approval that completes a head on its base branch.
enum Approval {
    /// The approval stands, or the run published no approval.
    Stands,
    /// GitHub dismissed the approval, or it is gone.
    Dismissed,
    /// The lookup failed; the next pass asks again.
    Unknown,
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
    /// `force`, and `state_entry_limit_exceeded` when the schedule is full. A `force` of a pull
    /// request whose credential could not search gets that search's code, not permanent.
    pub(crate) async fn poll_pass(
        &self,
        cancel: &CancellationToken,
        force: Option<&RetryRequest>,
    ) -> Result<Pass, Error> {
        for credential in self.config.credentials.iter() {
            // A failure leaves the drafts for the next pass and holds up no other review.
            if let Err(err) = self.integration.ready_drafts(cancel, credential).await
                && !cancel.is_cancelled()
            {
                tracing::warn!(
                    credential = %credential.name,
                    error_code = error_code(&err),
                    "marking drafts ready for review failed"
                );
            }
        }
        let listed = self.classify_listed(cancel, true).await?;
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
        let failed = &listed.failed_credentials;
        // The records of an unsearched repository stay as they are until its search succeeds.
        let held = |key: &str| self.unsearched(failed, key_repository(key));
        let mut notices = Vec::new();
        let kept: Vec<_> = (state.rejected.iter().filter(|(key, _)| held(key)))
            .map(|(key, code)| (key.clone(), code.clone()))
            .collect();
        self.report_rejected(&mut state, &listed.rejected, &mut notices)?;
        state.rejected.extend(kept);
        // The review queue and `review` read the skips of the last pass; a skip sends no notice.
        state.skipped.retain(|key, _| held(key));
        let skips = listed.skipped.iter();
        state
            .skipped
            .extend(skips.map(|item| (item.key(), item.reason.clone())));
        let mut result = PollResult {
            open,
            ..PollResult::default()
        };
        let supersede = self
            .supersede_awaiting(cancel, &mut state, &present, &history, failed)
            .await?;
        result.superseded = i64::try_from(supersede.len()).unwrap_or(i64::MAX);
        self.remind_handoffs(cancel, &mut state, &present, failed, now, &mut notices)
            .await?;
        if let Some(force) = force {
            // The search that would find it failed, so the pull request is unknown, not gone.
            if let Ok(credential) = self.config.credentials.serving(&force.repository)
                && let Some(code) = failed.get(&credential.name)
            {
                let cause = format!("the search of credential {} failed", credential.name);
                return Err(Error::workflow(code.clone(), false, Some(cause.into())));
            }
            self.apply_force(cancel, &mut state, &present, &history, force, now)?;
        }
        let mut dispatches = Vec::new();
        // A host that fails once is not asked again in this pass, so it costs one timeout; each
        // host's state is read at most once in the pass.
        let mut host_pass = HostPass::default();
        for (key, candidate) in &present {
            let pr = &candidate.pull_request;
            let entries = i64::try_from(state.schedule.len()).unwrap_or(i64::MAX);
            if !state.schedule.contains_key(key) && entries >= self.config.state_entry_limit {
                return Err(Error::workflow(STATE_ENTRY_LIMIT_EXCEEDED, true, None));
            }
            // A record that follows another run came back from another base branch, where
            // GitHub may have dismissed the published approval; it is planned below.
            if let Some(published) = state.published.get(key)
                && published.covers(pr)
                && !published.retargeted(pr)
                && !state.schedule.get(key).is_some_and(|previous| {
                    follows_another_run(previous, &pr.target(), &published.workflow_id)
                })
            {
                let mut completed = Record {
                    identity: pr.target().identity(),
                    phase: phase::COMPLETED.to_owned(),
                    workflow_id: published.workflow_id.clone(),
                    ..Record::default()
                };
                // A failure handled on another base branch stays known, so a return there does
                // not notify it again or restart its retries.
                if let Some(previous) = state.schedule.get(key)
                    && previous.identity == completed.identity
                {
                    completed.handled_workflow = previous.handled_workflow.clone();
                    completed.handled_retries = previous.handled_retries;
                }
                state.schedule.insert(key.clone(), completed);
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
            let previous = state.schedule.get(key).cloned().unwrap_or_default();
            // A published review that covers the head gets here when it names another base
            // branch or when the head came back from one.
            let (reason, mut decision) = match state.published.get(key) {
                Some(published) if published.covers(pr) => {
                    let run_bases = self.run_bases(cancel, &target, &history)?;
                    let retarget = Retarget {
                        base_branch: &pr.base_branch,
                        run_bases: &run_bases,
                        published_workflow: &published.workflow_id,
                    };
                    let workflow_id = |attempt: i64| pr.workflow_id(attempt);
                    let planned = scheduler::plan_retarget(
                        &previous,
                        &target,
                        &history,
                        &retarget,
                        workflow_id,
                        now,
                        &self.config.schedule,
                    )
                    .map_err(schedule_error)?;
                    match planned {
                        Some(decision) => (Some(REASON_BASE_BRANCH_CHANGED), decision),
                        None => {
                            let on_base = scheduler::plan_on_base(
                                previous.clone(),
                                &target,
                                &history,
                                &retarget,
                                workflow_id,
                                now,
                                &self.config.schedule,
                            )
                            .map_err(schedule_error)?;
                            // A completion on a run of this base branch that the record did not
                            // follow asks GitHub once whether the run's approval still stands.
                            let run = on_base.record.workflow_id.as_str();
                            let returned = on_base.record.phase == phase::COMPLETED
                                && follows_another_run(&previous, &target, run)
                                && run_bases
                                    .get(run)
                                    .is_some_and(|base| *base == pr.base_branch);
                            // A record that stopped before any run stays stopped without a
                            // lookup: its route failed, and `retry` dispatches it again.
                            let approval = if !returned {
                                Approval::Stands
                            } else if failed_before_run(&previous, &target) {
                                Approval::Dismissed
                            } else {
                                self.approval_stands(cancel, pr, run).await?
                            };
                            match approval {
                                Approval::Stands => (None, on_base),
                                // The previous record stays, so the next pass asks again.
                                Approval::Unknown => continue,
                                Approval::Dismissed => (
                                    Some(REASON_APPROVAL_DISMISSED),
                                    scheduler::plan_unreviewed(
                                        &previous,
                                        &target,
                                        &history,
                                        &retarget,
                                        workflow_id,
                                        now,
                                        &self.config.schedule,
                                    )
                                    .map_err(schedule_error)?,
                                ),
                            }
                        }
                    }
                }
                _ => (
                    None,
                    scheduler::plan(previous, &target, &history, now, &self.config.schedule)
                        .map_err(schedule_error)?,
                ),
            };
            if let Some(reason) = reason
                && decision.dispatch
            {
                tracing::info!(
                    review_id = %pr.review_id(),
                    reason,
                    base_branch = %pr.base_branch,
                    "reviewed head scheduled again for a full review"
                );
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
                    .resolve_route(cancel, &pr.repository, sources, &mut host_pass)
                    .await
                {
                    Err(err) if cancel.is_cancelled() => return Err(err),
                    Err(err) if host_unavailable(&err) => {
                        decision.record =
                            scheduler::wait_for_host(decision.record, error_code(&err));
                        // Each host outage is announced once, by the first review that waits.
                        let failed = self.failed_route_hosts(&pr.repository, &host_pass.failures);
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
                            if let Some(failure) = host_pass.failures.get(&host) {
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
            &host_pass.failures,
            &mut host_pass.answers,
        )
        .await?;
        // An outage ends when its host answers or leaves `pohunek.hosts`; a host that answered
        // and then failed in this pass is still out.
        state.host_outages.retain(|host, _| {
            self.config.pohunek_hosts.contains(host)
                && (host_pass.failures.contains_key(host) || !host_pass.answers.contains(host))
        });
        self.prune_discovery(&mut state, &present, failed, now);
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

    /// Returns the base branch of the stored input of each run of `target`, by workflow ID.
    ///
    /// A run whose stored review is gone has no entry.
    ///
    /// # Errors
    ///
    /// Returns the error of the store.
    fn run_bases(
        &self,
        cancel: &CancellationToken,
        target: &Target,
        history: &[Run],
    ) -> Result<BTreeMap<String, String>, Error> {
        let identity = target.identity();
        let mut bases = BTreeMap::new();
        for run in history
            .iter()
            .filter(|run| run.target.identity() == identity)
        {
            live(cancel)?;
            if let Some(review) = self.store.get_review(&run.workflow_id)? {
                bases.insert(
                    run.workflow_id.clone(),
                    review.input.pull_request.base_branch,
                );
            }
        }
        Ok(bases)
    }

    /// Returns the credential that the stored review `workflow_id` reads and publishes with.
    ///
    /// `None` when the review is not stored or its credential no longer fits it.
    ///
    /// # Errors
    ///
    /// Returns the error of the store.
    fn stored_credential(&self, workflow_id: &str) -> Result<Option<&Credential>, Error> {
        let Some(review) = self.store.get_review(workflow_id)? else {
            return Ok(None);
        };
        let input = &review.input;
        let name = self.review_credential(workflow_id, input, false)?;
        let repository = &input.pull_request.repository;
        let credentials = &self.config.credentials;
        Ok((credentials.reviewing(&name, repository, &input.publisher_login)).ok())
    }

    /// Reports whether the approval that `workflow_id` published on the head of `pr` stands.
    ///
    /// The lookup is the one of the publication, by the marker of the run's stored rendered
    /// review, with the run's credential; GitHub's `DISMISSED` state does not match. A run that
    /// published no approval stands. A failed lookup is `Unknown`; a permanent failure, or a
    /// credential that no longer fits the run, keeps the approval standing.
    ///
    /// # Errors
    ///
    /// Returns the error of the store, or the lookup error when `cancel` is cancelled.
    async fn approval_stands(
        &self,
        cancel: &CancellationToken,
        pr: &PullRequest,
        workflow_id: &str,
    ) -> Result<Approval, Error> {
        live(cancel)?;
        let published = self
            .store
            .load_step::<PublishResult>(workflow_id, STEP_PUBLICATION)?;
        if !published.is_some_and(|result| result.decision == Decision::Approve) {
            return Ok(Approval::Stands);
        }
        live(cancel)?;
        let rendered = self
            .store
            .load_step::<RenderOutcome>(workflow_id, STEP_RENDERED)?;
        let Some(rendered) = rendered.filter(|rendered| rendered.error_code.is_empty()) else {
            return Ok(Approval::Stands);
        };
        live(cancel)?;
        let Some(credential) = self.stored_credential(workflow_id)? else {
            tracing::warn!(
                review_id = %pr.review_id(),
                workflow_id,
                "published approval not looked up: its credential no longer fits the review"
            );
            return Ok(Approval::Stands);
        };
        let found = retry_step(
            STEP_FIND_APPROVAL,
            self.step_policy(),
            &self.clock,
            cancel,
            || {
                self.integration
                    .find_published_review(cancel, credential, pr, &rendered.review)
            },
        )
        .await;
        match found {
            Ok(Some(_)) => Ok(Approval::Stands),
            Ok(None) => Ok(Approval::Dismissed),
            Err(err) if cancel.is_cancelled() => Err(err),
            Err(err) => {
                let permanent = is_permanent(&err);
                tracing::warn!(
                    review_id = %pr.review_id(),
                    workflow_id,
                    error_code = error_code(&err),
                    permanent,
                    "published approval lookup failed"
                );
                if permanent {
                    Ok(Approval::Stands)
                } else {
                    Ok(Approval::Unknown)
                }
            }
        }
    }

    /// Returns the waits that no longer match their pull request and drops ended waits.
    ///
    /// A pull request missing from search is read directly first, because search lags behind.
    /// A wait of an unsearched repository stays; one that no credential serves ends unread.
    /// The read uses the credential of the waiting review; a wait whose credential no longer
    /// fits stays unread.
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
        failed: &BTreeMap<String, String>,
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
            } else if self.unsearched(failed, &record.repository) {
                continue;
            } else if self.config.credentials.serving(&record.repository).is_err() {
                tracing::info!(
                    repository = %record.repository,
                    pull_request = record.number,
                    "approval wait ended: no credential serves the repository"
                );
            } else {
                live(cancel)?;
                let Some(credential) = self.stored_credential(&record.workflow_id)? else {
                    // An approval stops the review with `credential_not_covered`.
                    tracing::info!(
                        repository = %record.repository,
                        pull_request = record.number,
                        "approval wait kept unread: its credential no longer fits the review"
                    );
                    continue;
                };
                let read = retry_step(
                    STEP_READ_PULL_REQUEST,
                    self.step_policy(),
                    &self.clock,
                    cancel,
                    || {
                        self.integration.get_pull_request(
                            cancel,
                            credential,
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
    /// An unsearched repository is not read; the hand-off of one that no credential serves ends.
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
        failed: &BTreeMap<String, String>,
        now: OffsetDateTime,
        notices: &mut Vec<Notice>,
    ) -> Result<(), Error> {
        let after = self.policy()?.handoff_reminder();
        if after.is_zero() {
            return Ok(());
        }
        let mut due = Vec::new();
        for (key, record) in &state.published {
            if !record.awaits_author
                || record.reminded
                || present.contains_key(key)
                || self.unsearched(failed, &record.repository)
            {
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
            if self.config.credentials.serving(&record.repository).is_err() {
                tracing::info!(
                    repository = %record.repository,
                    pull_request = record.number,
                    "notes hand-off ended: no credential serves the repository"
                );
                record.awaits_author = false;
                state.published.insert(key, record);
                continue;
            }
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
    /// Only a run that reviewed the current base branch completes the head. A run whose stored
    /// input names another base branch does not; one without a stored base branch does. A later
    /// failed run stored for the current base branch, as of a review that replaced a dismissed
    /// approval, leaves the head to retry.
    ///
    /// # Errors
    ///
    /// Returns `review_request_not_active`, `head_changed`, `review_already_running` or
    /// `review_already_completed`, all permanent, and the error of the store.
    fn apply_force(
        &self,
        cancel: &CancellationToken,
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
        let run_bases = self.run_bases(cancel, &target, own)?;
        let base_branch = &candidate.pull_request.base_branch;
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
            // The notified failure stays known; the retry count restarts, as a retry asks.
            forced.handled_workflow = previous.handled_workflow.clone();
        }
        let mut latest_attempt = NO_ATTEMPT;
        // The latest attempts of the current base branch that completed and that failed.
        let mut completed_here = NO_ATTEMPT;
        let mut failed_here = NO_ATTEMPT;
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
            let other_base = run_bases
                .get(&run.workflow_id)
                .is_some_and(|base| !base.is_empty() && base != base_branch);
            if run.result == RESULT_SUCCESS || run.released {
                if !other_base {
                    completed_here = completed_here.max(run.attempt);
                }
                // A completed review is not retried; it only raises the attempt.
                forced.attempt = forced.attempt.max(run.attempt.wrapping_add(1));
                continue;
            }
            // Only a failure stored for this base branch reopens it; an unknown one does not.
            if run_bases.get(&run.workflow_id) == Some(base_branch) {
                failed_here = failed_here.max(run.attempt);
            }
            if run.attempt > latest_attempt {
                latest_attempt = run.attempt;
                forced.workflow_id = run.workflow_id.clone();
                forced.attempt = forced.attempt.max(run.attempt.wrapping_add(1));
            }
        }
        // A dispatch that failed at its route left no run; its attempt is a failure here. After
        // a completion here the planner does not see the other failed runs, so the forced record
        // names no run, as the stopped one did.
        if let Some(previous) = state.schedule.get(&key)
            && failed_before_run(previous, &target)
        {
            failed_here = failed_here.max(previous.attempt);
            if completed_here > NO_ATTEMPT {
                forced.workflow_id = String::new();
            }
        }
        if completed_here > failed_here {
            return Err(Error::workflow(REVIEW_ALREADY_COMPLETED, true, None));
        }
        state.schedule.insert(key, forced);
        Ok(())
    }

    /// Keeps the discovery state bounded.
    ///
    /// Records of pull requests that left every trigger are withdrawn, and dropped once they
    /// ended and block nothing; those of an unsearched repository stay. Published decisions
    /// older than the retention are dropped.
    fn prune_discovery(
        &self,
        state: &mut DiscoveryState,
        present: &BTreeMap<String, Candidate>,
        failed: &BTreeMap<String, String>,
        now: OffsetDateTime,
    ) {
        state.schedule.retain(|key, record| {
            if present.contains_key(key) || self.unsearched(failed, key_repository(key)) {
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

/// Reports whether `previous`, the record of the head of `target`, follows another run.
///
/// The record of a head that completed on `workflow_id` names that run. A record of the head
/// that names another run, or none, came back from another base branch.
fn follows_another_run(previous: &Record, target: &Target, workflow_id: &str) -> bool {
    previous.identity == target.identity() && previous.workflow_id != workflow_id
}

/// Reports whether `previous`, the record of the head of `target`, stopped before any run.
///
/// A dispatch whose route fails with a code that is not retried leaves a `failed` record that
/// names no run.
fn failed_before_run(previous: &Record, target: &Target) -> bool {
    previous.identity == target.identity()
        && previous.phase == phase::FAILED
        && previous.workflow_id.is_empty()
}

/// Returns the repository of a `repository#number` key.
fn key_repository(key: &str) -> &str {
    key.rsplit_once('#')
        .map_or(key, |(repository, _)| repository)
}

/// Reports a pull request that changed between the search and its detail read.
///
/// It is simply not a candidate in this poll.
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
