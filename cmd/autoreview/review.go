package main

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

// Review step and state names are persisted; renaming one makes a resumed
// review repeat that step.
const (
	stepArtifact     = "artifact"
	stepDiscussion   = "discussion"
	stepCommentable  = "commentable"
	stepPlan         = "plan"
	stepRendered     = "rendered"
	stepPublication  = "publication"
	stepPublishedAt  = "published_at"
	stepVerification = "verification"
	stepHumanReview  = "human_review"
	stateDeadline    = "collect_deadline"
	// The verifier runs after the reviewers, so it gets a deadline of its own.
	stateVerifierDeadline = "verify_deadline"
	slotStatePrefix       = "slot/"
	notifyStepPrefix      = "notify/"
)

// errShutdown marks a review interrupted by an engine shutdown. The review
// keeps its status and resumes from its stored steps on the next start.
var errShutdown = errors.New("review interrupted by shutdown")

func validateInput(input ReviewInput) error {
	pr := input.PullRequest
	if input.Version < minInputVersion || input.Version > inputVersion || !reviewIdentityValid(pr.Repository, pr.Number, pr.HeadSHA) || input.Attempt < 0 || pr.ReviewRequestID < 0 {
		return errors.New("review identity is invalid")
	}
	if rule, matched := input.Rule, false; rule.Match == "" || len(input.Reviewers) == 0 {
		return errors.New("review rule is missing")
	} else {
		for _, reviewer := range input.Reviewers {
			matched = false
			for _, member := range rule.Pipeline.Reviewers {
				matched = matched || member == reviewer
			}
			if !matched || reviewer.Protocol != rule.Pipeline.Protocol {
				return errors.New("reviewer is not part of the pipeline")
			}
		}
	}
	if err := validatePrior(input); err != nil {
		return err
	}
	if handoff := input.Handoff; handoff != nil && (!identifierPattern.MatchString(handoff.WorkflowID) || !shaPattern.MatchString(handoff.HeadSHA) || handoff.Adopt && handoff.HeadSHA != pr.HeadSHA) {
		return errors.New("notes hand-off is invalid")
	}
	switch input.Scope.Mode {
	case scopeFull:
		if input.Scope.PreviousHeadSHA != "" {
			return errors.New("full review names a previous head")
		}
	case scopeIncremental:
		if !shaPattern.MatchString(input.Scope.PreviousHeadSHA) || input.Scope.PreviousHeadSHA == pr.HeadSHA {
			return errors.New("incremental review needs a different previous head")
		}
	default:
		return errors.New("review scope is invalid")
	}
	if !identifierPattern.MatchString(input.Route.Host) || !identifierPattern.MatchString(input.Route.ProjectID) ||
		!githubLoginPattern.MatchString(input.PublisherLogin) || input.ApprovalTimeoutSeconds <= 0 || !input.Limits.valid() {
		return errors.New("review settings are invalid")
	}
	for _, event := range policy.WorkflowEvents {
		switch input.Notifications[event] {
		case policy.LevelAction, policy.LevelDigest, policy.LevelLog:
		default:
			return fmt.Errorf("notification level of %s is missing", event)
		}
	}
	return nil
}

// validatePrior accepts prior findings only from the input version that
// settles them and only for reviewers that can return verdicts.
func validatePrior(input ReviewInput) error {
	if len(input.Prior) == 0 {
		return nil
	}
	if input.Version < priorFindingsVersion || input.Rule.Pipeline.Protocol != policy.ProtocolClaudeStructured {
		return errors.New("prior findings are not supported by this input")
	}
	seen := map[string]bool{}
	for _, finding := range input.Prior {
		if seen[finding.Key] || finding.Key != priorKey(finding.HeadSHA, finding.ID) || !shaPattern.MatchString(finding.HeadSHA) || finding.GitHubReviewID < 0 {
			return errors.New("prior finding is invalid")
		}
		seen[finding.Key] = true
	}
	return nil
}

// stillWanted reports an error when the reviewed head is no longer the pull
// request head. A trigger that no longer asks for the review fails the read of
// the pull request itself; a renewed review request of the same head keeps the
// running review.
func stillWanted(expected, current PullRequest) error {
	if current.HeadSHA != expected.HeadSHA {
		return workflowError("head_changed", true, nil)
	}
	return nil
}

// slotState is the persisted progress of one reviewer or of the verifier.
type slotState struct {
	Run int `json:"run"`
	// Start is the session start attempt of the current run, so a restart
	// finds a session started before it by that attempt's name.
	Start        int              `json:"start,omitempty"`
	Session      *SessionIdentity `json:"session,omitempty"`
	Result       *ReviewerResult  `json:"result,omitempty"`
	Verification *verifierResult  `json:"verification,omitempty"`
}

