package main

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

// parityGoldenDir is relative to the package directory, which go test uses as
// its working directory.
var parityGoldenDir = filepath.Join("..", "..", "testdata", "parity")

const parityWriteEnv = "AUTOREVIEW_WRITE_GOLDEN"

// parityCase is one golden case. Output holds the Go value; Error holds the
// errorCode of a failed call. RustExpectedError is set only on the intentional
// parsing divergences and names the Rust error code proposed for them.
type parityCase struct {
	Name              string `json:"name"`
	Input             any    `json:"input"`
	Output            any    `json:"output,omitempty"`
	Error             string `json:"error,omitempty"`
	RustExpectedError string `json:"rust_expected_error,omitempty"`
}

func parityOK(name string, input, output any) parityCase {
	return parityCase{Name: name, Input: input, Output: output}
}

// parityFailure records the errorCode of err. A failure without a coded
// WorkflowError is a fixture mistake, so it fails the test.
func parityFailure(t *testing.T, name string, input any, err error, rustExpected string) parityCase {
	t.Helper()
	if err == nil {
		t.Fatalf("case %s: want an error, got none", name)
	}
	code := errorCode(err)
	if code == "workflow_failed" {
		t.Fatalf("case %s: error %v has no code", name, err)
	}
	return parityCase{Name: name, Input: input, Error: code, RustExpectedError: rustExpected}
}

// parityEncode uses the same encoder settings for every golden: no HTML
// escaping, two-space indent, and the single trailing newline Encode appends.
func parityEncode(t *testing.T, cases []parityCase) []byte {
	t.Helper()
	var buffer bytes.Buffer
	encoder := json.NewEncoder(&buffer)
	encoder.SetEscapeHTML(false)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(cases); err != nil {
		t.Fatal(err)
	}
	return buffer.Bytes()
}

