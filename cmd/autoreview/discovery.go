package main

import (
	"context"
	"database/sql"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

// Discovery state entries.
const (
	stateSchedule  = "schedule"
	statePublished = "published"
	stateAwaiting  = "awaiting"
	stateRejected  = "rejected"
)

type discoveryState struct {
	schedule  map[string]schedule.Record
	published map[string]PublishedRecord
	awaiting  map[string]AwaitRecord
	// rejected holds the code last reported for a rejected pull request, so
	// each rejection is announced once.
	rejected map[string]string
}

func (engine *Engine) withDiscovery(ctx context.Context, fn func() error) error {
	return engine.store.withDiscoveryLock(ctx, engine.cfg.DiscoveryLockTimeout, engine.cfg.DiscoveryLockPoll, fn)
}

func (engine *Engine) notifyNow(ctx context.Context, level policy.Level, message string) error {
	_, err := retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, func(ctx context.Context) (bool, error) {
		return true, engine.integration.Notify(ctx, level, message)
	})
	return err
}

// reportRejected announces a pull request whose data the engine refused once
// per cause; the user has to look at it, because it is not reviewed.
func (engine *Engine) reportRejected(ctx context.Context, state discoveryState, rejected []RejectedCandidate) error {
	current := map[string]string{}
	for _, item := range rejected {
		current[item.Key()] = item.Code
		if state.rejected[item.Key()] == item.Code {
			continue
		}
		message := fmt.Sprintf("Review for %s is not started: GitHub returned data the review service refuses (%s). Check the pull request; it is reviewed once the data is valid.", item.Key(), item.Code)
		if err := engine.notifyNow(ctx, engine.cfg.Policy.Level(policy.EventReviewStopped), message); err != nil {
			return err
		}
	}
	for key := range state.rejected {
		delete(state.rejected, key)
	}
	for key, code := range current {
		state.rejected[key] = code
	}
	return nil
}

// reviewScope returns an incremental scope only when the last published
// decision leaves nothing open that a review without a human could approve:
// an approval, a rule that never approves or waits for the user's approval,
// or a pull request the reviewing account authored. A rule that approves on
// its own gets a full review after a request for changes, so its approval
// always rests on a complete or approved baseline.
func reviewScope(rule policy.Rule, published PublishedRecord, found bool, head string, ownPullRequest bool) ReviewScope {
	if !found || !shaPattern.MatchString(published.HeadSHA) || published.HeadSHA == head {
		return ReviewScope{Mode: scopeFull}
	}
	if published.Decision == DecisionApprove || !rule.ApprovesClean() || rule.OnClean == policy.CleanAwait || ownPullRequest {
		return ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: published.HeadSHA}
	}
	return ReviewScope{Mode: scopeFull}
}

// planReview decides the scope and the prior findings of a review. The prior
// findings are the blocking findings of the last published review and those
// it left open. An incremental scope also needs that review's stored result
// and a head that extends the reviewed one linearly; otherwise the review is
// full, and still settles the prior findings it can find.
func (engine *Engine) planReview(ctx context.Context, candidate Candidate, published PublishedRecord, found bool) (ReviewScope, []PriorFinding, error) {
	pr := candidate.PullRequest
	scope := reviewScope(candidate.Rule, published, found, pr.HeadSHA, pr.Author == engine.cfg.GitHubLogin)
	if !found || published.HeadSHA == pr.HeadSHA || candidate.Rule.Pipeline.Protocol != policy.ProtocolClaudeStructured {
		return scope, nil, nil
	}
	prior, stored, err := engine.priorFindings(ctx, published)
	if err != nil {
		return ReviewScope{}, nil, err
	}
	if !scope.Incremental() {
		return scope, prior, nil
	}
	if !stored {
		engine.logger.InfoContext(ctx, "review widened to full", "review_id", pr.ReviewID(), "reason", "previous_review_not_stored")
		return ReviewScope{Mode: scopeFull}, prior, nil
	}
	linear, err := engine.integration.LinearHistory(ctx, pr.Repository, published.HeadSHA, pr.HeadSHA)
	if err != nil && ctx.Err() != nil {
		return ReviewScope{}, nil, err
	}
	if err != nil {
		// Without the comparison the diff since the reviewed head is unknown.
		engine.logger.WarnContext(ctx, "review widened to full", "review_id", pr.ReviewID(), "reason", "history_unknown", "error_code", errorCode(err))
		return ReviewScope{Mode: scopeFull}, prior, nil
	}
	if !linear {
		engine.logger.InfoContext(ctx, "review widened to full", "review_id", pr.ReviewID(), "reason", "history_not_linear")
		return ReviewScope{Mode: scopeFull}, prior, nil
	}
	return scope, prior, nil
}