// sessionFault is a failure of a reviewer session itself: it did not start,
// could not be read, or never gave usable output in time. A review can go on
// without an optional session; any other error stops it.
type sessionFault struct{ err error }

func (fault *sessionFault) Error() string { return fault.err.Error() }
func (fault *sessionFault) Unwrap() error { return fault.err }

type reviewerSlot struct {
	reviewer policy.Reviewer
	request  SessionRequest
	state    slotState
}

type artifactOutcome struct {
	Artifact  Artifact `json:"artifact"`
	ErrorCode string   `json:"error_code,omitempty"`
}

type renderOutcome struct {
	Review    GitHubReview `json:"review"`
	ErrorCode string       `json:"error_code,omitempty"`
}

type review struct {
	engine     *Engine
	ctx        context.Context
	workflowID string
	input      ReviewInput
	slots      []*reviewerSlot
	// verifier is the verifier's slot of a review whose pipeline has one.
	verifier *reviewerSlot
	// discussion is what the reviewers of a review with prior findings read.
	discussion Discussion
	// approved is set once the user approved a waiting review.
	approved bool
}

func (r *review) limits() RunLimits { return r.input.Limits }

func (r *review) retryPolicy() stepRetryPolicy {
	limits := r.limits()
	return stepRetryPolicy{For: milliseconds(limits.StepRetryMS), Initial: milliseconds(limits.StepRetryInitialMS), Max: milliseconds(limits.StepRetryMaxIntervalMS)}
}

// interrupted turns an error caused by the engine shutting down into
// errShutdown, so it is never recorded as a review failure.
func (r *review) interrupted(err error) error {
	if err != nil && r.ctx.Err() != nil {
		return errShutdown
	}
	return err
}

// once runs fn a single time per review; a resumed review reads the stored
// output instead of repeating the side effect.
func once[T any](r *review, name string, fn func(context.Context) (T, error)) (T, error) {
	var value T
	found, err := r.engine.store.loadStep(r.ctx, r.workflowID, name, &value)
	if err != nil || found {
		return value, err
	}
	value, err = retryStep(r.ctx, r.retryPolicy(), r.engine.now, r.engine.sleep, fn)
	if err != nil {
		return value, r.interrupted(err)
	}
	return value, r.engine.store.saveStep(r.ctx, r.workflowID, name, value)
}

// fresh runs fn with step retries and never stores its output, for reads whose
// current answer matters on every resume.
func fresh[T any](r *review, fn func(context.Context) (T, error)) (T, error) {
	value, err := retryStep(r.ctx, r.retryPolicy(), r.engine.now, r.engine.sleep, fn)
	return value, r.interrupted(err)
}

func (r *review) sleep(delay time.Duration) error {
	if err := r.engine.sleep(r.ctx, delay); err != nil {
		return errShutdown
	}
	return nil
}

func (r *review) notify(name string, event policy.Event, message string) error {
	_, err := once(r, notifyStepPrefix+name, func(ctx context.Context) (bool, error) {
		return true, r.engine.integration.Notify(ctx, r.input.level(event), message)
	})
	return err
}

func (r *review) saveSlot(slot *reviewerSlot) error {
	return r.engine.store.setState(r.ctx, r.workflowID, slotStatePrefix+slot.reviewer.ID, slot.state)
}

func (r *review) setCleanupPending(pending bool) error {
	return r.engine.store.setReviewFlags(r.ctx, r.workflowID, nil, &pending)
}

func (r *review) setReleased(released bool) error {
	return r.engine.store.setReviewFlags(r.ctx, r.workflowID, &released, nil)
}

// allSlots returns the reviewer slots and the verifier slot.
func (r *review) allSlots() []*reviewerSlot {
	if r.verifier == nil {
		return r.slots
	}
	return append(append([]*reviewerSlot(nil), r.slots...), r.verifier)
}

func (r *review) sessionsOpen() bool {
	for _, slot := range r.allSlots() {
		if slot.state.Session != nil {
			return true
		}
	}
	return false
}

