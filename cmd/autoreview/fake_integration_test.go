package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

type note struct {
	Level   policy.Level
	Message string
}

// fakeIntegration stands in for GitHub, pohunek and Matrix. Tests change the
// pull request state and reviewer outputs while reviews run.
type fakeIntegration struct {
	t          *testing.T
	mu         sync.Mutex
	candidates map[string]Candidate
	current    map[string]PullRequest
	lapsed     map[string]string
	closed     map[string]bool
	output     func(SessionRequest) []byte
	sessions   map[string]SessionRequest
	started    []SessionRequest
	// startFail fails a session start when it returns an error; attempts
	// records every start request, failed or not.
	startFail       func(SessionRequest) error
	attempts        []SessionRequest
	removeFailing   map[string]bool
	removed         []string
	commentable     CommentableLines
	deleted         []string
	deletedReads    int
	discussion      Discussion
	discussionReads [][]PriorFinding
	rewritten       map[string]bool
	publishFail     string
	// transientUntil makes pull request reads and searches fail transiently
	// until the given time.
	transientUntil time.Time
	rejected       []RejectedCandidate
	published      []GitHubReview
	notes          []note
	nextSession    int
	// listings counts candidate searches, so a test can wait for the
	// start-up discovery pass before it opens pull requests.
	listings int
	// requests are the review requests the queue search returns, in order.
	requests []ReviewRequest
	// queued records the digests the queue posted; queueSends counts attempts
	// and queueFailures makes that many leading attempts fail transiently.
	queued     []queuedDigest
	queueSends int
	// queueTransactions lists the transaction ID of every send attempt.
	queueTransactions []string
	queueFailures     int
}

type queuedDigest struct {
	transaction string
	plain       string
	formatted   string
}

func newFake(t *testing.T) *fakeIntegration {
	return &fakeIntegration{t: t, candidates: map[string]Candidate{}, current: map[string]PullRequest{}, lapsed: map[string]string{}, closed: map[string]bool{}, sessions: map[string]SessionRequest{}, removeFailing: map[string]bool{}, commentable: CommentableLines{}, discussion: Discussion{Threads: []DiscussionThread{}, Comments: []DiscussionComment{}}, rewritten: map[string]bool{}}
}

// open makes pr a candidate of rule with pr as its current state.
func (f *fakeIntegration) open(pr PullRequest, rule policy.Rule) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.candidates[pr.Key()] = Candidate{PullRequest: pr, Rule: rule}
	f.current[pr.Key()] = pr
	delete(f.lapsed, pr.Key())
}

// lapse removes the pull request from search and makes reads fail with code.
func (f *fakeIntegration) lapse(pr PullRequest, code string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.candidates, pr.Key())
	f.lapsed[pr.Key()] = code
}

func (f *fakeIntegration) ListCandidates(context.Context) (CandidateList, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.listings++
	if time.Now().Before(f.transientUntil) {
		return CandidateList{}, workflowError("github_request_failed", false, nil)
	}
	list := CandidateList{Candidates: []Candidate{}, Rejected: append([]RejectedCandidate{}, f.rejected...)}
	for _, candidate := range f.candidates {
		list.Candidates = append(list.Candidates, candidate)
	}
	return list, nil
}

func (f *fakeIntegration) ReadyDrafts(context.Context) ([]ReadiedDraft, error) {
	return []ReadiedDraft{}, nil
}

func (f *fakeIntegration) listingCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.listings
}

func (f *fakeIntegration) GetPullRequest(_ context.Context, repository string, number int, _ policy.Trigger) (PullRequest, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	key := fmt.Sprintf("%s#%d", repository, number)
	if time.Now().Before(f.transientUntil) {
		return PullRequest{}, workflowError("github_request_failed", false, nil)
	}
	if code, ok := f.lapsed[key]; ok {
		return PullRequest{}, workflowError(code, true, nil)
	}
	pr, ok := f.current[key]
	if !ok {
		return PullRequest{}, workflowError("pull_request_not_open", true, nil)
	}
	return pr, nil
}