// priorFindings reads the blocking findings of a published review and the
// prior findings it left open. stored is false when that review's result is
// gone, such as after retention or for a review this engine did not make.
func (engine *Engine) priorFindings(ctx context.Context, published PublishedRecord) (prior []PriorFinding, stored bool, err error) {
	if !strings.HasPrefix(published.WorkflowID, workflowIDPrefix) {
		return nil, false, nil
	}
	review, found, err := engine.store.getReview(ctx, published.WorkflowID)
	if err != nil || !found {
		return nil, false, err
	}
	var artifact Artifact
	var publication PublishResult
	if found, err := engine.store.loadStep(ctx, published.WorkflowID, stepArtifact, &artifact); err != nil || !found {
		return nil, false, err
	}
	if found, err := engine.store.loadStep(ctx, published.WorkflowID, stepPublication, &publication); err != nil || !found {
		return nil, false, err
	}
	seen := map[string]bool{}
	add := func(finding PriorFinding) {
		if !seen[finding.Key] && len(prior) < engine.cfg.MaxFindings {
			seen[finding.Key] = true
			prior = append(prior, finding)
		}
	}
	for _, open := range artifact.openPrior() {
		add(open.Finding)
	}
	blocking, _ := review.Input.splitFindings(artifact.Findings)
	for _, finding := range blocking {
		add(PriorFinding{
			Key: priorKey(artifact.HeadSHA, finding.ID), ID: finding.ID, HeadSHA: artifact.HeadSHA, GitHubReviewID: publication.GitHubReviewID,
			Severity: finding.Severity, Path: finding.Path, Line: finding.Line, Title: finding.Title, Body: finding.Body,
		})
	}
	return prior, true, nil
}

// notesHandoff returns the published review that handed non-blocking findings
// to the author, or nil. At an unchanged head the review adopts its result
// while that result is stored; otherwise the reviewers run and the clean
// outcome waits for the user.
func (engine *Engine) notesHandoff(ctx context.Context, published PublishedRecord, found bool, pr PullRequest) (*NotesHandoff, error) {
	if !found || !published.AwaitsAuthor {
		return nil, nil
	}
	handoff := &NotesHandoff{WorkflowID: published.WorkflowID, HeadSHA: published.HeadSHA}
	if published.HeadSHA != pr.HeadSHA {
		return handoff, nil
	}
	var artifact Artifact
	stored, err := engine.store.loadStep(ctx, published.WorkflowID, stepArtifact, &artifact)
	if err != nil {
		return nil, err
	}
	if !stored {
		engine.logger.InfoContext(ctx, "notes hand-off reviewed again", "review_id", pr.ReviewID(), "reason", "handoff_review_not_stored")
	}
	handoff.Adopt = stored
	return handoff, nil
}

func (engine *Engine) buildInput(candidate Candidate, route ProjectRoute, scope ReviewScope, prior []PriorFinding, handoff *NotesHandoff, attempt int) ReviewInput {
	reviewers := candidate.Rule.Pipeline.Reviewers
	if scope.Incremental() {
		reviewers = candidate.Rule.Pipeline.IncrementalReviewers
	}
	return ReviewInput{
		Version: inputVersion, PullRequest: candidate.PullRequest, Attempt: attempt, Rule: candidate.Rule,
		Reviewers: append([]policy.Reviewer(nil), reviewers...), Scope: scope, Route: route,
		Notifications: engine.cfg.Policy.Notifications(), ApprovalTimeoutSeconds: int64(engine.cfg.Policy.ApprovalTimeout().Seconds()),
		PolicyDigest: engine.cfg.Policy.Digest(), PublisherLogin: engine.cfg.GitHubLogin,
		Limits: runLimits(engine.cfg), Prior: prior, Handoff: handoff,
	}
}

func (engine *Engine) notifyLevel(record schedule.Record) policy.Level {
	if record.Phase == "retry_wait" {
		return engine.cfg.Policy.Level(policy.EventRetryScheduled)
	}
	return engine.cfg.Policy.Level(policy.EventReviewStopped)
}