// removeSessions removes the given slots' sessions with bounded attempts. A
// session the host keeps is reported for manual removal and the review goes
// on: its output is already stored, so the session holds nothing.
func (r *review) removeSessions(slots []*reviewerSlot) error {
	failures := []sessionCleanupFailure{}
	limits := r.limits()
	names := []string{}
	for _, slot := range slots {
		if slot.state.Session == nil {
			continue
		}
		session := *slot.state.Session
		names = append(names, fmt.Sprintf("%s-%d", slot.reviewer.ID, slot.state.Run))
		code := ""
		for attempt := 1; attempt <= limits.SessionCleanupAttempts; attempt++ {
			removeErr := r.engine.integration.RemoveSessionConfirmed(r.ctx, session)
			if removeErr = r.interrupted(removeErr); errors.Is(removeErr, errShutdown) {
				return removeErr
			}
			if removeErr == nil {
				code = ""
				break
			}
			code = errorCode(removeErr)
			if pohunek := pohunekErrorCode(removeErr); pohunek != "" {
				code += "/" + pohunek
			}
			r.engine.logger.WarnContext(r.ctx, "reviewer session cleanup failed", "review_id", r.input.PullRequest.ReviewID(), "reviewer", session.ReviewerID, "session_id", session.SessionID, "host", session.Host, "attempt", attempt, "error_code", code)
			if attempt < limits.SessionCleanupAttempts {
				if err := r.sleep(milliseconds(limits.SessionCleanupRetryMS)); err != nil {
					return err
				}
			}
		}
		if code != "" {
			failures = append(failures, sessionCleanupFailure{Session: session, Code: code})
		}
		slot.state.Session = nil
		if err := r.saveSlot(slot); err != nil {
			return err
		}
	}
	if !r.sessionsOpen() {
		if err := r.setCleanupPending(false); err != nil {
			return err
		}
	}
	if len(failures) == 0 {
		return nil
	}
	sort.Strings(names)
	return r.notify("cleanup/"+strings.Join(names, ","), policy.EventCleanupManual, sessionCleanupFailedMessage(r.input.PullRequest, failures, limits.SessionCleanupAttempts))
}

// startSession retries transient start failures inside the review, so a slow
// review host does not cost a whole scheduler attempt. EnsureSession finds a
// session started before a restart by its name.
func (r *review) startSession(slot *reviewerSlot) error {
	limits := r.limits()
	request := slot.request
	request.Run = slot.state.Run
	for attempt := max(slot.state.Start, 1); attempt <= limits.SessionStartAttempts; attempt++ {
		if slot.state.Start != attempt {
			slot.state.Start = attempt
			if err := r.saveSlot(slot); err != nil {
				return err
			}
		}
		request.Start = attempt
		session, startErr := r.engine.integration.EnsureSession(r.ctx, request)
		if startErr = r.interrupted(startErr); errors.Is(startErr, errShutdown) {
			return startErr
		}
		if startErr == nil {
			slot.state.Session = &session
			if err := r.saveSlot(slot); err != nil {
				return err
			}
			if err := r.setCleanupPending(true); err != nil {
				return err
			}
			return r.removeOrphanedSessions(request, attempt-1)
		}
		code, permanent := "workflow_failed", true
		var workflowErr *WorkflowError
		if errors.As(startErr, &workflowErr) {
			code, permanent = workflowErr.Code, workflowErr.Permanent
			r.engine.logger.WarnContext(r.ctx, "reviewer session start failed", "review_id", r.input.PullRequest.ReviewID(), "reviewer", slot.reviewer.ID, "host", request.Route.Host, "start_attempt", attempt, "error_code", code, "diagnostic", schedule.Diagnostic(workflowErr.Cause))
		}
		if permanent || attempt == limits.SessionStartAttempts {
			if err := r.removeOrphanedSessions(request, attempt); err != nil {
				return err
			}
			return &sessionFault{workflowError(code, true, nil)}
		}
		if err := r.sleep(milliseconds(limits.SessionStartRetryMS)); err != nil {
			return err
		}
	}
	return &sessionFault{workflowError("workflow_failed", true, nil)}
}

// removeOrphanedSessions removes sessions of the first starts attempts of
// request's run that the host created after their start was given up.
func (r *review) removeOrphanedSessions(request SessionRequest, starts int) error {
	for start := 1; start <= starts; start++ {
		request.Start = start
		if err := r.removeOrphanedSession(request); err != nil {
			return err
		}
	}
	return nil
}

// removeOrphanedSession handles a host that created the session after the
// client gave up. The start failure stays the cause; a session that may still
// run is reported for manual removal.
func (r *review) removeOrphanedSession(request SessionRequest) error {
	session, err := r.engine.integration.FindSession(r.ctx, request)
	if session != nil {
		err = r.engine.integration.RemoveSessionConfirmed(r.ctx, *session)
	}
	if err = r.interrupted(err); errors.Is(err, errShutdown) || err == nil {
		return err
	}
	r.engine.logger.WarnContext(r.ctx, "orphaned reviewer session cleanup failed", "review_id", r.input.PullRequest.ReviewID(), "reviewer", request.Reviewer.ID, "error_code", errorCode(err), "diagnostic", schedule.Diagnostic(err))
	return r.notify(fmt.Sprintf("orphan/%s/%d/%d", request.Reviewer.ID, request.Run, request.Start), policy.EventCleanupManual, orphanedSessionMessage(request, errorCode(err)))
}

