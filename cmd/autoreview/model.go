package main

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

const (
	// workflowIDPrefix marks reviews of this engine; pohunek session names
	// derive from the review ID.
	workflowIDPrefix = "prr-v2-"
	// inputVersion is the contract of ReviewInput that discovery writes. A
	// review accepts every version from minInputVersion on, so a review stored
	// before an upgrade resumes under the rules it was dispatched with.
	inputVersion    = 5
	minInputVersion = 2
	// blockingGateVersion is the first input whose findings may be marked
	// non-blocking; under an earlier input every finding blocks.
	blockingGateVersion = 3
	// priorFindingsVersion is the first input that may carry the blocking
	// findings of earlier reviews, each of which the reviewers must settle.
	priorFindingsVersion = 4
	// verifierVersion is the first input whose blocking findings the pipeline's
	// verifier checks before publication.
	verifierVersion = 5
)

// PullRequest keeps the JSON names of the stored review input, from which the
// scheduler reads repository, number, head_sha and review_request_id.
type PullRequest struct {
	Repository      string `json:"repository"`
	Number          int    `json:"number"`
	HeadSHA         string `json:"head_sha"`
	ReviewRequestID int64  `json:"review_request_id"`
	Title           string `json:"title"`
	URL             string `json:"url"`
	Author          string `json:"author"`
	// AuthorBot marks an app author, such as a coding agent, which never
	// decides on notes handed to it.
	AuthorBot  bool   `json:"author_bot,omitempty"`
	HeadBranch string `json:"head_branch"`
	BaseBranch string `json:"base_branch"`
	BaseSHA    string `json:"base_sha"`
	// Labels are the pull request's labels when it was read.
	Labels []string `json:"labels,omitempty"`
}

func (pr PullRequest) Key() string { return fmt.Sprintf("%s#%d", pr.Repository, pr.Number) }

func (pr PullRequest) Target() schedule.Target {
	return schedule.Target{Repository: pr.Repository, Number: pr.Number, HeadSHA: pr.HeadSHA, RequestID: pr.ReviewRequestID}
}

// ReviewID is the notification and command handle of a review head.
func (pr PullRequest) ReviewID() string {
	return fmt.Sprintf("%s#PR%d@%.12s", pr.Repository, pr.Number, pr.HeadSHA)
}

// WorkflowID is deterministic per review identity and attempt, so a repeated
// dispatch of the same attempt reaches the same workflow.
func (pr PullRequest) WorkflowID(attempt int) string {
	identity := fmt.Sprintf("pr-review-v2\x00%s\x00%d\x00%s\x00%d\x00%d", pr.Repository, pr.Number, pr.HeadSHA, pr.ReviewRequestID, attempt)
	digest := sha256.Sum256([]byte(identity))
	return workflowIDPrefix + hex.EncodeToString(digest[:])
}

// Candidate is a pull request the policy covers, found by its rule's trigger.
type Candidate struct {
	PullRequest PullRequest `json:"pull_request"`
	Rule        policy.Rule `json:"rule"`
}

// RejectedCandidate is a pull request a trigger found but whose data the engine
// refuses, such as a response that fails validation.
type RejectedCandidate struct {
	Repository string `json:"repository"`
	Number     int    `json:"number"`
	Code       string `json:"code"`
}

func (rejected RejectedCandidate) Key() string {
	return fmt.Sprintf("%s#%d", rejected.Repository, rejected.Number)
}

// CandidateList is the result of one discovery search. A rejected pull
// request does not hold up the others.
type CandidateList struct {
	Candidates []Candidate         `json:"candidates"`
	Rejected   []RejectedCandidate `json:"rejected"`
}

type ReviewScope struct {
	Mode            string `json:"mode"`
	PreviousHeadSHA string `json:"previous_head_sha,omitempty"`
}

const (
	scopeFull        = "full"
	scopeIncremental = "incremental"
)

func (scope ReviewScope) Incremental() bool { return scope.Mode == scopeIncremental }

type ProjectRoute struct {
	Host      string `json:"host"`
	ProjectID string `json:"project_id"`
}