// Poll plans every candidate under the discovery lock and queues the reviews
// that are due.
func (engine *Engine) Poll(ctx context.Context) (PollResult, error) {
	var result PollResult
	err := engine.withDiscovery(ctx, func() error {
		var err error
		result, err = engine.poll(ctx, nil)
		return err
	})
	return result, err
}

type dispatch struct {
	workflowID string
	input      ReviewInput
}

// poll runs one discovery pass. force asks for a new attempt of one pull
// request head that failed. The caller holds the discovery lock.
func (engine *Engine) poll(ctx context.Context, force *RetryRequest) (PollResult, error) {
	// A failure here leaves the drafts for the next pass and holds up no
	// other review.
	readied, err := engine.integration.ReadyDrafts(ctx)
	for _, draft := range readied {
		engine.logger.InfoContext(ctx, "draft marked ready for review", "repository", draft.Repository, "pull_request", draft.Number, "author", draft.Author)
	}
	if err != nil && ctx.Err() == nil {
		engine.logger.WarnContext(ctx, "marking drafts ready for review failed", "error_code", errorCode(err))
	}
	listed, err := retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, engine.integration.ListCandidates)
	if err != nil {
		return PollResult{}, err
	}
	candidates := listed.Candidates
	history, err := engine.store.runHistory(ctx, engine.cfg.StateEntryLimit)
	if err != nil {
		return PollResult{}, err
	}
	now := engine.now().UTC()
	state, err := engine.store.loadDiscovery(ctx)
	if err != nil {
		return PollResult{}, err
	}
	present := map[string]Candidate{}
	for _, candidate := range candidates {
		present[candidate.PullRequest.Key()] = candidate
	}

	if err := engine.reportRejected(ctx, state, listed.Rejected); err != nil {
		return PollResult{}, err
	}
	result := PollResult{Open: len(candidates)}
	supersede, err := engine.supersedeAwaiting(ctx, state, present, history)
	if err != nil {
		return PollResult{}, err
	}
	result.Superseded = len(supersede)

	if err := engine.remindHandoffs(ctx, state, present, now); err != nil {
		return PollResult{}, err
	}

	if force != nil {
		if err := engine.applyForce(state, present, history, *force, now); err != nil {
			return PollResult{}, err
		}
	}
	keys := make([]string, 0, len(present))
	for key := range present {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	dispatches := []dispatch{}
	for _, key := range keys {
		candidate := present[key]
		pr := candidate.PullRequest
		if _, exists := state.schedule[key]; !exists && len(state.schedule) >= engine.cfg.StateEntryLimit {
			return PollResult{}, workflowError("state_entry_limit_exceeded", true, nil)
		}
		if published, ok := state.published[key]; ok && published.covers(pr) {
			state.schedule[key] = schedule.Record{Identity: pr.Target().Identity(), Phase: "completed", WorkflowID: published.WorkflowID}
			continue
		}
		// The head already has a review that waits for the user's approval.
		if waiting, ok := state.awaiting[key]; ok && waiting.matches(pr) {
			continue
		}
		decision := schedule.Plan(state.schedule[key], pr.Target(), history, now, engine.cfg.Schedule)
		if decision.Notify {
			if err := engine.notifyNow(ctx, engine.notifyLevel(decision.Record), schedule.Notification(pr.Target(), decision.Record)); err != nil {
				return PollResult{}, err
			}
		}
		if decision.Dispatch {
			// A route failure is a scheduler failure with its own retry policy, so
			// it is not retried here.
			route, routeErr := engine.integration.ResolveProject(ctx, pr.Repository, candidate.Rule.Pipeline.ProjectSources)
			if routeErr != nil && ctx.Err() != nil {
				return PollResult{}, routeErr
			}
			if routeErr != nil {
				decision.Record = schedule.AfterFailure(decision.Record, errorCode(routeErr), now, engine.cfg.Schedule)
				if err := engine.notifyNow(ctx, engine.notifyLevel(decision.Record), schedule.Notification(pr.Target(), decision.Record)); err != nil {
					return PollResult{}, err
				}
			} else {
				published, found := state.published[key]
				scope, prior, planErr := engine.planReview(ctx, candidate, published, found)
				if planErr != nil {
					return PollResult{}, planErr
				}
				handoff, handoffErr := engine.notesHandoff(ctx, published, found, candidate.PullRequest)
				if handoffErr != nil {
					return PollResult{}, handoffErr
				}
				input := engine.buildInput(candidate, route, scope, prior, handoff, decision.Record.Attempt)
				decision.Record.WorkflowID = pr.WorkflowID(decision.Record.Attempt)
				dispatches = append(dispatches, dispatch{workflowID: decision.Record.WorkflowID, input: input})
				result.Dispatched++
				engine.logger.InfoContext(ctx, "review scheduled", "review_id", pr.ReviewID(), "rule", candidate.Rule.Match, "pipeline", candidate.Rule.Pipeline.Name, "scope", scope.Mode, "prior_findings", len(prior), "attempt", decision.Record.Attempt+1)
			}
		}
		state.schedule[key] = decision.Record
	}
	engine.pruneDiscovery(state, present, now)
	err = engine.store.inTx(ctx, func(tx *sql.Tx) error {
		for _, item := range dispatches {
			if err := engine.store.enqueueReview(ctx, tx, item.workflowID, item.input); err != nil {
				return err
			}
		}
		for _, workflowID := range supersede {
			if _, err := engine.store.decideApprovalTx(ctx, tx, workflowID, approvalKindSuperseded); err != nil {
				return err
			}
		}
		return saveDiscovery(ctx, tx, state)
	})
	if err != nil {
		return PollResult{}, err
	}
	return result, nil
}