func (r *review) currentPullRequest() (PullRequest, error) {
	current, err := fresh(r, func(ctx context.Context) (PullRequest, error) {
		return r.engine.integration.GetPullRequest(ctx, r.input.PullRequest.Repository, r.input.PullRequest.Number, r.input.Rule.Trigger)
	})
	if err != nil {
		return PullRequest{}, err
	}
	if err := stillWanted(r.input.PullRequest, current); err != nil {
		return PullRequest{}, err
	}
	return current, nil
}

// fail ends the review. A superseded review succeeds quietly; any other cause
// is a failure that the scheduler retries or reports.
func (r *review) fail(cause error) error {
	if errors.Is(cause, errShutdown) {
		return cause
	}
	if err := r.removeSessions(r.allSlots()); err != nil {
		return err
	}
	code := errorCode(cause)
	if schedule.Superseded(code) {
		if err := r.notify("superseded", policy.EventSuperseded, supersededMessage(r.input, code)); err != nil {
			return err
		}
	} else {
		r.engine.logger.ErrorContext(r.ctx, "review failed", "review_id", r.input.PullRequest.ReviewID(), "error_code", code)
	}
	if r.approved && !schedule.Superseded(code) {
		// The user approved this review and waits for its publication; a
		// superseded one is replaced by the review of the newer head.
		if err := r.notify("approved_failed", policy.EventReviewStopped, approvedReviewFailedMessage(r.input, code)); err != nil {
			return err
		}
	}
	// A superseded review also ends as a failure: the scheduler reviews the
	// same head again if its trigger asks for it once more.
	return r.engine.store.completeReview(r.ctx, r.workflowID, resultFailure, code, nil)
}

func (r *review) deadline(key string) (time.Time, error) {
	var deadline time.Time
	found, err := r.engine.store.getState(r.ctx, r.workflowID, key, &deadline)
	if err != nil || found {
		return deadline, err
	}
	limits := r.limits()
	deadline = r.engine.now().Add(milliseconds(limits.SessionTimeoutMS) * time.Duration(limits.ReviewerRunAttempts))
	return deadline, r.engine.store.setState(r.ctx, r.workflowID, key, deadline)
}

// collect runs every reviewer to a result.
func (r *review) collect() error {
	return r.runSlots(r.slots, stateDeadline,
		func(slot *reviewerSlot) bool { return slot.state.Result != nil },
		func(slot *reviewerSlot, output []byte) error {
			result, err := ParseReviewerOutput(r.engine.cfg, r.input, r.discussion, slot.reviewer, output)
			if err != nil {
				return err
			}
			slot.state.Result = &result
			return nil
		},
		func() error { return r.notify("started", policy.EventStarted, startedMessage(r.input)) })
}

// runSlots runs the slots in parallel until each has a result: done reports a
// slot whose result is stored, parse validates a complete output into the slot
// state. A slot whose output is unusable gets a fresh session up to the run
// limit. started runs once the sessions are up.
func (r *review) runSlots(slots []*reviewerSlot, deadlineKey string, done func(*reviewerSlot) bool, parse func(*reviewerSlot, []byte) error, started func() error) error {
	limits := r.limits()
	finished := []*reviewerSlot{}
	for _, slot := range slots {
		if done(slot) && slot.state.Session != nil {
			finished = append(finished, slot)
		}
	}
	// A restart between storing a result and removing its session leaves the
	// session open.
	if err := r.removeSessions(finished); err != nil {
		return err
	}
	for _, slot := range slots {
		if done(slot) || slot.state.Session != nil {
			continue
		}
		if err := r.startSession(slot); err != nil {
			return err
		}
	}
	if started != nil {
		if err := started(); err != nil {
			return err
		}
	}
	deadline, err := r.deadline(deadlineKey)
	if err != nil {
		return err
	}
	for {
		if _, err := r.currentPullRequest(); err != nil {
			return err
		}
		pending := false
		for _, slot := range slots {
			if done(slot) {
				continue
			}
			session := *slot.state.Session
			reviewer := slot.reviewer
			output, err := fresh(r, func(ctx context.Context) ([]byte, error) {
				return r.engine.integration.SessionOutput(ctx, session, reviewer.Protocol)
			})
			if errors.Is(err, errShutdown) {
				return err
			}
			if err != nil {
				return &sessionFault{err}
			}
			if output == nil {
				pending = true
				continue
			}
			parseErr := parse(slot, output)
			switch {
			case parseErr == nil:
				if err := r.saveSlot(slot); err != nil {
					return err
				}
				if err := r.removeSessions([]*reviewerSlot{slot}); err != nil {
					return err
				}
			case reviewerOutputRetryable(parseErr) && slot.state.Run < limits.ReviewerRunAttempts:
				r.engine.logger.WarnContext(r.ctx, "retrying reviewer after unusable output", "review_id", r.input.PullRequest.ReviewID(), "reviewer", reviewer.ID, "run", slot.state.Run, "error_code", errorCode(parseErr), "error", parseErr.Error())
				if err := r.removeSessions([]*reviewerSlot{slot}); err != nil {
					return err
				}
				if err := r.sleep(milliseconds(limits.ReviewerRunRetryMS)); err != nil {
					return err
				}
				slot.state.Run++
				slot.state.Start = 0
				if err := r.saveSlot(slot); err != nil {
					return err
				}
				if err := r.startSession(slot); err != nil {
					return err
				}
				pending = true
			default:
				r.engine.logger.WarnContext(r.ctx, "rejecting reviewer output", "review_id", r.input.PullRequest.ReviewID(), "reviewer", reviewer.ID, "run", slot.state.Run, "error_code", errorCode(parseErr), "error", parseErr.Error())
				return &sessionFault{workflowError(errorCode(parseErr), true, nil)}
			}
		}
		if !pending {
			return nil
		}
		if !r.engine.now().Before(deadline) {
			return &sessionFault{workflowError("review_output_timeout", true, nil)}
		}
		if err := r.sleep(milliseconds(limits.OutputPollMS)); err != nil {
			return err
		}
	}
}