// ReviewInput carries everything the workflow decides by, so a policy or
// configuration change never alters a review that is already running.
type ReviewInput struct {
	Version                int                           `json:"version"`
	PullRequest            PullRequest                   `json:"pull_request"`
	Attempt                int                           `json:"attempt"`
	Rule                   policy.Rule                   `json:"rule"`
	Reviewers              []policy.Reviewer             `json:"reviewers"`
	Scope                  ReviewScope                   `json:"scope"`
	Route                  ProjectRoute                  `json:"route"`
	Notifications          map[policy.Event]policy.Level `json:"notifications"`
	ApprovalTimeoutSeconds int64                         `json:"approval_timeout_seconds"`
	PolicyDigest           string                        `json:"policy_digest"`
	PublisherLogin         string                        `json:"publisher_login"`
	Limits                 RunLimits                     `json:"limits"`
	// Prior holds the blocking findings that earlier reviews of the pull
	// request left open. A full review carries them too, after a rebase.
	Prior []PriorFinding `json:"prior,omitempty"`
	// Handoff names the published review that handed the pull request's
	// non-blocking findings to its author, so this review waits for the user.
	Handoff *NotesHandoff `json:"handoff,omitempty"`
}

// NotesHandoff is a published review that handed non-blocking findings to the
// author. Adopt means the head is unchanged and its result is stored, so the
// review takes that result instead of running the reviewers again.
type NotesHandoff struct {
	WorkflowID string `json:"workflow_id"`
	HeadSHA    string `json:"head_sha"`
	Adopt      bool   `json:"adopt,omitempty"`
}

func (input ReviewInput) adoptsHandoff() bool { return input.Handoff != nil && input.Handoff.Adopt }

// PriorFinding is a blocking finding of an earlier review. Key is unique
// across reviews, because each review numbers its findings from 1 again.
type PriorFinding struct {
	Key            string `json:"key"`
	ID             string `json:"id"`
	HeadSHA        string `json:"head_sha"`
	GitHubReviewID int64  `json:"github_review_id"`
	Severity       string `json:"severity"`
	Path           string `json:"path"`
	Line           int    `json:"line"`
	Title          string `json:"title"`
	Body           string `json:"body"`
}

func priorKey(head, id string) string { return fmt.Sprintf("%.12s:%s", head, id) }

// Verdicts on a prior finding. Only an open one keeps blocking.
const (
	priorFixed    = "fixed"
	priorOpen     = "still_open"
	priorAnswered = "answered"
)

// PriorVerdict is one reviewer's judgement of a prior finding.
type PriorVerdict struct {
	Key    string `json:"key"`
	Status string `json:"status"`
	Note   string `json:"note"`
	// ReliesOn is the login of the comment an answered verdict rests on, or
	// empty when the verdict rests on the code alone.
	ReliesOn string `json:"relies_on,omitempty"`
}

// PriorResult is the combined verdict on a prior finding.
type PriorResult struct {
	Finding PriorFinding `json:"finding"`
	PriorVerdict
	// Republished names the new finding an open prior finding was published
	// again as, because it had no unresolved thread left to answer.
	Republished string `json:"republished,omitempty"`
	// Reopened marks an answered verdict that the verifier did not uphold.
	Reopened bool `json:"reopened,omitempty"`
}

// Discussion is what people and agents wrote on the pull request since the
// earlier reviews. It is untrusted data for the reviewers.
type Discussion struct {
	Threads  []DiscussionThread  `json:"threads"`
	Comments []DiscussionComment `json:"comments"`
}

// DiscussionThread is the reply thread of one prior finding's inline comment.
type DiscussionThread struct {
	Key      string              `json:"key"`
	Resolved bool                `json:"resolved"`
	Replies  []DiscussionComment `json:"replies"`
}

type DiscussionComment struct {
	Author string `json:"author"`
	// Role is reviewer for the reviewing account's own human comments, author
	// for the pull request author and other for anyone else.
	Role     string `json:"role"`
	AISigned bool   `json:"ai_signed"`
	Body     string `json:"body"`
}

