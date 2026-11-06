package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

func sha(character string) string { return strings.Repeat(character, 40) }

var (
	opus  = policy.Reviewer{ID: "opus", Label: "Opus", Protocol: policy.ProtocolClaudeStructured, AgentProfile: "claude-profile", FindingPrefix: "O"}
	codex = policy.Reviewer{ID: "codex", Label: "Codex", Protocol: policy.ProtocolCodexEnvelope, AgentProfile: "codex-profile", FindingPrefix: "X"}
)

func notificationLevels() map[policy.Event]policy.Level {
	levels := map[policy.Event]policy.Level{}
	for _, event := range policy.Events {
		levels[event] = policy.LevelLog
	}
	levels[policy.EventApprovalRequested] = policy.LevelAction
	levels[policy.EventFindingsOnOwnPR] = policy.LevelAction
	levels[policy.EventReviewStopped] = policy.LevelAction
	levels[policy.EventCleanupManual] = policy.LevelAction
	levels[policy.EventApproved] = policy.LevelDigest
	levels[policy.EventChangesRequested] = policy.LevelDigest
	levels[policy.EventCommented] = policy.LevelDigest
	return levels
}

func testRule(reviewer policy.Reviewer, onFindings policy.OnFindings, onClean policy.OnClean) policy.Rule {
	checkout := policy.CheckoutBase
	trigger := policy.Trigger{Kind: policy.TriggerReviewRequested}
	if reviewer.Protocol == policy.ProtocolCodexEnvelope {
		checkout, trigger = policy.CheckoutHead, policy.Trigger{Kind: policy.TriggerLabel, Label: "ai:review"}
	}
	return policy.Rule{
		Match: "acme/*", Trigger: trigger, OnFindings: onFindings, OnClean: onClean,
		Pipeline: policy.Pipeline{Name: "main", Protocol: reviewer.Protocol, Reviewers: []policy.Reviewer{reviewer}, IncrementalReviewers: []policy.Reviewer{reviewer}, Prompt: "Review the pull request.", Checkout: checkout, ProjectSources: []string{"manual"}},
	}
}

func testPullRequest(number int) PullRequest {
	return PullRequest{Repository: "acme/widget", Number: number, HeadSHA: sha("a"), ReviewRequestID: 7, Title: "Add widget", URL: "https://github.com/acme/widget/pull/" + strconv.Itoa(number), Author: "colleague", HeadBranch: "feature", BaseBranch: "main", BaseSHA: sha("b")}
}

func testInput(rule policy.Rule, pr PullRequest) ReviewInput {
	return ReviewInput{
		Version: inputVersion, PullRequest: pr, Rule: rule, Reviewers: rule.Pipeline.Reviewers, Scope: ReviewScope{Mode: scopeFull},
		Route: ProjectRoute{Host: "review-host", ProjectID: "project"}, Notifications: notificationLevels(), ApprovalTimeoutSeconds: 3600,
		PolicyDigest: strings.Repeat("d", 64), PublisherLogin: "reviewer",
		Limits: RunLimits{SessionTimeoutMS: 60000, OutputPollMS: 1000, SessionStartAttempts: 2, SessionStartRetryMS: 1000, SessionCleanupAttempts: 2, SessionCleanupRetryMS: 1000, ReviewerRunAttempts: 2, ReviewerRunRetryMS: 1000, StepRetryMS: 60000, StepRetryInitialMS: 10, StepRetryMaxIntervalMS: 1000},
	}
}

