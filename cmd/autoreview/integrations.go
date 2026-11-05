package main

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"strings"

	"github.com/pohunekd/autoreview/policy"
)

// Integrations are the side effects of the review engine. Workflow code calls
// them only inside durable steps.
type Integrations interface {
	ListCandidates(context.Context) (CandidateList, error)
	// ReadyDrafts marks the drafts of coding agents that ask for a review as
	// ready for review and returns them.
	ReadyDrafts(context.Context) ([]ReadiedDraft, error)
	GetPullRequest(ctx context.Context, repository string, number int, trigger policy.Trigger) (PullRequest, error)
	// ReadPullRequestState reads a pull request without any trigger check, for
	// one whose notes wait for its author.
	ReadPullRequestState(ctx context.Context, repository string, number int) (PullRequestState, error)
	ResolveProject(ctx context.Context, repository string, sources []string) (ProjectRoute, error)
	EnsureSession(context.Context, SessionRequest) (SessionIdentity, error)
	FindSession(context.Context, SessionRequest) (*SessionIdentity, error)
	SessionOutput(context.Context, SessionIdentity, policy.Protocol) ([]byte, error)
	RemoveSessionConfirmed(context.Context, SessionIdentity) error
	ListCommentableLines(context.Context, PullRequest) (CommentableLines, error)
	// ListDeletedFiles returns the paths of the files the pull request deletes.
	ListDeletedFiles(context.Context, PullRequest) ([]string, error)
	// ListDiscussion reads the reply threads of the prior findings' inline
	// comments and the pull request's conversation, bounded in size.
	ListDiscussion(context.Context, PullRequest, []PriorFinding) (Discussion, error)
	// LinearHistory reports whether head extends previous by commits that
	// each have one parent, so the diff between them is the author's work.
	LinearHistory(ctx context.Context, repository, previous, head string) (bool, error)
	PublishReview(context.Context, PullRequest, GitHubReview) (PublishResult, error)
	Notify(context.Context, policy.Level, string) error
	ListReviewRequests(ctx context.Context, max int) (ReviewRequestList, error)
	SendQueueDigest(ctx context.Context, transaction, plain, formatted string) error
}

// ReadiedDraft is a draft that discovery marked ready for review.
type ReadiedDraft struct {
	Repository string `json:"repository"`
	Number     int    `json:"number"`
	Author     string `json:"author"`
}

// SessionRequest identifies one reviewer session of one workflow attempt.
type SessionRequest struct {
	PullRequest PullRequest     `json:"pull_request"`
	Route       ProjectRoute    `json:"route"`
	Reviewer    policy.Reviewer `json:"reviewer"`
	Checkout    policy.Checkout `json:"checkout"`
	WorkflowID  string          `json:"workflow_id"`
	// Run counts the sessions of this reviewer in the workflow; a reviewer
	// whose output was unusable gets a fresh session with the next run.
	Run int `json:"run"`
	// Start counts the start attempts of one run. Pohunek keeps the branch of
	// a removed session and checks an existing branch out as it is, so every
	// start after the first gets its own branch to begin from the base again.
	Start  int    `json:"start,omitempty"`
	Prompt string `json:"prompt"`
}

func (request SessionRequest) token() string {
	token := fmt.Sprintf("%s-%s-run%d", request.WorkflowID, request.Reviewer.ID, request.Run)
	if request.Start > 1 {
		token += fmt.Sprintf("-start%d", request.Start)
	}
	return token
}

func (request SessionRequest) Name() string { return "github-review-" + request.token() }

func (request SessionRequest) Branch() string { return "autoreview/review/" + request.token() }

// BaseRef is the ref the reviewer worktree starts from.
func (request SessionRequest) BaseRef() string {
	if request.Checkout == policy.CheckoutHead {
		return fmt.Sprintf("refs/pull/%d/head", request.PullRequest.Number)
	}
	return request.PullRequest.BaseBranch
}

type CommandRunner interface {
	Run(ctx context.Context, binary string, args []string, input []byte, environment []string) ([]byte, error)
}

type execRunner struct{}

func (execRunner) Run(ctx context.Context, binary string, args []string, input []byte, environment []string) ([]byte, error) {
	command := exec.CommandContext(ctx, binary, args...)
	command.Env = environment
	command.Stdin = bytes.NewReader(input)
	var stdout, stderr bytes.Buffer
	command.Stdout = &stdout
	command.Stderr = &stderr
	if err := command.Run(); err != nil {
		return stdout.Bytes(), fmt.Errorf("%w: %s", err, strings.TrimSpace(stderr.String()))
	}
	return stdout.Bytes(), nil
}

type External struct {
	cfg        Config
	runner     CommandRunner
	httpClient *http.Client
}

func NewExternal(cfg Config) *External {
	return &External{cfg: cfg, runner: execRunner{}, httpClient: &http.Client{Timeout: cfg.MatrixTimeout}}
}

// cleanEnvironment drops the variables a child CLI must not inherit: pohunek
// session identity, which would bind the call to a foreign session, and the
// GitHub token, which only the gh child receives explicitly.
func cleanEnvironment(extra ...string) []string {
	environment := make([]string, 0, len(os.Environ())+len(extra))
	for _, item := range os.Environ() {
		if !strings.HasPrefix(item, "POHUNEK_SESSION_ID=") && !strings.HasPrefix(item, "POHUNEK_DAEMON_ID=") && !strings.HasPrefix(item, "GH_TOKEN=") {
			environment = append(environment, item)
		}
	}
	return append(environment, extra...)
}