// verify runs the verifier on the claims. A verifier that fails leaves every
// claim as the reviewers made it, so the review still publishes; an error of
// the review itself, such as a newer head, stops it.
func (r *review) verify(claims verificationClaims) (Verification, error) {
	var verification Verification
	if found, err := r.engine.store.loadStep(r.ctx, r.workflowID, stepVerification, &verification); err != nil || found {
		return verification, err
	}
	prompt, err := BuildVerifierRequest(r.input, claims, r.discussion)
	if err != nil {
		return Verification{}, err
	}
	slot := r.verifier
	slot.request.Prompt = prompt
	runErr := r.runSlots([]*reviewerSlot{slot}, stateVerifierDeadline,
		func(slot *reviewerSlot) bool { return slot.state.Verification != nil },
		func(slot *reviewerSlot, output []byte) error {
			result, err := ParseVerifierOutput(r.engine.cfg, r.input, claims, output)
			if err != nil {
				return err
			}
			slot.state.Verification = &result
			return nil
		}, nil)
	verification = Verification{ReviewerLabel: slot.reviewer.Label, Findings: []FindingCheck{}, Prior: []PriorCheck{}}
	var fault *sessionFault
	switch {
	case runErr == nil:
		verification.Findings, verification.Prior = slot.state.Verification.Findings, slot.state.Verification.Prior
		counts := map[string]int{}
		for _, check := range verification.Findings {
			counts[check.Verdict]++
		}
		for _, check := range verification.Prior {
			counts[check.Verdict]++
		}
		r.engine.logger.InfoContext(r.ctx, "review verified", "review_id", r.input.PullRequest.ReviewID(), "confirmed", counts[verdictConfirmed], "not_blocking", counts[verdictNotBlocking],
			"refuted", counts[verdictRefuted], "upheld", counts[verdictUpheld], "reopened", counts[verdictReopened])
	case errors.As(runErr, &fault):
		if err := r.removeSessions([]*reviewerSlot{slot}); err != nil {
			return Verification{}, err
		}
		verification.ErrorCode = errorCode(runErr)
		r.engine.logger.WarnContext(r.ctx, "verification did not finish, publishing the claims unverified", "review_id", r.input.PullRequest.ReviewID(), "error_code", verification.ErrorCode)
	default:
		return Verification{}, runErr
	}
	return verification, r.engine.store.saveStep(r.ctx, r.workflowID, stepVerification, verification)
}