func testConfig() Config {
	return Config{
		GitHubHost: "github.com", GitHubLogin: "reviewer", GitHubBodyMaxBytes: 60000, AISignature: "🤖 Written by AI: test reviewer", MaxFindings: 50, MaxTests: 20,
		MinSummaryCharacters: 10, MaxSummaryCharacters: 20000, MaxTitleCharacters: 512, MaxBodyCharacters: 20000, MaxDiscussionCharacters: 40000, MaxPathCharacters: 4096,
		MaxTestCharacters: 1024, ResultMaxBytes: 1 << 20, ExcerptCharacters: 200, MatrixMaxCharacters: 8000,
		SessionTimeout: 60 * time.Second, OutputPollInterval: time.Second, SessionStartAttempts: 2, SessionStartRetry: time.Second,
		SessionCleanupAttempts: 2, SessionCleanupRetry: time.Second, ReviewerRunAttempts: 2, ReviewerRunRetry: time.Second,
		StepRetry: 60 * time.Second, StepRetryInitial: 10 * time.Millisecond, StepRetryMaxInterval: time.Second,
		ReviewListLimit: 50, MaxConcurrentReviews: 8, StateEntryLimit: 5000, SessionGroup: "autoreview",
	}
}

func TestPlanPublicationFollowsTheRuleAndGitHubOwnPullRequestLimit(t *testing.T) {
	finding := []Finding{{ID: "O001"}}
	cases := []struct {
		name       string
		onFindings policy.OnFindings
		onClean    policy.OnClean
		findings   []Finding
		author     string
		want       PublicationPlan
	}{
		{"findings request changes", policy.FindingsRequestChanges, policy.CleanApprove, finding, "colleague", PublicationPlan{Decision: DecisionRequestChanges}},
		{"findings comment", policy.FindingsComment, policy.CleanComment, finding, "colleague", PublicationPlan{Decision: DecisionComment}},
		{"clean approve", policy.FindingsRequestChanges, policy.CleanApprove, nil, "colleague", PublicationPlan{Decision: DecisionApprove}},
		{"clean await", policy.FindingsRequestChanges, policy.CleanAwait, nil, "colleague", PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"clean comment", policy.FindingsComment, policy.CleanComment, nil, "colleague", PublicationPlan{Decision: DecisionComment}},
		{"own pull request cannot be approved", policy.FindingsRequestChanges, policy.CleanAwait, nil, "reviewer", PublicationPlan{Decision: DecisionComment, OwnPullRequest: true}},
		{"own pull request cannot get changes requested", policy.FindingsRequestChanges, policy.CleanApprove, finding, "reviewer", PublicationPlan{Decision: DecisionComment, OwnPullRequest: true}},
	}
	for _, tc := range cases {
		pr := testPullRequest(1)
		pr.Author = tc.author
		plan := PlanPublication(testInput(testRule(opus, tc.onFindings, tc.onClean), pr), Artifact{Findings: tc.findings}, "reviewer")
		if plan != tc.want {
			t.Fatalf("%s: plan = %+v, want %+v", tc.name, plan, tc.want)
		}
	}
	if event := PublicationEvent(PublicationPlan{Decision: DecisionComment, OwnPullRequest: true}, 2); event != policy.EventFindingsOnOwnPR {
		t.Fatalf("findings on an own pull request need the user, got %s", event)
	}
	if event := PublicationEvent(PublicationPlan{Decision: DecisionRequestChanges}, 2); event != policy.EventChangesRequested {
		t.Fatalf("event = %s", event)
	}
}