// parityCheckGolden writes testdata/parity/<name>.json when AUTOREVIEW_WRITE_GOLDEN=1.
// Otherwise it fails unless the stored file is byte-identical to fresh output.
func parityCheckGolden(t *testing.T, name string, cases []parityCase) {
	t.Helper()
	fresh := parityEncode(t, cases)
	path := filepath.Join(parityGoldenDir, name+".json")
	if os.Getenv(parityWriteEnv) == "1" {
		if err := os.MkdirAll(parityGoldenDir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, fresh, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	stored, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("golden %s cannot be read (%v); write it with %s=1", path, err, parityWriteEnv)
	}
	if !bytes.Equal(stored, fresh) {
		t.Fatalf("golden %s differs from the Go output; review the change and rewrite it with %s=1", path, parityWriteEnv)
	}
}

func TestWriteParityGolden(t *testing.T) {
	files := []struct {
		name  string
		cases func(*testing.T) []parityCase
	}{
		{"ids", parityIDsCases},
		{"marker", parityMarkerCases},
		{"timestamps", parityTimestampCases},
		{"digest", parityDigestCases},
		{"neutralize", parityNeutralizeCases},
		{"render", parityRenderCases},
		{"protocol", parityProtocolCases},
	}
	for _, file := range files {
		t.Run(file.name, func(t *testing.T) {
			parityCheckGolden(t, file.name, file.cases(t))
		})
	}
}

func paritySessionNames(request SessionRequest) map[string]string {
	return map[string]string{"name": request.Name(), "branch": request.Branch(), "base_ref": request.BaseRef()}
}

func parityIDsCases(t *testing.T) []parityCase {
	pr := testPullRequest(5)
	return []parityCase{
		parityOK("pull_request_key", pr, pr.Key()),
		parityOK("review_id", pr, pr.ReviewID()),
		parityOK("workflow_id_attempt_1", map[string]any{"pull_request": pr, "attempt": 1}, pr.WorkflowID(1)),
		parityOK("workflow_id_attempt_2", map[string]any{"pull_request": pr, "attempt": 2}, pr.WorkflowID(2)),
		parityOK("prior_key_full_sha", map[string]string{"head": sha("a"), "id": "O001"}, priorKey(sha("a"), "O001")),
		parityOK("prior_key_short_head", map[string]string{"head": "abc", "id": "S002"}, priorKey("abc", "S002")),
		parityOK("target_key_and_identity", pr, map[string]string{"key": pr.Target().Key(), "identity": pr.Target().Identity()}),
		parityOK("published_record_key", map[string]any{"repository": "acme/widget", "number": 5}, PublishedRecord{Repository: "acme/widget", Number: 5}.Key()),
		parityOK("await_record_key", map[string]any{"repository": "acme/widget", "number": 5}, AwaitRecord{Repository: "acme/widget", Number: 5}.Key()),
		parityOK("session_run1_base", SessionRequest{PullRequest: pr, Reviewer: opus, Checkout: policy.CheckoutBase, WorkflowID: pr.WorkflowID(1), Run: 1},
			paritySessionNames(SessionRequest{PullRequest: pr, Reviewer: opus, Checkout: policy.CheckoutBase, WorkflowID: pr.WorkflowID(1), Run: 1})),
		parityOK("session_run1_start2", SessionRequest{PullRequest: pr, Reviewer: opus, Checkout: policy.CheckoutBase, WorkflowID: pr.WorkflowID(1), Run: 1, Start: 2},
			paritySessionNames(SessionRequest{PullRequest: pr, Reviewer: opus, Checkout: policy.CheckoutBase, WorkflowID: pr.WorkflowID(1), Run: 1, Start: 2})),
		parityOK("session_run2_head_checkout", SessionRequest{PullRequest: pr, Reviewer: codex, Checkout: policy.CheckoutHead, WorkflowID: pr.WorkflowID(2), Run: 2},
			paritySessionNames(SessionRequest{PullRequest: pr, Reviewer: codex, Checkout: policy.CheckoutHead, WorkflowID: pr.WorkflowID(2), Run: 2})),
	}
}

func parityMarkerCase(name string, pr PullRequest, decision Decision, findings []Finding, open []PriorResult) parityCase {
	return parityOK(name, map[string]any{"pull_request": pr, "decision": decision, "findings": findings, "open_prior": open}, reviewMarker(pr, decision, findings, open))
}

func parityMarkerCases(t *testing.T) []parityCase {
	pr := testPullRequest(5)
	moved := testPullRequest(5)
	moved.HeadSHA = sha("c")
	openPrior := []PriorResult{{Finding: PriorFinding{Key: priorKey(sha("c"), "O001")}, PriorVerdict: PriorVerdict{Status: priorOpen}}}
	return []parityCase{
		parityMarkerCase("approve_without_findings", pr, DecisionApprove, nil, nil),
		parityMarkerCase("request_changes_one_finding", pr, DecisionRequestChanges, []Finding{{ID: "O001"}}, nil),
		parityMarkerCase("comment_two_findings", pr, DecisionComment, []Finding{{ID: "O001"}, {ID: "S001"}}, nil),
		parityMarkerCase("comment_two_findings_reversed", pr, DecisionComment, []Finding{{ID: "S001"}, {ID: "O001"}}, nil),
		parityMarkerCase("open_prior_counted_by_key", pr, DecisionRequestChanges, nil, openPrior),
		parityMarkerCase("finding_and_open_prior", pr, DecisionComment, []Finding{{ID: "S001"}}, openPrior),
		parityMarkerCase("other_head_sha", moved, DecisionApprove, nil, nil),
	}
}

func parityStoreTimestamp(instant time.Time) string {
	store := &Store{now: func() time.Time { return instant }}
	return store.timestamp()
}

// parityReviewedAt returns the ReviewedAt that CombineResults writes for instant.
func parityReviewedAt(t *testing.T, instant time.Time) string {
	t.Helper()
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), testPullRequest(5))
	results := []ReviewerResult{{ReviewerID: "opus", Summary: "Reviewed the change thoroughly.", Findings: []Finding{}, Tests: []string{}}}
	artifact, err := CombineResults(testConfig(), input, results, instant)
	if err != nil {
		t.Fatal(err)
	}
	return artifact.ReviewedAt
}

func parityTimestampCase(t *testing.T, name string, input any, instant time.Time) parityCase {
	return parityOK(name, input, map[string]any{
		"store_timestamp":  parityStoreTimestamp(instant),
		"reviewed_at":      parityReviewedAt(t, instant),
		"published_record": PublishedRecord{PublishedAt: instant},
	})
}

func parityTimestampCases(t *testing.T) []parityCase {
	const base = int64(1767225600) // 2026-01-01T00:00:00Z
	return []parityCase{
		parityTimestampCase(t, "nanos_0", map[string]int64{"unix_seconds": base, "nanos": 0}, time.Unix(base, 0).UTC()),
		parityTimestampCase(t, "nanos_500000000", map[string]int64{"unix_seconds": base, "nanos": 500000000}, time.Unix(base, 500000000).UTC()),
		parityTimestampCase(t, "nanos_123456789", map[string]int64{"unix_seconds": base, "nanos": 123456789}, time.Unix(base, 123456789).UTC()),
		parityTimestampCase(t, "nanos_100", map[string]int64{"unix_seconds": base, "nanos": 100}, time.Unix(base, 100).UTC()),
		parityTimestampCase(t, "zero_time", map[string]bool{"zero_time": true}, time.Time{}),
	}
}