// ReadPullRequestState reports a pull request of current as open; a candidate
// is requested again. A closed one is listed in closed.
func (f *fakeIntegration) ReadPullRequestState(_ context.Context, repository string, number int) (PullRequestState, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	key := fmt.Sprintf("%s#%d", repository, number)
	pr, ok := f.current[key]
	if !ok || f.closed[key] {
		return PullRequestState{}, nil
	}
	_, requested := f.candidates[key]
	return PullRequestState{Open: true, Requested: requested, Title: pr.Title, URL: pr.URL, Author: pr.Author, HeadSHA: pr.HeadSHA}, nil
}

// close closes pr on the fake GitHub.
func (f *fakeIntegration) close(pr PullRequest) {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.candidates, pr.Key())
	f.closed[pr.Key()] = true
}

func (f *fakeIntegration) ResolveProject(context.Context, string, []string) (ProjectRoute, error) {
	return ProjectRoute{Host: "review-host", ProjectID: "project"}, nil
}

// EnsureSession returns the live session of the same request name without
// recording a new start, as pohunek does for a review resumed after an
// interruption.
func (f *fakeIntegration) EnsureSession(_ context.Context, request SessionRequest) (SessionIdentity, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.attempts = append(f.attempts, request)
	if f.startFail != nil {
		if err := f.startFail(request); err != nil {
			return SessionIdentity{}, err
		}
	}
	for id, existing := range f.sessions {
		if existing.Name() == request.Name() {
			return SessionIdentity{Host: request.Route.Host, SessionID: id, WorkerInstanceID: "worker", RuntimeGeneration: "1", ReviewerID: request.Reviewer.ID}, nil
		}
	}
	f.nextSession++
	id := fmt.Sprintf("s-%d", f.nextSession)
	f.sessions[id] = request
	f.started = append(f.started, request)
	return SessionIdentity{Host: request.Route.Host, SessionID: id, WorkerInstanceID: "worker", RuntimeGeneration: "1", ReviewerID: request.Reviewer.ID}, nil
}

func (f *fakeIntegration) FindSession(context.Context, SessionRequest) (*SessionIdentity, error) {
	return nil, nil
}

func (f *fakeIntegration) SessionOutput(_ context.Context, session SessionIdentity, _ policy.Protocol) ([]byte, error) {
	f.mu.Lock()
	request, ok := f.sessions[session.SessionID]
	output := f.output
	f.mu.Unlock()
	if !ok {
		return nil, workflowError("pohunek_session_not_found", true, nil)
	}
	return output(request), nil
}

func (f *fakeIntegration) RemoveSessionConfirmed(ctx context.Context, session SessionIdentity) error {
	// Like the host, it fails once ctx has ended, so a stopping engine keeps its reviewer sessions.
	if err := ctx.Err(); err != nil {
		return err
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.removeFailing[session.SessionID] {
		return &PohunekCodeError{Code: "runtime_supervision_ambiguous", Cause: workflowError("pohunek_request_failed", false, nil)}
	}
	if _, ok := f.sessions[session.SessionID]; ok {
		delete(f.sessions, session.SessionID)
		f.removed = append(f.removed, session.SessionID)
	}
	return nil
}

func (f *fakeIntegration) ListCommentableLines(context.Context, PullRequest) (CommentableLines, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.commentable, nil
}

// ListDeletedFiles answers with the deleted files a test set, counting reads.
func (f *fakeIntegration) ListDeletedFiles(context.Context, PullRequest) ([]string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.deletedReads++
	return append([]string(nil), f.deleted...), nil
}

// ListDiscussion answers with the discussion a test set, recording each read.
func (f *fakeIntegration) ListDiscussion(_ context.Context, _ PullRequest, prior []PriorFinding) (Discussion, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.discussionReads = append(f.discussionReads, prior)
	return f.discussion, nil
}

// LinearHistory treats every pair of heads as linear unless a test marks the
// newer head as rewritten.
func (f *fakeIntegration) LinearHistory(_ context.Context, _ string, _ string, head string) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return !f.rewritten[head], nil
}