// RunLimits bound the loops of one review, in milliseconds. They are part of
// the input so a review resumed after a configuration change keeps its limits.
type RunLimits struct {
	SessionTimeoutMS       int64 `json:"session_timeout_ms"`
	OutputPollMS           int64 `json:"output_poll_ms"`
	SessionStartAttempts   int   `json:"session_start_attempts"`
	SessionStartRetryMS    int64 `json:"session_start_retry_ms"`
	SessionCleanupAttempts int   `json:"session_cleanup_attempts"`
	SessionCleanupRetryMS  int64 `json:"session_cleanup_retry_ms"`
	ReviewerRunAttempts    int   `json:"reviewer_run_attempts"`
	ReviewerRunRetryMS     int64 `json:"reviewer_run_retry_ms"`
	StepRetryMS            int64 `json:"step_retry_ms"`
	StepRetryInitialMS     int64 `json:"step_retry_initial_ms"`
	StepRetryMaxIntervalMS int64 `json:"step_retry_max_interval_ms"`
}

func runLimits(cfg Config) RunLimits {
	return RunLimits{
		SessionTimeoutMS: cfg.SessionTimeout.Milliseconds(), OutputPollMS: cfg.OutputPollInterval.Milliseconds(),
		SessionStartAttempts: cfg.SessionStartAttempts, SessionStartRetryMS: cfg.SessionStartRetry.Milliseconds(),
		SessionCleanupAttempts: cfg.SessionCleanupAttempts, SessionCleanupRetryMS: cfg.SessionCleanupRetry.Milliseconds(),
		ReviewerRunAttempts: cfg.ReviewerRunAttempts, ReviewerRunRetryMS: cfg.ReviewerRunRetry.Milliseconds(),
		StepRetryMS: cfg.StepRetry.Milliseconds(), StepRetryInitialMS: cfg.StepRetryInitial.Milliseconds(),
		StepRetryMaxIntervalMS: cfg.StepRetryMaxInterval.Milliseconds(),
	}
}

func (limits RunLimits) valid() bool {
	return limits.SessionTimeoutMS > 0 && limits.OutputPollMS > 0 && limits.OutputPollMS <= limits.SessionTimeoutMS &&
		limits.SessionStartAttempts > 0 && limits.SessionStartRetryMS > 0 && limits.SessionCleanupAttempts > 0 &&
		limits.SessionCleanupRetryMS > 0 && limits.ReviewerRunAttempts > 0 && limits.ReviewerRunRetryMS > 0 && limits.StepRetryMS > 0 &&
		limits.StepRetryInitialMS > 0 && limits.StepRetryMaxIntervalMS >= limits.StepRetryInitialMS
}

func milliseconds(value int64) time.Duration { return time.Duration(value) * time.Millisecond }

func (input ReviewInput) level(event policy.Event) policy.Level { return input.Notifications[event] }

// openPrior returns the prior findings the review left open; they hold the
// merge back like a new blocking finding.
func (artifact Artifact) openPrior() []PriorResult {
	open := []PriorResult{}
	for _, result := range artifact.PriorResults {
		// A republished one is counted as the new finding it became.
		if result.Status == priorOpen && result.Republished == "" {
			open = append(open, result)
		}
	}
	return open
}

// blocks reports whether a finding of this review holds the merge back.
func (input ReviewInput) blocks(finding Finding) bool {
	return input.Version < blockingGateVersion || !finding.NonBlocking
}

// splitFindings separates the findings that hold the merge back from the
// notes, which are published without asking for changes.
func (input ReviewInput) splitFindings(findings []Finding) (blocking, notes []Finding) {
	for _, finding := range findings {
		if input.blocks(finding) {
			blocking = append(blocking, finding)
		} else {
			notes = append(notes, finding)
		}
	}
	return blocking, notes
}

type SessionIdentity struct {
	Host              string `json:"host"`
	SessionID         string `json:"session_id"`
	WorkerInstanceID  string `json:"worker_instance_id"`
	RuntimeGeneration string `json:"runtime_generation"`
	ReviewerID        string `json:"reviewer_id"`
}