// parityTestdata holds the policy fixture and its prompts.
var parityTestdata = filepath.Join("..", "..", "testdata")

var parityPolicyPath = filepath.Join(parityTestdata, "review-policy.yaml")

// parityDigest parses the policy fixture after applying each replacement once,
// in order, and with the prompt contents of overrides keyed by the relative
// path the document names.
func parityDigest(t *testing.T, replacements [][2]string, overrides map[string]string) string {
	t.Helper()
	data, err := os.ReadFile(parityPolicyPath)
	if err != nil {
		t.Fatal(err)
	}
	document := string(data)
	for _, pair := range replacements {
		if !strings.Contains(document, pair[0]) {
			t.Fatalf("replacement target %q is not in the policy fixture", pair[0])
		}
		document = strings.Replace(document, pair[0], pair[1], 1)
	}
	readPrompt := func(relative string) ([]byte, error) {
		if content, ok := overrides[relative]; ok {
			return []byte(content), nil
		}
		return os.ReadFile(filepath.Join(parityTestdata, filepath.FromSlash(relative)))
	}
	parsed, err := policy.Parse([]byte(document), readPrompt)
	if err != nil {
		t.Fatal(err)
	}
	return parsed.Digest()
}

func parityDigestCase(t *testing.T, name string, replacements [][2]string, overrides map[string]string) parityCase {
	input := map[string]any{"policy_file": "testdata/review-policy.yaml", "replacements": replacements, "prompt_overrides": overrides}
	return parityOK(name, input, map[string]string{"digest": parityDigest(t, replacements, overrides)})
}

func parityDigestCases(t *testing.T) []parityCase {
	loaded, err := policy.Load(parityPolicyPath)
	if err != nil {
		t.Fatal(err)
	}
	return []parityCase{
		parityOK("fixture_via_load", map[string]any{"policy_file": "testdata/review-policy.yaml", "loader": "policy.Load"}, map[string]string{"digest": loaded.Digest()}),
		parityDigestCase(t, "fixture_via_parse", nil, nil),
		parityDigestCase(t, "approval_timeout_24_hours", [][2]string{{"timeout_hours: 168", "timeout_hours: 24"}}, nil),
		parityDigestCase(t, "escalate_count_2", [][2]string{{"escalate_count: 1", "escalate_count: 2"}}, nil),
		parityDigestCase(t, "approved_notification_action", [][2]string{{"approved: digest", "approved: action"}}, nil),
		parityDigestCase(t, "ready_draft_authors_cleared", [][2]string{{"ready_draft_authors: [devin-ai-integration]", "ready_draft_authors: []"}}, nil),
		parityDigestCase(t, "human_review_label_changed", [][2]string{{"labels: [needs-human-review]", "labels: [needs-review]"}}, nil),
		parityDigestCase(t, "strict_prompt_content_changed", nil, map[string]string{"prompts/strict.md": "Review strictly. This prompt differs from the fixture.\n"}),
	}
}

func parityNeutralize(name, text string) parityCase {
	return parityOK(name, text, neutralize(text))
}

func parityNeutralizeCases(*testing.T) []parityCase {
	return []parityCase{
		parityNeutralize("indented_fence_is_code", "    ```\n@victim"),
		parityNeutralize("fence_info_with_backtick_is_text", "```a`b\n@victim"),
		parityNeutralize("mention_after_closed_fence", "```go\nx := @y\n```\n@after"),
		parityNeutralize("tilde_fence_closed_by_tildes_only", "~~~~\n@kept\n~~~\n@kept too\n~~~~"),
		parityNeutralize("image_outside_code_defused", "see ![x](https://example.org/a.png)"),
		parityNeutralize("html_outside_code_defused", "<img src=x> and `<b>`"),
		parityNeutralize("comment_delimiters_inside_fence", "```\n<!-- hidden -->\n```\n<!-- out -->"),
		parityNeutralize("address_kept_mention_defused", "mail dev@example.com and @octocat"),
		parityNeutralize("multibyte_before_mention", "é@octocat"),
		parityNeutralize("at_after_letter_kept", "a@b"),
		parityNeutralize("at_underscore_not_mention", "@_x"),
		parityNeutralize("unclosed_backtick", "a `b @octocat"),
		parityNeutralize("mention_inside_code_span_kept", "`@octocat` and @octocat"),
	}
}