func (f *fakeIntegration) PublishReview(_ context.Context, pr PullRequest, review GitHubReview) (PublishResult, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.publishFail != "" {
		return PublishResult{}, workflowError(f.publishFail, true, nil)
	}
	for index, existing := range f.published {
		if existing.Marker == review.Marker {
			return PublishResult{Decision: review.Decision, GitHubReviewID: int64(index + 1), ReviewURL: pr.URL + "#review", Reused: true}, nil
		}
	}
	f.published = append(f.published, review)
	return PublishResult{Decision: review.Decision, GitHubReviewID: int64(len(f.published)), ReviewURL: pr.URL + "#review"}, nil
}

func (f *fakeIntegration) ListReviewRequests(_ context.Context, max int) (ReviewRequestList, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	found := f.requests[:min(max+1, len(f.requests))]
	return ReviewRequestList{Requests: append([]ReviewRequest(nil), found...), Truncated: len(f.requests) > max}, nil
}

func (f *fakeIntegration) SendQueueDigest(_ context.Context, transaction, plain, formatted string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.queueSends++
	f.queueTransactions = append(f.queueTransactions, transaction)
	if f.queueSends <= f.queueFailures {
		return workflowError("matrix_unavailable", false, nil)
	}
	f.queued = append(f.queued, queuedDigest{transaction: transaction, plain: plain, formatted: formatted})
	return nil
}

func (f *fakeIntegration) Notify(_ context.Context, level policy.Level, message string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.notes = append(f.notes, note{Level: level, Message: message})
	return nil
}

func (f *fakeIntegration) snapshot() (published []GitHubReview, notes []note, started []SessionRequest, open int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]GitHubReview(nil), f.published...), append([]note(nil), f.notes...), append([]SessionRequest(nil), f.started...), len(f.sessions)
}

func (f *fakeIntegration) hasNote(level policy.Level, fragment string) bool {
	_, notes, _, _ := f.snapshot()
	return countNotes(notes, level, fragment) > 0
}

func countNotes(notes []note, level policy.Level, fragment string) int {
	count := 0
	for _, entry := range notes {
		if entry.Level == level && strings.Contains(entry.Message, fragment) {
			count++
		}
	}
	return count
}

// requestPrior reads the prior findings a reviewer request hands over.
func requestPrior(t *testing.T, prompt string) []PriorFinding {
	t.Helper()
	// A structured reviewer receives its prompt inside a JSON request.
	var request struct {
		Prompt string `json:"prompt"`
	}
	if json.Unmarshal([]byte(prompt), &request) == nil && request.Prompt != "" {
		prompt = request.Prompt
	}
	const label = "Prior findings (JSON):\n"
	start := strings.Index(prompt, label)
	if start < 0 {
		return nil
	}
	line, _, _ := strings.Cut(prompt[start+len(label):], "\n")
	var prior []PriorFinding
	if err := json.Unmarshal([]byte(line), &prior); err != nil {
		t.Fatal(err)
	}
	return prior
}

// reviewerOutput returns a finished reviewer output with the given findings.
// It settles every prior finding as fixed.
func reviewerOutput(t *testing.T, findings []map[string]any) func(SessionRequest) []byte {
	return settlingOutput(t, findings, func(PriorFinding) string { return priorFixed })
}

// settlingOutput returns a finished reviewer output with the given findings
// and the verdict status picks for every prior finding of the request.
func settlingOutput(t *testing.T, findings []map[string]any, status func(PriorFinding) string) func(SessionRequest) []byte {
	return func(request SessionRequest) []byte {
		if request.Reviewer.ID == policy.VerifierReviewerID {
			return verifierOutput(t, request, func(claimFinding) string { return verdictConfirmed }, func(claimPrior) string { return verdictUpheld })
		}
		if request.Reviewer.Protocol == policy.ProtocolCodexEnvelope {
			return codexOutput(t, request.PullRequest, findings, 0)
		}
		verdicts := []map[string]any{}
		for _, prior := range requestPrior(t, request.Prompt) {
			verdicts = append(verdicts, map[string]any{"key": prior.Key, "status": status(prior), "note": "Checked against the current head.", "relies_on": ""})
		}
		claudeFindings := []map[string]any{}
		for index, finding := range findings {
			copied := map[string]any{"id": fmt.Sprintf("C%03d", index+1), "side": "RIGHT"}
			for key, value := range finding {
				copied[key] = value
			}
			claudeFindings = append(claudeFindings, copied)
		}
		return claudeOutputWithPrior(t, request.PullRequest, claudeFindings, verdicts)
	}
}