// supersedeAwaiting returns the waits that no longer match their pull request
// and drops records of waits that already ended. A pull request missing from
// search is read directly first, because search lags behind.
func (engine *Engine) supersedeAwaiting(ctx context.Context, state discoveryState, present map[string]Candidate, own []schedule.Run) ([]string, error) {
	finished := map[string]bool{}
	for _, run := range own {
		if run.Status == "completed" {
			finished[run.WorkflowID] = true
		}
	}
	superseded := []string{}
	keys := make([]string, 0, len(state.awaiting))
	for key := range state.awaiting {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		record := state.awaiting[key]
		if finished[record.WorkflowID] {
			delete(state.awaiting, key)
			continue
		}
		if candidate, open := present[key]; open {
			if record.matches(candidate.PullRequest) {
				continue
			}
		} else {
			pr, err := retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, func(ctx context.Context) (PullRequest, error) {
				return engine.integration.GetPullRequest(ctx, record.Repository, record.Number, record.Trigger)
			})
			if err != nil && !skippableCandidateError(err) {
				return nil, err
			}
			if err == nil && record.matches(pr) {
				continue
			}
		}
		superseded = append(superseded, record.WorkflowID)
		delete(state.awaiting, key)
	}
	return superseded, nil
}

// remindHandoffs reminds the user once of notes whose author did not request
// the review again within the policy's reminder time. The hand-off of a pull
// request that is no longer open ends. A failed read is retried on the next
// pass, so it holds up no review.
func (engine *Engine) remindHandoffs(ctx context.Context, state discoveryState, present map[string]Candidate, now time.Time) error {
	after := engine.cfg.Policy.HandoffReminder()
	if after <= 0 {
		return nil
	}
	keys := make([]string, 0, len(state.published))
	for key, record := range state.published {
		if _, requested := present[key]; record.AwaitsAuthor && !record.Reminded && !requested && now.Sub(record.PublishedAt) >= after {
			keys = append(keys, key)
		}
	}
	sort.Strings(keys)
	for _, key := range keys {
		record := state.published[key]
		current, err := retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, func(ctx context.Context) (PullRequestState, error) {
			return engine.integration.ReadPullRequestState(ctx, record.Repository, record.Number)
		})
		if err != nil {
			if ctx.Err() != nil {
				return err
			}
			engine.logger.WarnContext(ctx, "notes hand-off unreadable", "repository", record.Repository, "pull_request", record.Number, "error_code", errorCode(err))
			continue
		}
		switch {
		case !current.Open:
			record.AwaitsAuthor = false
		case current.Requested:
			// Search lags behind; the next pass reviews it.
			continue
		default:
			if err := engine.notifyNow(ctx, engine.cfg.Policy.Level(policy.EventHandoffReminder), handoffReminderMessage(engine.cfg, record, current, now)); err != nil {
				return err
			}
			record.Reminded = true
		}
		state.published[key] = record
	}
	return nil
}