// runReview executes or resumes one review until it completes or waits for an
// approval. It returns errShutdown when the engine stops it midway.
func (engine *Engine) runReview(ctx context.Context, stored StoredReview) error {
	r := &review{engine: engine, ctx: ctx, workflowID: stored.WorkflowID, input: stored.Input, discussion: Discussion{Threads: []DiscussionThread{}, Comments: []DiscussionComment{}}}
	if err := validateInput(stored.Input); err != nil {
		return r.fail(workflowError("review_input_invalid", true, err))
	}
	if len(stored.Input.Prior) > 0 {
		// Stored once, so a resumed review hands its reviewers the same context.
		read, err := once(r, stepDiscussion, func(ctx context.Context) (Discussion, error) {
			return engine.integration.ListDiscussion(ctx, stored.Input.PullRequest, stored.Input.Prior)
		})
		if err != nil {
			return r.fail(err)
		}
		r.discussion = read
	}
	for _, reviewer := range stored.Input.Reviewers {
		prompt, err := BuildReviewerRequest(stored.Input, reviewer, r.discussion)
		if err != nil {
			return r.fail(err)
		}
		slot := &reviewerSlot{reviewer: reviewer, state: slotState{Run: 1}, request: SessionRequest{
			PullRequest: stored.Input.PullRequest, Route: stored.Input.Route, Reviewer: reviewer, Checkout: stored.Input.Rule.Pipeline.Checkout,
			WorkflowID: stored.WorkflowID, Prompt: prompt,
		}}
		if _, err := engine.store.getState(ctx, stored.WorkflowID, slotStatePrefix+reviewer.ID, &slot.state); err != nil {
			return err
		}
		r.slots = append(r.slots, slot)
	}
	if stored.Input.verifies() {
		// The prompt depends on the combined findings; verify sets it.
		reviewer := stored.Input.verifierReviewer()
		r.verifier = &reviewerSlot{reviewer: reviewer, state: slotState{Run: 1}, request: SessionRequest{
			PullRequest: stored.Input.PullRequest, Route: stored.Input.Route, Reviewer: reviewer, Checkout: stored.Input.Rule.Pipeline.Checkout, WorkflowID: stored.WorkflowID,
		}}
		if _, err := engine.store.getState(ctx, stored.WorkflowID, slotStatePrefix+reviewer.ID, &r.verifier.state); err != nil {
			return err
		}
	}

	var plan PublicationPlan
	planned, err := engine.store.loadStep(ctx, stored.WorkflowID, stepPlan, &plan)
	if err != nil {
		return err
	}
	if !planned {
		if stored.Input.adoptsHandoff() {
			return r.adoptHandoff()
		}
		return r.review()
	}
	var artifact Artifact
	if found, err := engine.store.loadStep(ctx, stored.WorkflowID, stepArtifact, &artifact); err != nil || !found {
		return r.fail(workflowError("review_incomplete", true, err))
	}
	if !plan.Await {
		current, err := r.currentPullRequest()
		if err != nil {
			return r.fail(err)
		}
		return r.publish(current, artifact, plan)
	}
	return r.continueAfterApproval(stored, artifact, plan)
}

// review runs the reviewers, combines and anchors their findings and plans the
// publication.
func (r *review) review() error {
	if _, err := r.currentPullRequest(); err != nil {
		return r.fail(err)
	}
	if err := r.collect(); err != nil {
		return r.fail(err)
	}
	results := make([]ReviewerResult, 0, len(r.slots))
	for _, slot := range r.slots {
		results = append(results, *slot.state.Result)
	}
	combined, err := once(r, stepArtifact+"/combined", func(context.Context) (artifactOutcome, error) {
		artifact, combineErr := CombineResults(r.engine.cfg, r.input, results, r.engine.now())
		if combineErr != nil {
			return artifactOutcome{ErrorCode: errorCode(combineErr)}, nil
		}
		return artifactOutcome{Artifact: artifact}, nil
	})
	if err != nil {
		return r.fail(err)
	}
	if combined.ErrorCode != "" {
		return r.fail(workflowError(combined.ErrorCode, true, nil))
	}
	artifact := combined.Artifact
	current, err := r.currentPullRequest()
	if err != nil {
		return r.fail(err)
	}
	if len(artifact.Findings) == 0 && len(artifact.PriorResults) == 0 {
		return r.plan(current, artifact)
	}
	commentable, err := once(r, stepCommentable, func(ctx context.Context) (CommentableLines, error) {
		return r.engine.integration.ListCommentableLines(ctx, current)
	})
	if err != nil {
		return r.fail(err)
	}
	// A finding outside the changed lines is a note before the verifier runs,
	// so it is never verified. A reopened prior finding is published again
	// below, so the verifier runs first.
	artifact = AnchorFindings(artifact, commentable)
	if claims := claimsOf(r.input, artifact); r.input.verifies() && !claims.empty() {
		verification, err := r.verify(claims)
		if err != nil {
			return r.fail(err)
		}
		artifact = ApplyVerification(artifact, verification)
	}
	artifact = AnchorFindings(SettlePrior(r.input, r.discussion, artifact), commentable)
	return r.plan(current, artifact)
}

// adoptHandoff plans the result of the review that handed the notes to the
// author. The head is unchanged, so running the reviewers again would only
// sample the same code anew.
func (r *review) adoptHandoff() error {
	current, err := r.currentPullRequest()
	if err != nil {
		return r.fail(err)
	}
	var artifact Artifact
	if found, err := r.engine.store.loadStep(r.ctx, r.input.Handoff.WorkflowID, stepArtifact, &artifact); err != nil || !found {
		return r.fail(workflowError("handoff_review_not_stored", true, err))
	}
	if artifact.Repository != current.Repository || artifact.Number != current.Number || artifact.HeadSHA != current.HeadSHA {
		return r.fail(workflowError("handoff_review_mismatch", true, nil))
	}
	return r.plan(current, artifact)
}