// requestClaims reads the claims a verifier request hands over.
func requestClaims(t *testing.T, prompt string) verificationClaims {
	t.Helper()
	var request struct {
		Prompt string `json:"prompt"`
	}
	if err := json.Unmarshal([]byte(prompt), &request); err != nil {
		t.Fatal(err)
	}
	const label = "Claims (untrusted JSON):\n"
	start := strings.Index(request.Prompt, label)
	if start < 0 {
		t.Fatalf("the verifier request carries no claims: %s", request.Prompt)
	}
	line, _, _ := strings.Cut(request.Prompt[start+len(label):], "\n")
	var claims verificationClaims
	if err := json.Unmarshal([]byte(line), &claims); err != nil {
		t.Fatal(err)
	}
	return claims
}

// verifierOutput returns a finished verifier output that gives every claim of
// the request the verdict the functions pick.
func verifierOutput(t *testing.T, request SessionRequest, finding func(claimFinding) string, prior func(claimPrior) string) []byte {
	claims := requestClaims(t, request.Prompt)
	findings, priors := []map[string]any{}, []map[string]any{}
	for _, claim := range claims.Findings {
		findings = append(findings, map[string]any{"id": claim.ID, "verdict": finding(claim), "note": "Traced the scenario at the head."})
	}
	for _, claim := range claims.Prior {
		priors = append(priors, map[string]any{"key": claim.Key, "verdict": prior(claim), "note": "Read the code at the head."})
	}
	pr := request.PullRequest
	return claudeStructuredOutput(map[string]any{"repository": pr.Repository, "number": pr.Number, "head_sha": pr.HeadSHA, "findings": findings, "prior_findings": priors})
}

// verifying answers verifier requests with the given verdicts and every other
// request with reviewers.
func verifying(t *testing.T, reviewers func(SessionRequest) []byte, finding func(claimFinding) string, prior func(claimPrior) string) func(SessionRequest) []byte {
	return func(request SessionRequest) []byte {
		if request.Reviewer.ID == policy.VerifierReviewerID {
			return verifierOutput(t, request, finding, prior)
		}
		return reviewers(request)
	}
}

const integrationPolicy = `version: 1
reviewers:
  opus: {protocol: claude-structured, agent_profile: claude-profile, label: Opus, finding_prefix: O}
  sonnet: {protocol: claude-structured, agent_profile: claude-profile-sonnet, label: Sonnet, finding_prefix: S}
  codex: {protocol: codex-envelope, agent_profile: codex-profile, label: Codex, finding_prefix: X}
pipelines:
  manual: {reviewers: [opus, sonnet], incremental_reviewers: [sonnet], prompt: prompt.md, checkout: base, project_sources: [manual]}
  dual: {reviewers: [opus, sonnet], incremental_reviewers: [opus, sonnet], prompt: prompt.md, checkout: base, project_sources: [manual]}
  solo: {reviewers: [codex], incremental_reviewers: [codex], prompt: prompt.md, checkout: head, project_sources: [manual]}
  checked:
    reviewers: [opus, sonnet]
    incremental_reviewers: [opus, sonnet]
    prompt: prompt.md
    checkout: base
    project_sources: [manual]
    verifier: {reviewer: opus, prompt: verifier.md, escalate_severities: [critical], escalate_count: 2}
repositories:
  - {match: acme/manual, trigger: review-requested, pipeline: manual, on_findings: request_changes, on_clean: await}
  - {match: acme/notes, trigger: review-requested, pipeline: manual, on_findings: request_changes, on_clean: await, on_notes: comment}
  - {match: acme/*, trigger: review-requested, pipeline: dual, on_findings: request_changes, on_clean: approve}
  - match: risky/*
    trigger: review-requested
    pipeline: dual
    on_findings: request_changes
    on_clean: approve
    human_review: {labels: [needs-human], deleted_tests: ["tests/**", "**/tests/**"]}
  - {match: solo/*, trigger: "label:ai:review", pipeline: solo, on_findings: comment, on_clean: comment}
  - {match: checked/manual, trigger: review-requested, pipeline: checked, on_findings: request_changes, on_clean: await}
  - {match: checked/*, trigger: review-requested, pipeline: checked, on_findings: request_changes, on_clean: approve}
unmatched: skip
approval: {timeout_hours: 1}
notes_handoff: {reminder_hours: 24}
notifications:
  approval_requested: action
  findings_on_own_pr: action
  review_stopped: action
  cleanup_manual: action
  command_failed: action
  approved: digest
  changes_requested: digest
  commented: digest
  approval_expired: digest
  started: log
  retry_scheduled: log
  superseded: log
  handoff_reminder: action
`