// applyForce turns the failed latest attempt of a pull request head into a
// due retry.
func (engine *Engine) applyForce(state discoveryState, present map[string]Candidate, own []schedule.Run, force RetryRequest, now time.Time) error {
	key := fmt.Sprintf("%s#%d", force.Repository, force.Number)
	candidate, open := present[key]
	if !open {
		return workflowError("review_request_not_active", true, nil)
	}
	target := candidate.PullRequest.Target()
	if !strings.HasPrefix(target.HeadSHA, force.HeadSHA) {
		return workflowError("head_changed", true, nil)
	}
	// The run history decides, not the schedule record: the record may still
	// say running from a poll that saw the run before it ended.
	forced := schedule.Record{Identity: target.Identity(), Phase: "retry_wait", DueAt: now}
	// The forced record names the latest attempt of this head, so the planner
	// treats its failure as handled. The schedule record keeps the attempt
	// counter when the review history no longer holds that attempt, so a retry
	// never reuses an earlier workflow ID.
	if previous, ok := state.schedule[key]; ok && previous.Identity == forced.Identity {
		forced.Attempt, forced.WorkflowID = previous.Attempt+1, previous.WorkflowID
	}
	latestAttempt := -1
	for _, run := range own {
		if run.Target.Key() != key {
			continue
		}
		if run.Active() {
			return workflowError("review_already_running", true, nil)
		}
		if run.Target.Identity() != forced.Identity {
			continue
		}
		if run.Result == resultSuccess || run.Released {
			return workflowError("review_already_completed", true, nil)
		}
		if run.Attempt > latestAttempt {
			latestAttempt = run.Attempt
			forced.WorkflowID = run.WorkflowID
			forced.Attempt = max(forced.Attempt, run.Attempt+1)
		}
	}
	state.schedule[key] = forced
	return nil
}

// pruneDiscovery keeps the discovery state bounded: schedule records of pull
// requests that left every trigger and are not running, and published
// decisions older than the retention.
func (engine *Engine) pruneDiscovery(state discoveryState, present map[string]Candidate, now time.Time) {
	for key, record := range state.schedule {
		if _, open := present[key]; open {
			continue
		}
		switch record.Phase {
		case "waiting", "retry_wait":
			record.Phase = "withdrawn"
			state.schedule[key] = record
		case "withdrawn", "completed", "failed":
			if len(record.BlockingWorkflows) == 0 {
				delete(state.schedule, key)
			}
		}
	}
	for key, record := range state.published {
		if now.Sub(record.PublishedAt) > engine.cfg.PublishedRetention {
			delete(state.published, key)
		}
	}
}

// Retry starts a new attempt of a stopped review. HeadSHA may be the head
// prefix a stop notification shows; it must match the current head.
func (engine *Engine) Retry(ctx context.Context, request RetryRequest) (RetryResult, error) {
	if !repositoryPattern.MatchString(request.Repository) || request.Number <= 0 || !headPrefixValid(request.HeadSHA) {
		return RetryResult{}, workflowError("review_identity_invalid", true, nil)
	}
	var record schedule.Record
	err := engine.withDiscovery(ctx, func() error {
		if _, err := engine.poll(ctx, &request); err != nil {
			return err
		}
		state, err := engine.store.loadDiscovery(ctx)
		if err != nil {
			return err
		}
		record = state.schedule[fmt.Sprintf("%s#%d", request.Repository, request.Number)]
		return nil
	})
	if err != nil {
		return RetryResult{}, err
	}
	if record.Phase != "running" {
		return RetryResult{}, workflowError("review_retry_not_dispatched", true, nil)
	}
	// The identity is repository#number@head@request; the head is its second field.
	parts := strings.SplitN(record.Identity, "@", 3)
	if len(parts) != 3 || !shaPattern.MatchString(parts[1]) {
		return RetryResult{}, workflowError("review_retry_not_dispatched", true, nil)
	}
	return RetryResult{OK: true, WorkflowID: record.WorkflowID, HeadSHA: parts[1], Attempt: record.Attempt}, nil
}

func (engine *Engine) markAwaiting(ctx context.Context, record AwaitRecord) error {
	if !reviewIdentityValid(record.Repository, record.Number, record.HeadSHA) || !identifierPattern.MatchString(record.WorkflowID) {
		return workflowError("review_identity_invalid", true, nil)
	}
	return engine.withDiscovery(ctx, func() error {
		return engine.store.updateDiscovery(ctx, func(state discoveryState, _ *sql.Tx) error {
			state.awaiting[record.Key()] = record
			return nil
		})
	})
}

func (engine *Engine) clearAwaiting(ctx context.Context, record AwaitRecord) error {
	return engine.withDiscovery(ctx, func() error {
		return engine.store.updateDiscovery(ctx, func(state discoveryState, _ *sql.Tx) error {
			if existing, ok := state.awaiting[record.Key()]; ok && existing.WorkflowID == record.WorkflowID {
				delete(state.awaiting, record.Key())
			}
			return nil
		})
	})
}

