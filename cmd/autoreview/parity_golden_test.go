package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
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