// parityRender renders the review. A render error is recorded as its code.
func parityRender(t *testing.T, name string, cfg Config, input ReviewInput, artifact Artifact, plan PublicationPlan) parityCase {
	t.Helper()
	fixture := map[string]any{"config": cfg, "input": input, "artifact": artifact, "plan": plan}
	review, err := RenderReview(cfg, input, artifact, plan)
	if err != nil {
		return parityFailure(t, name, fixture, err, "")
	}
	return parityOK(name, fixture, review)
}

func parityRenderCases(t *testing.T) []parityCase {
	cfg := testConfig()
	pr := testPullRequest(5)
	requestChanges := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), pr)
	notesRule := testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait)
	notesRule.OnNotes = policy.NotesComment
	notesInput := testInput(notesRule, pr)
	summary := []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: "Reviewed the change."}}
	body := "Call `$client->send(@payload)` here:\n```php\n$map = ['a' => $b->c()]; // @param int $x\n```\nThanks @octocat, mail me at dev@example.com. <!-- hidden --> done"
	mixed := Artifact{
		Repository: "acme/widget", Number: 5, HeadSHA: sha("a"), Scope: ReviewScope{Mode: scopeFull},
		Summaries: []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: "Summary mentions @someone."}},
		Findings: []Finding{
			{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/a.go", Line: 3, Title: "Inline", Body: body, Anchored: true},
			{ID: "O002", ReviewerLabel: "Opus", Severity: "low", Confidence: "medium", Path: "src/b.go", Line: 9, Title: "Elsewhere", Body: "Outside the diff."},
		},
	}
	clean := Artifact{Repository: "acme/widget", Number: 5, HeadSHA: sha("a"), Scope: ReviewScope{Mode: scopeFull}, Summaries: summary, Findings: []Finding{}}
	notes := clean
	notes.Findings = []Finding{{ID: "O001", ReviewerLabel: "Opus", Severity: "low", Confidence: "high", Path: "src/a.go", Line: 4, Title: "Naming", Body: "Rename it:\n```go\nfunc better() {", Anchored: true, NonBlocking: true}}
	outside := clean
	outside.Findings = []Finding{{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/c.go", Line: 12, Title: "Outside", Body: "Outside the diff."}}
	incremental := clean
	incremental.Scope = ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")}
	incremental.Findings = []Finding{{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/a.go", Line: 3, Title: "Bug", Body: "Breaks.", Anchored: true}}
	incremental.Verification = &Verification{ReviewerLabel: "Opus", Findings: []FindingCheck{{ID: "O001", Verdict: verdictConfirmed, Note: "Reachable."}}}
	priorOnly := clean
	priorOnly.PriorResults = []PriorResult{{Finding: PriorFinding{Key: priorKey(sha("c"), "O001"), ID: "O001", HeadSHA: sha("c"), Severity: "high", Path: "src/a.go", Line: 3, Title: "Bug"}, PriorVerdict: PriorVerdict{Status: priorOpen, Note: "Still there."}}}
	ownPR := clean
	ownPR.Findings = []Finding{{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/a.go", Line: 3, Title: "Bug", Body: "Breaks.", Anchored: true}}
	human := clean
	human.HumanReview = []string{"label needs-human-review"}
	verifierFailed := clean
	verifierFailed.Verification = &Verification{ReviewerLabel: "Opus", ErrorCode: "review_output_invalid"}
	small := cfg
	small.GitHubBodyMaxBytes = 10
	return []parityCase{
		parityRender(t, "mixed_inline_and_outside", cfg, requestChanges, mixed, PublicationPlan{Decision: DecisionRequestChanges}),
		parityRender(t, "clean_approve", cfg, requestChanges, clean, PublicationPlan{Decision: DecisionApprove}),
		parityRender(t, "notes_handed_to_author", cfg, notesInput, notes, PublicationPlan{Decision: DecisionComment, NotesToAuthor: true}),
		parityRender(t, "blocking_outside_changed_lines", cfg, requestChanges, outside, PublicationPlan{Decision: DecisionRequestChanges}),
		parityRender(t, "incremental_with_verification", cfg, requestChanges, incremental, PublicationPlan{Decision: DecisionRequestChanges}),
		parityRender(t, "open_prior_only", cfg, requestChanges, priorOnly, PublicationPlan{Decision: DecisionRequestChanges}),
		parityRender(t, "own_pull_request_findings", cfg, requestChanges, ownPR, PublicationPlan{Decision: DecisionComment, OwnPullRequest: true}),
		parityRender(t, "human_review_waited", cfg, requestChanges, human, PublicationPlan{Decision: DecisionComment}),
		parityRender(t, "verifier_error_code", cfg, requestChanges, verifierFailed, PublicationPlan{Decision: DecisionApprove}),
		parityRender(t, "body_over_limit", small, requestChanges, mixed, PublicationPlan{Decision: DecisionRequestChanges}),
	}
}

// parityWrap returns the claude-structured wrapper around the stdout of a finished session.
func parityWrap(stdout string) string {
	wrapper, _ := json.Marshal(map[string]any{"protocol": claudeResultProtocol, "mode": "structured", "exit_code": 0, "stdout_base64": base64.StdEncoding.EncodeToString([]byte(stdout)), "stderr_base64": ""})
	return string(wrapper) + "\n"
}

// parityStdout returns the stdout of a claude session with the prose result and the structured output JSON text.
func parityStdout(result, structured string) string {
	return `{"type":"result","subtype":"success","is_error":false,"result":` + strconv.Quote(result) + `,"structured_output":` + structured + `}`
}

// parityReviewJSON is a reviewer's structured output for the pull request with head and the findings JSON text.
func parityReviewJSON(head, findings string) string {
	return `{"repository":"acme/widget","number":5,"head_sha":"` + head + `","summary":"Reviewed the change thoroughly and found the listed issues.","findings":` + findings + `}`
}

// parityCodexRaw returns the codex envelope around the report JSON text.
func parityCodexRaw(report string) string {
	return "[pohunek-codex-review-result-v1:" + base64.StdEncoding.EncodeToString([]byte(report)) + "]\n[pohunek-codex-review exited with status 0]\n"
}

// parityCodexReport fills the head of a codex report literal.
func parityCodexReport(report, head string) string {
	return strings.ReplaceAll(report, "HEADSHA", head)
}

const parityCodexFindingsReport = `{"findings":[],"summary":"Full review of the pull request diff; details follow.","tests":["go test ./..."],"status":"completed","reviewed_head_sha":"HEADSHA"}`

// parityReviewerCase parses a reviewer output for testPullRequest(5). rust is the
// Rust error code proposed for an intentional parsing divergence, empty otherwise.
func parityReviewerCase(t *testing.T, name string, reviewer policy.Reviewer, raw, rust string) parityCase {
	t.Helper()
	input := ReviewInput{PullRequest: testPullRequest(5)}
	fixture := map[string]any{"config": testConfig(), "input": input, "discussion": Discussion{}, "reviewer": reviewer, "raw": raw}
	result, err := ParseReviewerOutput(testConfig(), input, Discussion{}, reviewer, []byte(raw))
	var c parityCase
	if err != nil {
		c = parityFailure(t, name, fixture, err, "")
	} else {
		c = parityOK(name, fixture, result)
	}
	c.RustExpectedError = rust
	return c
}

// parityVerifierCase parses a verifier output for testPullRequest(5) with one
// blocking finding pair and one prior finding. rust is as in parityReviewerCase.
func parityVerifierCase(t *testing.T, name, raw, rust string) parityCase {
	t.Helper()
	pr := testPullRequest(5)
	input := testInput(verifiedRule(policy.CleanApprove), pr)
	claims := verificationClaims{Findings: []claimFinding{{ID: "O001"}, {ID: "S001"}}, Prior: []claimPrior{{Key: priorKey(sha("c"), "O001")}}}
	fixture := map[string]any{"config": testConfig(), "input": input, "claims": claims, "raw": raw}
	result, err := ParseVerifierOutput(testConfig(), input, claims, []byte(raw))
	var c parityCase
	if err != nil {
		c = parityFailure(t, name, fixture, err, "")
	} else {
		c = parityOK(name, fixture, result)
	}
	c.RustExpectedError = rust
	return c
}

func parityProtocolCases(t *testing.T) []parityCase {
	validFinding := `[{"id":"C001","severity":"high","confidence":"high","path":"a.go","line":4,"side":"RIGHT","title":"Bug","body":"Details."}]`
	validStdout := parityStdout("Prose summary.", parityReviewJSON(sha("a"), validFinding))
	caseVariantWrapper := strings.NewReplacer(`"protocol"`, `"Protocol"`, `"mode"`, `"Mode"`, `"exit_code"`, `"EXIT_CODE"`,
		`"stdout_base64"`, `"Stdout_Base64"`, `"stderr_base64"`, `"Stderr_Base64"`).Replace(parityWrap(validStdout))
	verifierStdout := func(findings, priorFindings string) string {
		return `{"repository":"acme/widget","number":5,"head_sha":"` + sha("a") + `","findings":` + findings + `,"prior_findings":` + priorFindings + `}`
	}
	verifierPrior := `[{"key":"cccccccccccc:O001","verdict":"` + verdictReopened + `","note":"Checked."}]`
	verifierChecks := `[{"id":"O001","verdict":"` + verdictConfirmed + `","note":"Checked."},{"id":"S001","verdict":"` + verdictRefuted + `","note":"Checked."}]`
	return []parityCase{
		parityReviewerCase(t, "claude_valid_blocking_finding", opus, parityWrap(validStdout), ""),
		parityReviewerCase(t, "claude_blocking_false_is_note", opus, parityWrap(parityStdout("Prose summary.", parityReviewJSON(sha("a"),
			`[{"id":"C001","severity":"high","confidence":"high","path":"a.go","line":4,"side":"RIGHT","title":"Bug","body":"Details.","blocking":false}]`))), ""),
		parityReviewerCase(t, "claude_side_left_rejected", opus, parityWrap(parityStdout("Prose summary.", parityReviewJSON(sha("a"),
			`[{"id":"C001","severity":"high","confidence":"high","path":"a.go","line":4,"side":"LEFT","title":"Bug","body":"Details."}]`))), ""),
		parityReviewerCase(t, "claude_head_mismatch_rejected", opus, parityWrap(parityStdout("Prose summary.", parityReviewJSON(sha("d"), "[]"))), ""),
		parityReviewerCase(t, "claude_duplicate_key_last_wins", opus, parityWrap(parityStdout("Prose summary.",
			`{"repository":"acme/widget","number":5,"head_sha":"`+sha("a")+`","summary":"Reviewed the change thoroughly and found the listed issues.","findings":[],"findings":`+validFinding+`}`)), "claude_result_invalid"),
		parityReviewerCase(t, "claude_case_variant_wrapper_keys", opus, caseVariantWrapper, "review_wrapper_invalid"),
		parityReviewerCase(t, "claude_wrapper_top_level_null", opus, "null\n", "review_wrapper_invalid"),
		parityReviewerCase(t, "claude_stdout_top_level_null", opus, parityWrap("null"), "claude_result_invalid"),
		parityReviewerCase(t, "claude_result_text_null", opus, parityWrap(parityStdout("null", parityReviewJSON(sha("a"), "[]"))), "claude_result_mismatch"),
		parityReviewerCase(t, "codex_valid_report", codex, parityCodexRaw(parityCodexReport(parityCodexFindingsReport, sha("a"))), ""),
		parityReviewerCase(t, "codex_reviewed_head_mismatch", codex, parityCodexRaw(parityCodexReport(parityCodexFindingsReport, sha("d"))), ""),
		parityReviewerCase(t, "codex_case_variant_keys", codex, parityCodexRaw(parityCodexReport(
			`{"Findings":[],"Summary":"Full review of the pull request diff; details follow.","Tests":["go test ./..."],"Status":"completed","Reviewed_Head_Sha":"HEADSHA"}`, sha("a"))), "review_result_invalid"),
		parityReviewerCase(t, "codex_duplicate_key_last_wins", codex, parityCodexRaw(parityCodexReport(
			`{"findings":[],"summary":"Full review of the pull request diff; details follow.","tests":["go test ./..."],"status":"failed","status":"completed","reviewed_head_sha":"HEADSHA"}`, sha("a"))), "review_result_invalid"),
		parityReviewerCase(t, "codex_top_level_null", codex, parityCodexRaw("null"), "review_result_invalid"),
		parityVerifierCase(t, "verifier_valid_verdicts", parityWrap(parityStdout("Prose summary.", verifierStdout(verifierChecks, verifierPrior))), ""),
		parityVerifierCase(t, "verifier_duplicate_key_last_wins", parityWrap(parityStdout("Prose summary.",
			`{"repository":"acme/widget","number":5,"head_sha":"`+sha("a")+`","findings":[],"findings":`+verifierChecks+`,"prior_findings":`+verifierPrior+`}`)), "claude_result_invalid"),
		parityVerifierCase(t, "verifier_top_level_null", parityWrap("null"), "claude_result_invalid"),
	}
}