// plan stores the final artifact with its publication plan and publishes it
// or waits for the user.
func (r *review) plan(current PullRequest, artifact Artifact) error {
	signals, err := r.humanReview(current, artifact)
	if err != nil {
		return r.fail(err)
	}
	artifact.HumanReview = signals
	plan := PlanPublication(r.input, artifact, r.input.PublisherLogin)
	if err := r.engine.store.saveStep(r.ctx, r.workflowID, stepArtifact, artifact); err != nil {
		return err
	}
	if err := r.engine.store.saveStep(r.ctx, r.workflowID, stepPlan, plan); err != nil {
		return err
	}
	if plan.Await {
		return r.awaitApproval(artifact)
	}
	return r.publish(current, artifact, plan)
}

// humanReview evaluates the rule's human_review signals once per review, so a
// resumed review keeps its decision. Only a clean review that its rule would
// approve without the user needs them.
func (r *review) humanReview(current PullRequest, artifact Artifact) ([]string, error) {
	signals := r.input.Rule.HumanReview
	if signals == nil || r.input.Rule.OnClean != policy.CleanApprove || blockingCount(r.input, artifact) > 0 {
		return nil, nil
	}
	return once(r, stepHumanReview, func(ctx context.Context) ([]string, error) {
		var deleted []string
		if len(signals.DeletedTests) > 0 {
			files, err := r.engine.integration.ListDeletedFiles(ctx, current)
			if err != nil {
				return nil, err
			}
			deleted = files
		}
		return HumanReviewSignals(*signals, current.Labels, deleted), nil
	})
}

// awaitApproval releases the pull request to the scheduler and records the
// wait. The engine resumes the review once a decision is set: the user's
// approval, a newer review that supersedes it, or the deadline.
func (r *review) awaitApproval(artifact Artifact) error {
	// The wait is recorded before the pull request is released, so discovery
	// never sees the head without a review it keeps.
	if err := r.engine.markAwaiting(r.ctx, r.awaitRecord()); err != nil {
		return r.interrupted(err)
	}
	if err := r.setReleased(true); err != nil {
		return err
	}
	if err := r.notify("approval_requested", policy.EventApprovalRequested, approvalRequestedMessage(r.engine.cfg, r.input, artifact)); err != nil {
		if errors.Is(err, errShutdown) {
			return err
		}
		// Nobody would learn of the wait, so the review fails and the
		// scheduler reports it instead.
		if releaseErr := r.setReleased(false); releaseErr != nil {
			return releaseErr
		}
		if clearErr := r.engine.clearAwaiting(r.ctx, r.awaitRecord()); clearErr != nil {
			return r.interrupted(clearErr)
		}
		return r.fail(err)
	}
	deadline := r.engine.now().Add(time.Duration(r.input.ApprovalTimeoutSeconds) * time.Second)
	return r.engine.store.startAwaiting(r.ctx, r.workflowID, deadline)
}

func (r *review) continueAfterApproval(stored StoredReview, artifact Artifact, plan PublicationPlan) error {
	switch stored.Approval {
	case approvalKindApprove:
		// Holding the pull request again makes a failed publication visible
		// to the scheduler and allows an explicit retry.
		r.approved = true
		if err := r.setReleased(false); err != nil {
			return err
		}
		current, err := r.currentPullRequest()
		if err != nil {
			if errors.Is(err, errShutdown) {
				return err
			}
			if clearErr := r.engine.clearAwaiting(r.ctx, r.awaitRecord()); clearErr != nil {
				return r.interrupted(clearErr)
			}
			return r.fail(err)
		}
		return r.publish(current, artifact, plan)
	case approvalKindExpired:
		if err := r.notify("approval_expired", policy.EventApprovalExpired, approvalExpiredMessage(r.input)); err != nil {
			return err
		}
		return r.settleAwait(OutcomeExpired)
	case approvalKindSuperseded:
		if err := r.notify("superseded", policy.EventSuperseded, supersededMessage(r.input, "newer_review")); err != nil {
			return err
		}
		return r.settleAwait(OutcomeSuperseded)
	default:
		// A wait without a decision was interrupted before it was recorded.
		return r.awaitApproval(artifact)
	}
}

func (r *review) awaitRecord() AwaitRecord {
	pr := r.input.PullRequest
	return AwaitRecord{Repository: pr.Repository, Number: pr.Number, HeadSHA: pr.HeadSHA, ReviewRequestID: pr.ReviewRequestID, Trigger: r.input.Rule.Trigger, WorkflowID: r.workflowID}
}