type Finding struct {
	ID            string `json:"id"`
	SourceID      string `json:"source_id"`
	ReviewerID    string `json:"reviewer_id"`
	ReviewerLabel string `json:"reviewer_label"`
	Severity      string `json:"severity"`
	Confidence    string `json:"confidence"`
	Path          string `json:"path"`
	Line          int    `json:"line"`
	Title         string `json:"title"`
	Body          string `json:"body"`
	// Anchored reports whether Line is a changed RIGHT-side line of the diff,
	// so the finding can be posted as an inline comment.
	Anchored bool `json:"anchored"`
	// NonBlocking marks a note that is published without asking for changes.
	// The zero value blocks, so a finding without the flag, such as a stored
	// one or one of a protocol that has no such field, keeps blocking.
	NonBlocking bool `json:"non_blocking,omitempty"`
	// Verdict is the verifier's judgement of a blocking finding, empty when no
	// verifier checked it.
	Verdict string `json:"verdict,omitempty"`
}

// ReviewerResult is the validated output of one reviewer session.
type ReviewerResult struct {
	ReviewerID string    `json:"reviewer_id"`
	Summary    string    `json:"summary"`
	Findings   []Finding `json:"findings"`
	Tests      []string  `json:"tests"`
	// Prior holds the reviewer's verdict on every prior finding of the input.
	Prior []PriorVerdict `json:"prior,omitempty"`
}

type ReviewerSummary struct {
	ReviewerID string `json:"reviewer_id"`
	Label      string `json:"label"`
	Summary    string `json:"summary"`
}

// Artifact is the combined review of one pull request head.
type Artifact struct {
	Repository string            `json:"repository"`
	Number     int               `json:"number"`
	HeadSHA    string            `json:"head_sha"`
	Scope      ReviewScope       `json:"scope"`
	Summaries  []ReviewerSummary `json:"summaries"`
	Findings   []Finding         `json:"findings"`
	Tests      []string          `json:"tests"`
	ReviewedAt string            `json:"reviewed_at"`
	// PriorResults settle the input's prior findings, in their order.
	PriorResults []PriorResult `json:"prior_results,omitempty"`
	// Verification reports the verifier's run, absent when nothing was checked.
	Verification *Verification `json:"verification,omitempty"`
	// HumanReview lists the signals of the rule's human_review that the pull
	// request matched, so a clean review waits for the user.
	HumanReview []string `json:"human_review,omitempty"`
}

// Decision is the GitHub review event.
type Decision string

const (
	DecisionApprove        Decision = "APPROVE"
	DecisionRequestChanges Decision = "REQUEST_CHANGES"
	DecisionComment        Decision = "COMMENT"
)

type PublishResult struct {
	Decision       Decision `json:"decision"`
	GitHubReviewID int64    `json:"github_review_id"`
	ReviewURL      string   `json:"review_url"`
	Reused         bool     `json:"reused"`
}

// PublishedRecord is the last review published for a pull request. It decides
// the scope of the next review.
type PublishedRecord struct {
	Repository      string    `json:"repository"`
	Number          int       `json:"number"`
	HeadSHA         string    `json:"head_sha"`
	ReviewRequestID int64     `json:"review_request_id"`
	Decision        Decision  `json:"decision"`
	WorkflowID      string    `json:"workflow_id"`
	PublishedAt     time.Time `json:"published_at"`
	// AwaitsAuthor marks a comment that handed non-blocking findings to the
	// author; the author's next review request asks the user for approval.
	AwaitsAuthor bool `json:"awaits_author,omitempty"`
	// Reminded records that the user was reminded of the hand-off once.
	Reminded bool `json:"reminded,omitempty"`
}

func (record PublishedRecord) Key() string {
	return fmt.Sprintf("%s#%d", record.Repository, record.Number)
}

// covers reports whether this record is the published review of the pull
// request's current head. A renewed review request without a new commit is
// covered: reviewing an unchanged head again only yields another sample of the
// same review. A renewed request after a hand-off of notes is not covered,
// because it is the author's answer that the user's approval follows.
func (record PublishedRecord) covers(pr PullRequest) bool {
	return record.HeadSHA == pr.HeadSHA && !(record.AwaitsAuthor && record.ReviewRequestID != pr.ReviewRequestID)
}