// An approval without the user must rest on a full review or on an approved
// baseline: after a request for changes the next review is full again, unless
// the user approves.
func TestReviewScopeIsIncrementalOnlyAfterADecisionThatLeavesNothingOpen(t *testing.T) {
	approving := testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove)
	awaiting := testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait)
	commenting := testRule(codex, policy.FindingsComment, policy.CleanComment)
	previous := func(decision Decision) PublishedRecord {
		return PublishedRecord{HeadSHA: sha("c"), Decision: decision}
	}
	cases := []struct {
		name      string
		rule      policy.Rule
		published PublishedRecord
		found     bool
		head      string
		own       bool
		want      ReviewScope
	}{
		{"first review", approving, PublishedRecord{}, false, sha("a"), false, ReviewScope{Mode: scopeFull}},
		{"after approval", approving, previous(DecisionApprove), true, sha("a"), false, ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")}},
		{"after request changes", approving, previous(DecisionRequestChanges), true, sha("a"), false, ReviewScope{Mode: scopeFull}},
		{"after request changes on a rule the user approves", awaiting, previous(DecisionRequestChanges), true, sha("a"), false, ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")}},
		{"after comment on an approving rule", approving, previous(DecisionComment), true, sha("a"), false, ReviewScope{Mode: scopeFull}},
		{"own pull request is never approved", approving, previous(DecisionComment), true, sha("a"), true, ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")}},
		{"comment-only rule", commenting, previous(DecisionComment), true, sha("a"), false, ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")}},
		{"same head", approving, previous(DecisionApprove), true, sha("c"), false, ReviewScope{Mode: scopeFull}},
	}
	for _, tc := range cases {
		if got := reviewScope(tc.rule, tc.published, tc.found, tc.head, tc.own); got != tc.want {
			t.Fatalf("%s: scope = %+v, want %+v", tc.name, got, tc.want)
		}
	}
}

func TestAnchorFindingsMovesWithinTheFileAndKeepsOtherFilesInTheBody(t *testing.T) {
	artifact := Artifact{Findings: []Finding{
		{ID: "O001", Path: "src/a.go", Line: 10, Body: "exact"},
		{ID: "O002", Path: "src/a.go", Line: 14, Body: "near"},
		{ID: "O003", Path: "src/unchanged.go", Line: 3, Body: "elsewhere"},
	}}
	anchored := AnchorFindings(artifact, CommentableLines{"src/a.go": {10, 12, 20}})
	if !anchored.Findings[0].Anchored || anchored.Findings[0].Line != 10 || anchored.Findings[0].Body != "exact" {
		t.Fatalf("exact finding changed: %+v", anchored.Findings[0])
	}
	if !anchored.Findings[1].Anchored || anchored.Findings[1].Line != 12 || !strings.Contains(anchored.Findings[1].Body, "Reported at `src/a.go:14`") {
		t.Fatalf("near finding not moved to the closest changed line: %+v", anchored.Findings[1])
	}
	if anchored.Findings[2].Anchored || anchored.Findings[2].Line != 3 {
		t.Fatalf("finding outside the diff must stay in the body: %+v", anchored.Findings[2])
	}
}

func TestRenderReviewKeepsCodeIntactAndDefusesMentionsAndComments(t *testing.T) {
	rule := testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove)
	input := testInput(rule, testPullRequest(5))
	body := "Call `$client->send(@payload)` here:\n```php\n$map = ['a' => $b->c()]; // @param int $x\n```\nThanks @octocat, mail me at dev@example.com. <!-- hidden --> done"
	artifact := Artifact{
		Repository: "acme/widget", Number: 5, HeadSHA: sha("a"), Scope: ReviewScope{Mode: scopeFull},
		Summaries: []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: "Summary mentions @someone."}},
		Findings: []Finding{
			{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/a.go", Line: 3, Title: "Inline", Body: body, Anchored: true},
			{ID: "O002", ReviewerLabel: "Opus", Severity: "low", Confidence: "medium", Path: "src/b.go", Line: 9, Title: "Elsewhere", Body: "Outside the diff."},
		},
	}
	review, err := RenderReview(testConfig(), input, artifact, PublicationPlan{Decision: DecisionRequestChanges})
	if err != nil {
		t.Fatal(err)
	}
	if len(review.Comments) != 1 || review.Comments[0].Path != "src/a.go" || review.Comments[0].Line != 3 || review.Comments[0].Side != "RIGHT" {
		t.Fatalf("comments = %+v", review.Comments)
	}
	comment := review.Comments[0].Body
	if !strings.HasSuffix(comment, "\n\n"+testConfig().AISignature) {
		t.Fatalf("inline comment must end with the AI signature:\n%s", comment)
	}
	if !strings.HasSuffix(review.Body, "\n\n"+testConfig().AISignature) {
		t.Fatalf("review body must end with the AI signature:\n%s", review.Body)
	}
	for _, kept := range []string{"`$client->send(@payload)`", "$map = ['a' => $b->c()]; // @param int $x", "dev@example.com"} {
		if !strings.Contains(comment, kept) {
			t.Fatalf("code or plain text was altered, missing %q in:\n%s", kept, comment)
		}
	}
	if strings.Contains(comment, "Thanks @octocat") || !strings.Contains(comment, "Thanks @\u200boctocat") {
		t.Fatalf("mention outside code was not defused:\n%s", comment)
	}
	if strings.Contains(comment, "<!--") {
		t.Fatalf("an HTML comment survived:\n%s", comment)
	}
	if !strings.HasPrefix(review.Body, review.Marker) || strings.Count(review.Body, "<!--") != 1 || !strings.Contains(review.Body, "`src/b.go:9`") || strings.Contains(review.Body, "@someone") {
		t.Fatalf("body = %s", review.Body)
	}
	largest := max(len(review.Body), len(comment))
	cfg := testConfig()
	cfg.GitHubBodyMaxBytes = largest - 1
	if _, err := RenderReview(cfg, input, artifact, PublicationPlan{Decision: DecisionRequestChanges}); errorCode(err) != "github_review_too_large" {
		t.Fatalf("an oversized field must be rejected, got %v", err)
	}
	cfg.GitHubBodyMaxBytes = largest
	if largest >= len(review.Body)+len(comment) {
		t.Fatal("the fixture must make the sum exceed the per-field limit")
	}
	if _, err := RenderReview(cfg, input, artifact, PublicationPlan{Decision: DecisionRequestChanges}); err != nil {
		t.Fatalf("each field is limited on its own, not their sum: %v", err)
	}
}

func TestNeutralizeFollowsGitHubFencesAndBlocksImagesAndHTML(t *testing.T) {
	cases := map[string]string{
		"    ```\n@victim":                    "    ```\n@\u200bvictim",
		"```a`b\n@victim":                     "```a`b\n@\u200bvictim",
		"```go\nx := @y\n```\n@after":         "```go\nx := @y\n```\n@\u200bafter",
		"~~~~\n@kept\n~~~\n@kept too\n~~~~":   "~~~~\n@kept\n~~~\n@kept too\n~~~~",
		"see ![x](https://example.org/a.png)": "see !\\[x](https://example.org/a.png)",
		"<img src=x> and `<b>`":               "&lt;img src=x> and `<b>`",
	}
	for input, want := range cases {
		if got := neutralize(input); got != want {
			t.Fatalf("neutralize(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestReviewMarkerDependsOnDecisionAndFindings(t *testing.T) {
	pr := testPullRequest(5)
	approve := reviewMarker(pr, DecisionApprove, nil, nil)
	changes := reviewMarker(pr, DecisionRequestChanges, []Finding{{ID: "O001"}}, nil)
	if approve == changes || !strings.HasPrefix(approve, "<!-- hermes-pr-review:v1:acme/widget#5@") {
		t.Fatalf("markers %q and %q", approve, changes)
	}
	if !strings.Contains(approve, ":decision=APPROVE:findings=0:digest=") || !strings.Contains(changes, ":decision=REQUEST_CHANGES:findings=1:digest=") {
		t.Fatalf("markers must carry the decision and the finding count: %q and %q", approve, changes)
	}
	cleanComment := reviewMarker(pr, DecisionComment, nil, nil)
	commentWithFindings := reviewMarker(pr, DecisionComment, []Finding{{ID: "O001"}, {ID: "S001"}}, nil)
	if !strings.Contains(cleanComment, ":decision=COMMENT:findings=0:") || !strings.Contains(commentWithFindings, ":decision=COMMENT:findings=2:") {
		t.Fatalf("a COMMENT on an own pull request must still tell clean from findings: %q and %q", cleanComment, commentWithFindings)
	}
}

func TestReviewerRequestsCarryTheTrustedContextTheirPromptRefersTo(t *testing.T) {
	pr := testPullRequest(9)
	claudeInput := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), pr)
	raw, err := BuildReviewerRequest(claudeInput, opus, Discussion{})
	if err != nil {
		t.Fatal(err)
	}
	var request struct {
		Protocol string          `json:"protocol"`
		Prompt   string          `json:"prompt"`
		Schema   json.RawMessage `json:"json_schema"`
	}
	if err := json.Unmarshal([]byte(raw), &request); err != nil || request.Protocol != claudeRequestProtocol || len(request.Schema) == 0 ||
		!strings.Contains(request.Prompt, "Trusted request context:\nRepository: acme/widget\nPull request: 9\nHead SHA: "+pr.HeadSHA) {
		t.Fatalf("claude request = %s (%v)", raw, err)
	}
	codexInput := testInput(testRule(codex, policy.FindingsComment, policy.CleanComment), pr)
	codexInput.Scope = ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")}
	prompt, err := BuildReviewerRequest(codexInput, codex, Discussion{})
	if err != nil || !strings.Contains(prompt, "Trusted review context:\n- Repository: acme/widget\n- Pull request: #9") ||
		!strings.Contains(prompt, "- Trusted base SHA: "+pr.BaseSHA) || !strings.HasSuffix(prompt, "- Trusted previously reviewed head SHA: "+sha("c")) {
		t.Fatalf("codex prompt = %s (%v)", prompt, err)
	}
}

func claudeOutput(t *testing.T, pr PullRequest, findings []map[string]any) []byte {
	t.Helper()
	return claudeOutputWithPrior(t, pr, findings, nil)
}

func claudeOutputWithPrior(t *testing.T, pr PullRequest, findings []map[string]any, prior []map[string]any) []byte {
	t.Helper()
	structured := map[string]any{"repository": pr.Repository, "number": pr.Number, "head_sha": pr.HeadSHA, "summary": "Reviewed the change thoroughly and found the listed issues.", "findings": findings}
	if prior != nil {
		structured["prior_findings"] = prior
	}
	return claudeStructuredOutput(structured)
}

// claudeStructuredOutput returns the output of a finished claude session with
// this structured output.
func claudeStructuredOutput(structured any) []byte {
	outer, _ := json.Marshal(map[string]any{"type": "result", "subtype": "success", "is_error": false, "result": "Prose summary.", "structured_output": structured})
	wrapper, _ := json.Marshal(map[string]any{"protocol": claudeResultProtocol, "mode": "structured", "exit_code": 0, "stdout_base64": base64.StdEncoding.EncodeToString(outer), "stderr_base64": ""})
	return append(wrapper, '\n')
}

func codexOutput(t *testing.T, pr PullRequest, findings []map[string]any, status int) []byte {
	t.Helper()
	if findings == nil {
		findings = []map[string]any{}
	}
	report, _ := json.Marshal(map[string]any{"findings": findings, "summary": "Full review of the pull request diff; details follow.", "tests": []string{"go test ./..."}, "status": "completed", "reviewed_head_sha": pr.HeadSHA})
	return []byte("[pohunek-codex-review-result-v1:" + base64.StdEncoding.EncodeToString(report) + "]\n[pohunek-codex-review exited with status " + strconv.Itoa(status) + "]\n")
}

func TestParseReviewerOutputNormalizesBothProtocols(t *testing.T) {
	cfg := testConfig()
	pr := testPullRequest(3)
	claude, err := ParseReviewerOutput(cfg, ReviewInput{PullRequest: pr}, Discussion{}, opus, claudeOutput(t, pr, []map[string]any{{"id": "C001", "severity": "high", "confidence": "high", "path": "a.go", "line": 4, "side": "RIGHT", "title": "Bug", "body": "Details."}}))
	if err != nil || claude.ReviewerID != "opus" || len(claude.Findings) != 1 || claude.Findings[0].SourceID != "C001" {
		t.Fatalf("claude result = %+v, %v", claude, err)
	}
	result, err := ParseReviewerOutput(cfg, ReviewInput{PullRequest: pr}, Discussion{}, codex, codexOutput(t, pr, []map[string]any{
		{"severity": "low", "confidence": "low", "path": "z.go", "line": 1, "title": "Minor", "body": "Minor detail."},
		{"severity": "blocker", "confidence": "high", "path": "a.go", "line": 2, "title": "Blocker", "body": "Breaks the build."},
	}, 0))
	if err != nil || len(result.Findings) != 2 || result.Findings[0].Severity != "blocker" || result.Findings[0].SourceID != "C001" || result.Findings[1].SourceID != "C002" || len(result.Tests) != 1 {
		t.Fatalf("codex result = %+v, %v", result, err)
	}
	if _, err := ParseReviewerOutput(cfg, ReviewInput{PullRequest: pr}, Discussion{}, codex, codexOutput(t, pr, nil, 1)); !reviewerOutputRetryable(err) {
		t.Fatalf("a failed Codex run must be retryable, got %v", err)
	}
	other := pr
	other.HeadSHA = sha("e")
	if _, err := ParseReviewerOutput(cfg, ReviewInput{PullRequest: other}, Discussion{}, opus, claudeOutput(t, pr, []map[string]any{})); errorCode(err) != "review_output_invalid" || !reviewerOutputRetryable(err) {
		t.Fatalf("a result for another head must get a fresh run, got %v", err)
	}
	if _, err := ParseReviewerOutput(cfg, ReviewInput{PullRequest: pr}, Discussion{}, opus, claudeOutput(t, pr, []map[string]any{{"id": "C001", "severity": "high", "confidence": "high", "path": "../etc/passwd", "line": 1, "side": "RIGHT", "title": "x", "body": "y"}})); errorCode(err) != "review_output_invalid" || !reviewerOutputRetryable(err) {
		t.Fatalf("a traversing path must get a fresh run, got %v", err)
	}
	if !outputComplete(policy.ProtocolCodexEnvelope, codexOutput(t, pr, nil, 0)) || outputComplete(policy.ProtocolCodexEnvelope, []byte("still running")) {
		t.Fatal("Codex output completion is detected by the exit line")
	}
}

func TestCombineResultsPrefixesFindingsPerReviewer(t *testing.T) {
	rule := testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove)
	sonnet := opus
	sonnet.ID, sonnet.Label, sonnet.FindingPrefix = "sonnet", "Sonnet", "S"
	rule.Pipeline.Reviewers = []policy.Reviewer{opus, sonnet}
	input := testInput(rule, testPullRequest(4))
	artifact, err := CombineResults(testConfig(), input, []ReviewerResult{
		{ReviewerID: "opus", Summary: "opus summary", Findings: []Finding{{SourceID: "C001", Title: "a"}}},
		{ReviewerID: "sonnet", Summary: "sonnet summary", Findings: []Finding{{SourceID: "C001", Title: "b"}}},
	}, time.Unix(0, 0))
	if err != nil || len(artifact.Findings) != 2 || artifact.Findings[0].ID != "O001" || artifact.Findings[1].ID != "S001" || artifact.Findings[1].ReviewerLabel != "Sonnet" || len(artifact.Summaries) != 2 {
		t.Fatalf("artifact = %+v, %v", artifact, err)
	}
	if _, err := CombineResults(testConfig(), input, []ReviewerResult{{ReviewerID: "sonnet"}, {ReviewerID: "opus"}}, time.Unix(0, 0)); err == nil {
		t.Fatal("results out of reviewer order must be rejected")
	}
}

func TestValidateInputRejectsInconsistentInput(t *testing.T) {
	valid := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), testPullRequest(2))
	if err := validateInput(valid); err != nil {
		t.Fatal(err)
	}
	mutations := map[string]func(*ReviewInput){
		"version":          func(input *ReviewInput) { input.Version = 1 },
		"foreign reviewer": func(input *ReviewInput) { input.Reviewers = []policy.Reviewer{codex} },
		"incremental head": func(input *ReviewInput) {
			input.Scope = ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: input.PullRequest.HeadSHA}
		},
		"missing level":      func(input *ReviewInput) { delete(input.Notifications, policy.EventSuperseded) },
		"no approval window": func(input *ReviewInput) { input.ApprovalTimeoutSeconds = 0 },
		"no limits":          func(input *ReviewInput) { input.Limits = RunLimits{} },
	}
	for name, mutate := range mutations {
		input := valid
		input.Notifications = notificationLevels()
		mutate(&input)
		if validateInput(input) == nil {
			t.Fatalf("%s: invalid input accepted", name)
		}
	}
}