// settleAwait removes the waiting record from discovery once the wait ended
// without a publication.
func (r *review) settleAwait(outcome Outcome) error {
	if err := r.engine.clearAwaiting(r.ctx, r.awaitRecord()); err != nil {
		return r.interrupted(err)
	}
	return r.engine.store.completeReview(r.ctx, r.workflowID, resultSuccess, "", &ReviewOutcome{Outcome: outcome})
}

func (r *review) publish(current PullRequest, artifact Artifact, plan PublicationPlan) error {
	rendered, err := once(r, stepRendered, func(context.Context) (renderOutcome, error) {
		review, renderErr := RenderReview(r.engine.cfg, r.input, artifact, plan)
		if renderErr != nil {
			return renderOutcome{ErrorCode: errorCode(renderErr)}, nil
		}
		return renderOutcome{Review: review}, nil
	})
	if err != nil {
		return r.fail(err)
	}
	if rendered.ErrorCode != "" {
		return r.fail(workflowError(rendered.ErrorCode, true, nil))
	}
	result, err := once(r, stepPublication, func(ctx context.Context) (PublishResult, error) {
		return r.engine.integration.PublishReview(ctx, current, rendered.Review)
	})
	if err != nil {
		return r.fail(err)
	}
	publishedAt, err := once(r, stepPublishedAt, func(context.Context) (time.Time, error) { return r.engine.now().UTC(), nil })
	if err != nil {
		return err
	}
	record := PublishedRecord{Repository: current.Repository, Number: current.Number, HeadSHA: current.HeadSHA, ReviewRequestID: current.ReviewRequestID, Decision: plan.Decision, WorkflowID: r.workflowID, PublishedAt: publishedAt, AwaitsAuthor: plan.NotesToAuthor}
	if err := r.engine.markPublished(r.ctx, record); err != nil {
		return r.interrupted(err)
	}
	blocking := blockingCount(r.input, artifact)
	event := PublicationEvent(plan, blocking)
	if err := r.notify("published", event, publishedMessage(r.engine.cfg, r.input, artifact, plan, result)); err != nil {
		return err
	}
	r.engine.logger.InfoContext(r.ctx, "review published", "review_id", current.ReviewID(), "decision", string(plan.Decision), "findings", len(artifact.Findings), "blocking_findings", blocking, "review_url", result.ReviewURL, "reused", result.Reused, "policy_digest", r.input.PolicyDigest)
	return r.engine.store.completeReview(r.ctx, r.workflowID, resultSuccess, "", &ReviewOutcome{Outcome: OutcomePublished, Publication: &result})
}

// Approve records the user's approval of a clean review that waits for it.
// The engine publishes the review on its next tick.
func (engine *Engine) Approve(ctx context.Context, workflowID string, request ApproveRequest) (ApprovalResult, error) {
	stored, found, err := engine.store.getReview(ctx, workflowID)
	if err != nil {
		return ApprovalResult{}, err
	}
	var artifact Artifact
	var plan PublicationPlan
	if !found {
		return ApprovalResult{}, workflowError("review_incomplete", true, nil)
	}
	hasArtifact, err := engine.store.loadStep(ctx, workflowID, stepArtifact, &artifact)
	if err != nil {
		return ApprovalResult{}, err
	}
	hasPlan, err := engine.store.loadStep(ctx, workflowID, stepPlan, &plan)
	if err != nil {
		return ApprovalResult{}, err
	}
	if !hasArtifact || !hasPlan {
		return ApprovalResult{}, workflowError("review_incomplete", true, nil)
	}
	if request.Repository != artifact.Repository || request.Number != artifact.Number || request.HeadSHA != artifact.HeadSHA {
		return ApprovalResult{}, workflowError("review_identity_invalid", true, nil)
	}
	if !plan.Await {
		return ApprovalResult{}, workflowError("approval_not_required", true, nil)
	}
	result := ApprovalResult{OK: true, ReviewID: workflowID, PublicationStatus: "pending"}
	var published PublishResult
	if hasPublication, err := engine.store.loadStep(ctx, workflowID, stepPublication, &published); err != nil {
		return ApprovalResult{}, err
	} else if hasPublication {
		result.PublicationStatus, result.GitHubReviewID, result.ReviewURL = "published", published.GitHubReviewID, published.ReviewURL
		return result, nil
	}
	decision := stored.Approval
	if decision == "" {
		if stored.Status != reviewAwaiting {
			return ApprovalResult{}, workflowError("review_not_awaiting_approval", true, nil)
		}
		if decision, err = engine.store.decideApproval(ctx, workflowID, approvalKindApprove); err != nil {
			return ApprovalResult{}, err
		}
	}
	switch decision {
	case approvalKindApprove:
		return result, nil
	case approvalKindExpired:
		return ApprovalResult{}, workflowError("approval_expired", true, nil)
	default:
		return ApprovalResult{}, workflowError("review_superseded", true, nil)
	}
}