// AwaitRecord names a clean review that waits for a manual approval.
type AwaitRecord struct {
	Repository      string         `json:"repository"`
	Number          int            `json:"number"`
	HeadSHA         string         `json:"head_sha"`
	ReviewRequestID int64          `json:"review_request_id"`
	Trigger         policy.Trigger `json:"trigger"`
	WorkflowID      string         `json:"workflow_id"`
}

// matches reports whether the pull request is still at the waiting head; a
// renewed review request without a new commit keeps the wait.
func (record AwaitRecord) matches(pr PullRequest) bool {
	return pr.HeadSHA == record.HeadSHA
}

func (record AwaitRecord) Key() string { return fmt.Sprintf("%s#%d", record.Repository, record.Number) }

type Outcome string

const (
	OutcomePublished  Outcome = "published"
	OutcomeSuperseded Outcome = "superseded"
	OutcomeExpired    Outcome = "approval_expired"
)

type ReviewOutcome struct {
	Outcome     Outcome        `json:"outcome"`
	Publication *PublishResult `json:"publication,omitempty"`
}

const (
	approvalKindApprove    = "approve"
	approvalKindSuperseded = "superseded"
	approvalKindExpired    = "expired"
)

type ApprovalDecision struct {
	Kind string `json:"kind"`
}

type ApproveRequest struct {
	Repository string `json:"repository"`
	Number     int    `json:"number"`
	HeadSHA    string `json:"head_sha"`
}

type ApprovalResult struct {
	OK                bool   `json:"ok"`
	ReviewID          string `json:"review_id"`
	PublicationStatus string `json:"publication_status"`
	GitHubReviewID    int64  `json:"github_review_id,omitempty"`
	ReviewURL         string `json:"review_url,omitempty"`
}

type ResolveRequest struct {
	Repository string `json:"repository"`
	Number     int    `json:"number"`
	HeadPrefix string `json:"head_prefix"`
}

type ResolveResult struct {
	OK         bool   `json:"ok"`
	WorkflowID string `json:"workflow_id"`
	HeadSHA    string `json:"head_sha"`
	Findings   int    `json:"findings"`
}

type RetryRequest struct {
	Repository string `json:"repository"`
	Number     int    `json:"number"`
	HeadSHA    string `json:"head_sha"`
}

type RetryResult struct {
	OK         bool   `json:"ok"`
	WorkflowID string `json:"workflow_id"`
	HeadSHA    string `json:"head_sha"`
	Attempt    int    `json:"attempt"`
}

type PollResult struct {
	Dispatched int `json:"dispatched"`
	Open       int `json:"open"`
	Superseded int `json:"superseded"`
}

type WorkflowError struct {
	Code      string
	Permanent bool
	Cause     error
}

func (err *WorkflowError) Error() string {
	if err.Cause == nil {
		return err.Code
	}
	return fmt.Sprintf("%s: %v", err.Code, err.Cause)
}

func (err *WorkflowError) Unwrap() error { return err.Cause }

func workflowError(code string, permanent bool, cause error) error {
	return &WorkflowError{Code: code, Permanent: permanent, Cause: cause}
}

func errorCode(err error) string {
	var workflowErr *WorkflowError
	if errors.As(err, &workflowErr) {
		return workflowErr.Code
	}
	var config *ConfigError
	if errors.As(err, &config) {
		return config.Code
	}
	return "workflow_failed"
}

// PohunekCodeError carries the allowlisted error code of a failed CLI call.
type PohunekCodeError struct {
	Code  string
	Cause error
}

func (err *PohunekCodeError) Error() string {
	return fmt.Sprintf("%v: pohunek error code %s", err.Cause, err.Code)
}

func (err *PohunekCodeError) Unwrap() error { return err.Cause }

// pohunekErrorCode returns the CLI error code behind err, or "" when the CLI
// reported none.
func pohunekErrorCode(err error) string {
	var coded *PohunekCodeError
	if errors.As(err, &coded) {
		return coded.Code
	}
	return ""
}

func reviewIdentityValid(repository string, number int, head string) bool {
	return repositoryPattern.MatchString(repository) && number > 0 && shaPattern.MatchString(head)
}

func headPrefixValid(value string) bool {
	if len(value) < 12 || len(value) > 40 {
		return false
	}
	return strings.Trim(value, "0123456789abcdef") == ""
}