func (engine *Engine) markPublished(ctx context.Context, record PublishedRecord) error {
	if !reviewIdentityValid(record.Repository, record.Number, record.HeadSHA) || !identifierPattern.MatchString(record.WorkflowID) {
		return workflowError("review_identity_invalid", true, nil)
	}
	return engine.withDiscovery(ctx, func() error {
		return engine.store.updateDiscovery(ctx, func(state discoveryState, _ *sql.Tx) error {
			key := record.Key()
			state.published[key] = record
			if existing, ok := state.awaiting[key]; ok && existing.WorkflowID == record.WorkflowID {
				delete(state.awaiting, key)
			}
			if scheduled, ok := state.schedule[key]; ok {
				state.schedule[key] = schedule.ReconcileCompleted(scheduled, map[string]bool{record.WorkflowID: true})
			}
			return nil
		})
	})
}

// Resolve finds the review that waits for approval at the given head prefix.
// It only reads, so it never waits for a running poll.
func (engine *Engine) Resolve(ctx context.Context, request ResolveRequest) (ResolveResult, error) {
	if !repositoryPattern.MatchString(request.Repository) || request.Number <= 0 || !headPrefixValid(request.HeadPrefix) {
		return ResolveResult{}, workflowError("review_identity_invalid", true, nil)
	}
	state, err := engine.store.loadDiscovery(ctx)
	if err != nil {
		return ResolveResult{}, err
	}
	key := fmt.Sprintf("%s#%d", request.Repository, request.Number)
	if record, ok := state.awaiting[key]; ok && strings.HasPrefix(record.HeadSHA, request.HeadPrefix) {
		return ResolveResult{OK: true, WorkflowID: record.WorkflowID, HeadSHA: record.HeadSHA}, nil
	}
	// A repeated approval after publication resolves to the published review,
	// whose approval reports the publication.
	if record, ok := state.published[key]; ok && record.Decision == DecisionApprove && strings.HasPrefix(record.HeadSHA, request.HeadPrefix) && strings.HasPrefix(record.WorkflowID, workflowIDPrefix) {
		if _, found, err := engine.store.getReview(ctx, record.WorkflowID); err != nil {
			return ResolveResult{}, err
		} else if found {
			return ResolveResult{OK: true, WorkflowID: record.WorkflowID, HeadSHA: record.HeadSHA}, nil
		}
	}
	return ResolveResult{}, workflowError("review_not_awaiting_approval", true, nil)
}

// ListAwaiting returns the reviews that wait for an approval.
func (engine *Engine) ListAwaiting(ctx context.Context) ([]AwaitRecord, error) {
	state, err := engine.store.loadDiscovery(ctx)
	if err != nil {
		return nil, err
	}
	records := make([]AwaitRecord, 0, len(state.awaiting))
	for _, record := range state.awaiting {
		records = append(records, record)
	}
	sort.Slice(records, func(i, j int) bool { return records[i].Key() < records[j].Key() })
	return records, nil
}

// PullRequestReviewStatus is everything the engine knows about one pull request.
type PullRequestReviewStatus struct {
	Key       string           `json:"key"`
	Schedule  *schedule.Record `json:"schedule,omitempty"`
	Published *PublishedRecord `json:"published,omitempty"`
	Awaiting  *AwaitRecord     `json:"awaiting,omitempty"`
	Reviews   []StoredReview   `json:"reviews"`
}

func (engine *Engine) PullRequestStatus(ctx context.Context, repository string, number int) (PullRequestReviewStatus, error) {
	key := fmt.Sprintf("%s#%d", repository, number)
	state, err := engine.store.loadDiscovery(ctx)
	if err != nil {
		return PullRequestReviewStatus{}, err
	}
	status := PullRequestReviewStatus{Key: key}
	if record, ok := state.schedule[key]; ok {
		status.Schedule = &record
	}
	if record, ok := state.published[key]; ok {
		status.Published = &record
	}
	if record, ok := state.awaiting[key]; ok {
		status.Awaiting = &record
	}
	if status.Reviews, err = engine.store.listReviews(ctx, "review_key = ?", engine.cfg.ReviewListLimit, key); err != nil {
		return PullRequestReviewStatus{}, err
	}
	return status, nil
}