func TestCommentableLinesFromPatch(t *testing.T) {
	lines, err := commentableLinesFromPatch("@@ -1,3 +1,4 @@\n context\n-removed\n+added\n+added too\n context\n\\ No newline at end of file")
	if err != nil || len(lines) != 2 || lines[0] != 2 || lines[1] != 3 {
		t.Fatalf("lines = %v, %v", lines, err)
	}
	if lines, err := commentableLinesFromPatch("@@ -1,2 +0,0 @@\n-a\n-b"); err != nil || len(lines) != 0 {
		t.Fatalf("a deleted file has no anchors: %v, %v", lines, err)
	}
	if _, err := commentableLinesFromPatch("@@ broken @@"); err == nil {
		t.Fatal("a malformed hunk header must be rejected")
	}
}

// pohunekStartRunner answers every pohunek call with an empty session list and
// records the session new call.
type pohunekStartRunner struct{ started []string }

func (runner *pohunekStartRunner) Run(_ context.Context, _ string, args []string, _ []byte, _ []string) ([]byte, error) {
	if slices.Contains(args, "new") {
		runner.started = args
	}
	return []byte(`{"ok":[]}`), nil
}

func TestReviewSessionsStartInTheCompanionGroup(t *testing.T) {
	runner := &pohunekStartRunner{}
	cfg := testConfig()
	cfg.PohunekTimeout = time.Second
	request := SessionRequest{PullRequest: testPullRequest(1), Route: ProjectRoute{Host: "host", ProjectID: "project"}, Reviewer: opus, WorkflowID: "prr-v1-abc", Run: 1, Checkout: policy.CheckoutBase}
	_, err := (&External{cfg: cfg, runner: runner}).EnsureSession(context.Background(), request)
	if err == nil {
		t.Fatal("a start that lists no session afterwards must fail")
	}
	if !strings.Contains(strings.Join(runner.started, " "), "--meta companion.group=autoreview") {
		t.Fatalf("session new args = %q, err = %v", runner.started, err)
	}
}

func TestSessionRequestNamesAreUniquePerReviewerRun(t *testing.T) {
	request := SessionRequest{PullRequest: testPullRequest(1), Reviewer: opus, WorkflowID: "prr-v1-abc", Run: 1, Checkout: policy.CheckoutBase}
	retry := request
	retry.Run = 2
	if request.Name() == retry.Name() || request.Branch() == retry.Branch() || request.BaseRef() != "main" {
		t.Fatalf("names %s / %s, base %s", request.Name(), retry.Name(), request.BaseRef())
	}
	request.Checkout = policy.CheckoutHead
	if request.BaseRef() != "refs/pull/1/head" {
		t.Fatalf("head checkout base = %s", request.BaseRef())
	}
}