// harness runs one Engine on a fresh store against the fake integration.
// Discovery runs only when a test calls poll, because PollInterval is an hour.
type harness struct {
	t      *testing.T
	cfg    Config
	fake   *fakeIntegration
	store  *Store
	engine *Engine
	cancel context.CancelFunc
	done   chan error
}

func integrationConfig(t *testing.T) Config {
	t.Helper()
	parsed, err := policy.Parse([]byte(integrationPolicy), func(string) ([]byte, error) { return []byte("Review the pull request."), nil })
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig()
	cfg.Policy = parsed
	cfg.StateDBPath = filepath.Join(t.TempDir(), "state.db")
	cfg.StateEntryLimit = 5000
	cfg.ScheduleMaxBytes = 32 << 20
	cfg.PublishedRetention = 24 * time.Hour
	cfg.SessionTimeout = 20 * time.Second
	cfg.OutputPollInterval = 100 * time.Millisecond
	cfg.SessionStartAttempts, cfg.SessionStartRetry = 2, 100*time.Millisecond
	cfg.SessionCleanupAttempts, cfg.SessionCleanupRetry = 2, 50*time.Millisecond
	cfg.ReviewerRunAttempts, cfg.ReviewerRunRetry = 2, 100*time.Millisecond
	cfg.StepRetry = 5 * time.Second
	cfg.StepRetryInitial = 10 * time.Millisecond
	cfg.StepRetryMaxInterval = 100 * time.Millisecond
	cfg.PollInterval = time.Hour
	cfg.WorkerTick = 20 * time.Millisecond
	cfg.DiscoveryLockTimeout = 5 * time.Second
	cfg.DiscoveryLockPoll = 10 * time.Millisecond
	cfg.ReviewRetention = 48 * time.Hour
	cfg.MaxConcurrentReviews = 8
	cfg.ReviewListLimit = 50
	cfg.Schedule.Debounce = 100 * time.Millisecond
	cfg.Schedule.RetryDelays = []time.Duration{200 * time.Millisecond}
	return cfg
}

// startHarness opens a fresh store and starts an engine on it. configure may
// change the configuration before the engine is built.
func startHarness(t *testing.T, configure func(*Config)) *harness {
	t.Helper()
	cfg := integrationConfig(t)
	if configure != nil {
		configure(&cfg)
	}
	store, err := OpenStore(cfg.StateDBPath, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	h := &harness{t: t, cfg: cfg, fake: newFake(t), store: store}
	h.fake.output = reviewerOutput(t, nil)
	h.start()
	t.Cleanup(h.stopEngine)
	// The start-up pass lists the searches before any test opens a pull request,
	// so it never sees one half-registered.
	h.waitFor("the start-up discovery pass", func() bool { return h.fake.listingCount() >= 1 })
	return h
}

// start runs a new engine on the harness store until stopEngine.
func (h *harness) start() {
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	h.engine = NewEngine(h.cfg, h.store, h.fake, logger, time.Now, sleepWithContext)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- h.engine.Serve(ctx) }()
	h.cancel, h.done = cancel, done
}

// stopEngine stops the running engine and waits until Serve returned, which
// releases the engine lock on the store.
func (h *harness) stopEngine() {
	if h.cancel == nil {
		return
	}
	h.cancel()
	if err := <-h.done; err != nil {
		h.t.Errorf("engine stopped with error: %v", err)
	}
	h.cancel, h.done = nil, nil
}

func (h *harness) setOutput(output func(SessionRequest) []byte) {
	h.fake.mu.Lock()
	defer h.fake.mu.Unlock()
	h.fake.output = output
}

func (h *harness) poll() PollResult {
	h.t.Helper()
	result, err := h.engine.Poll(context.Background())
	if err != nil {
		h.t.Fatal(err)
	}
	return result
}

// pollUntil polls discovery until condition holds.
// settle polls for longer than the scheduling debounce and lets the worker
// pick up whatever the polls dispatched, so a review that must not start had
// the chance to.
func (h *harness) settle() {
	h.t.Helper()
	for range 5 {
		h.poll()
		time.Sleep(150 * time.Millisecond)
	}
}

func (h *harness) pollUntil(description string, condition func() bool) {
	h.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			published, notes, started, open := h.fake.snapshot()
			h.t.Fatalf("timed out waiting for %s: published=%d notes=%+v started=%d open sessions=%d", description, len(published), notes, len(started), open)
		}
		h.poll()
		time.Sleep(150 * time.Millisecond)
	}
}

func (h *harness) waitFor(description string, condition func() bool) {
	h.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			published, notes, started, open := h.fake.snapshot()
			h.t.Fatalf("timed out waiting for %s: published=%d notes=%+v started=%d open sessions=%d", description, len(published), notes, len(started), open)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func (h *harness) publications() int {
	published, _, _, _ := h.fake.snapshot()
	return len(published)
}

func (h *harness) startedCount() int {
	_, _, started, _ := h.fake.snapshot()
	return len(started)
}

func (h *harness) resolve(pr PullRequest) (ResolveResult, error) {
	return h.engine.Resolve(context.Background(), ResolveRequest{Repository: pr.Repository, Number: pr.Number, HeadPrefix: pr.HeadSHA[:12]})
}

func (h *harness) approve(workflowID string, pr PullRequest) (ApprovalResult, error) {
	return h.engine.Approve(context.Background(), workflowID, ApproveRequest{Repository: pr.Repository, Number: pr.Number, HeadSHA: pr.HeadSHA})
}

// approveWhenAwaiting approves the review of pr once it is stored as awaiting.
// The approval request is sent before that status is stored, and Approve
// refuses a review that is not awaiting yet.
func (h *harness) approveWhenAwaiting(pr PullRequest) (ApprovalResult, error) {
	h.t.Helper()
	resolved, err := h.resolve(pr)
	if err != nil {
		h.t.Fatal(err)
	}
	h.waitFor("the stored approval wait", func() bool {
		stored, found, err := h.store.getReview(context.Background(), resolved.WorkflowID)
		return err == nil && found && stored.Status == reviewAwaiting
	})
	return h.approve(resolved.WorkflowID, pr)
}

// stepRecorded reports whether the review of the first started session has
// stored the named step.
func (h *harness) stepRecorded(name string) bool {
	_, _, started, _ := h.fake.snapshot()
	if len(started) == 0 {
		return false
	}
	var recorded bool
	found, err := h.store.loadStep(context.Background(), started[0].WorkflowID, name, &recorded)
	return err == nil && found
}

func (h *harness) retry(pr PullRequest) (RetryResult, error) {
	return h.engine.Retry(context.Background(), RetryRequest{Repository: pr.Repository, Number: pr.Number, HeadSHA: pr.HeadSHA[:12]})
}

func uniqueNumber() int { return int(time.Now().UnixNano()%900000000) + 1 }

func integrationPullRequest(repository string, head string) PullRequest {
	number := uniqueNumber()
	pr := PullRequest{Repository: repository, Number: number, HeadSHA: head, ReviewRequestID: 11, Title: "Change", URL: fmt.Sprintf("https://github.com/%s/pull/%d", repository, number), Author: "colleague", HeadBranch: "feature", BaseBranch: "main", BaseSHA: sha("b")}
	return pr
}

func (h *harness) rule(repository string) policy.Rule {
	rule, ok := h.cfg.Policy.Resolve(repository)
	if !ok {
		h.t.Fatalf("%s is not covered by the integration policy", repository)
	}
	return rule
}
