package schedule

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"
)

func fixture() (Target, time.Time, Policy) {
	return Target{Repository: "owner/repo", Number: 42, HeadSHA: strings.Repeat("a", 40)}, time.Unix(1000, 0), Policy{Debounce: 2 * time.Minute, RetryDelays: []time.Duration{time.Minute, 5 * time.Minute, 15 * time.Minute}}
}

func TestRapidPushesDebounceToLatestHead(t *testing.T) {
	target, now, policy := fixture()
	r := Record{}
	for _, head := range []string{"a", "b", "c", "d"} {
		target.HeadSHA = strings.Repeat(head, 40)
		d := Plan(r, target, nil, now, policy)
		if d.Dispatch {
			t.Fatal("dispatched while commits are still arriving")
		}
		r = d.Record
		now = now.Add(time.Minute)
	}
	if d := Plan(r, target, nil, now, policy); d.Dispatch {
		t.Fatal("dispatched before quiet period")
	}
	d := Plan(r, target, nil, now.Add(time.Minute), policy)
	if !d.Dispatch || d.Record.Identity != target.Identity() {
		t.Fatalf("latest head not dispatched: %+v", d)
	}
}

func TestSupersedingReviewWaitsForConfirmedCompletion(t *testing.T) {
	target, now, policy := fixture()
	old := target
	old.HeadSHA = strings.Repeat("b", 40)
	runs := []Run{{Target: old, WorkflowID: "old", Status: "running"}}
	d := Plan(Record{}, target, runs, now, policy)
	d = Plan(d.Record, target, runs, now.Add(time.Hour), policy)
	if d.Dispatch {
		t.Fatal("overlapped superseded review")
	}
	runs[0].Status = "completed"
	runs[0].Result = "success"
	d = Plan(d.Record, target, runs, now.Add(time.Hour), policy)
	if !d.Dispatch {
		t.Fatal("replacement did not start after cleanup completed")
	}
}

func TestTransientFailureRetriesWithPersistentBoundedBackoff(t *testing.T) {
	target, now, policy := fixture()
	runs := []Run{{Target: target, WorkflowID: "run0", Status: "completed", Result: "failure", Failure: "[500] pohunek_session_start_failed"}}
	d := Plan(Record{}, target, runs, now, policy)
	if d.Record.Phase != "retry_wait" || d.Record.Attempt != 1 || !d.Notify {
		t.Fatalf("legacy failure not recovered: %+v", d)
	}
	for attempt := 1; attempt <= len(policy.RetryDelays); attempt++ {
		// Simulate a service restart between scheduling and dispatch.
		encoded, _ := json.Marshal(d.Record)
		var restored Record
		json.Unmarshal(encoded, &restored)
		d = Plan(restored, target, runs, restored.DueAt.Add(-time.Second), policy)
		if d.Dispatch || d.Notify || !d.Record.DueAt.Equal(restored.DueAt) {
			t.Fatal("retry deadline was reset")
		}
		d = Plan(d.Record, target, runs, restored.DueAt, policy)
		if !d.Dispatch || d.Record.Attempt != attempt {
			t.Fatalf("retry missing: %+v", d)
		}
		d.Record.WorkflowID = "new-run"
		lagged := Plan(d.Record, target, runs, restored.DueAt.Add(time.Second), policy)
		if lagged.Dispatch || lagged.Notify || lagged.Record.Attempt != attempt {
			t.Fatal("admin visibility lag duplicated dispatch")
		}
		runs = []Run{{Target: target, WorkflowID: "new-run", Attempt: attempt, Status: "completed", Result: "failure", Failure: "[500] pohunek_session_start_failed"}}
		d = Plan(d.Record, target, runs, restored.DueAt.Add(time.Second), policy)
	}
	if d.Record.Phase != "failed" || d.Dispatch {
		t.Fatalf("retry budget exceeded: %+v", d)
	}
}

func TestTerminalFailureSuccessAndUnknownHistoryNeverRedispatch(t *testing.T) {
	target, now, policy := fixture()
	for _, run := range []Run{
		{Target: target, WorkflowID: "run", Status: "completed", Result: "failure", Failure: "[500] review_wrapper_invalid"},
		{Target: target, WorkflowID: "run", Status: "completed", Result: "success"},
		{Target: target, WorkflowID: "run", Status: "suspended", Released: true},
	} {
		d := Plan(Record{}, target, []Run{run}, now, policy)
		d = Plan(d.Record, target, []Run{run}, now.Add(time.Hour), policy)
		if d.Dispatch {
			t.Fatalf("unexpected dispatch: %+v", run)
		}
	}
	r := Record{Identity: target.Identity(), Phase: "running", WorkflowID: "unknown"}
	if d := Plan(r, target, nil, now.Add(time.Hour), policy); d.Dispatch || d.Record.ErrorCode != "workflow_state_unknown" {
		t.Fatalf("uncertain run repeated: %+v", d)
	}
}

func TestNewAssignmentAndIndependentPR(t *testing.T) {
	target, now, policy := fixture()
	other := target
	other.Number++
	runs := []Run{{Target: other, Status: "running"}}
	r := Plan(Record{}, target, runs, now, policy).Record
	if !Plan(r, target, runs, now.Add(policy.Debounce), policy).Dispatch {
		t.Fatal("another PR blocked this PR")
	}
	r.Phase = "completed"
	target.RequestID++
	d := Plan(r, target, nil, now, policy)
	if d.Record.Phase != "waiting" {
		t.Fatal("new review request was ignored")
	}
}

func TestDiagnosticsDoNotExposeRawErrors(t *testing.T) {
	if Diagnostic(errors.New("secret-token: No space left on device")) != "disk_full" {
		t.Fatal("disk failure not classified")
	}
	if got := Diagnostic(errors.New("token=secret prompt=private")); got != "unclassified_cli_failure" {
		t.Fatalf("raw detail escaped: %s", got)
	}
}

func TestMissingOrUncleanPreviousRunBlocksNewHead(t *testing.T) {
	target, now, policy := fixture()
	old := target
	old.HeadSHA = strings.Repeat("b", 40)
	r := Record{Identity: old.Identity(), Phase: "running", WorkflowID: "old"}
	d := Plan(r, target, nil, now, policy)
	d = Plan(d.Record, target, nil, now.Add(time.Hour), policy)
	if d.Dispatch {
		t.Fatal("missing previous run allowed overlap")
	}
	runs := []Run{{Target: old, WorkflowID: "old", Status: "completed", Result: "failure", CleanupPending: true}}
	d = Plan(d.Record, target, runs, now.Add(time.Hour), policy)
	if d.Dispatch {
		t.Fatal("unconfirmed cleanup allowed overlap")
	}
	runs[0].CleanupPending = false
	d = Plan(d.Record, target, runs, now.Add(time.Hour), policy)
	if !d.Dispatch {
		t.Fatal("confirmed cleanup did not release the pending head")
	}
}

func TestCompletedEvidenceReleasesExpiredPreviousWorkflow(t *testing.T) {
	target, now, policy := fixture()
	old := target
	old.HeadSHA = strings.Repeat("b", 40)

	previous := Record{Identity: old.Identity(), Phase: "running", WorkflowID: "old"}
	previous = ReconcileCompleted(previous, map[string]bool{"old": true})
	if previous.Phase != "completed" {
		t.Fatalf("completed workflow remained %s", previous.Phase)
	}

	decision := Plan(previous, target, nil, now, policy)
	decision = Plan(decision.Record, target, nil, now.Add(policy.Debounce), policy)
	if !decision.Dispatch {
		t.Fatalf("new review remained blocked after run history expired: %+v", decision)
	}

	stuck := Record{
		Identity:          target.Identity(),
		Phase:             "waiting",
		BlockingWorkflows: []string{"old", "unknown"},
		DueAt:             now,
	}
	stuck = ReconcileCompleted(stuck, map[string]bool{"old": true})
	if len(stuck.BlockingWorkflows) != 1 || stuck.BlockingWorkflows[0] != "unknown" {
		t.Fatalf("unexpected blockers after reconciliation: %v", stuck.BlockingWorkflows)
	}
	if Plan(stuck, target, nil, now, policy).Dispatch {
		t.Fatal("unconfirmed workflow no longer blocked dispatch")
	}
	stuck = ReconcileCompleted(stuck, map[string]bool{"unknown": true})
	if !Plan(stuck, target, nil, now, policy).Dispatch {
		t.Fatal("confirmed stale blockers did not release dispatch")
	}
	if empty := ReconcileCompleted(Record{Phase: "waiting"}, map[string]bool{"": true}); empty.Phase != "waiting" {
		t.Fatal("empty workflow id was treated as completion evidence")
	}
}

func TestWithdrawnPendingRequestCanBeObservedAgain(t *testing.T) {
	target, now, policy := fixture()
	r := Record{Identity: target.Identity(), Phase: "withdrawn"}
	d := Plan(r, target, nil, now, policy)
	if d.Record.Phase != "waiting" || d.Dispatch {
		t.Fatalf("unexpected reactivation: %+v", d)
	}
}

func TestPolicyFailsFastAndRejectsUnsafeValues(t *testing.T) {
	values := map[string]string{"TEST_DEBOUNCE_SECONDS": "120", "TEST_RETRY_DELAYS_SECONDS_JSON": "[60,300,900]"}
	lookup := func(k string) (string, bool) { v, ok := values[k]; return v, ok }
	if _, err := LoadPolicy(lookup, "TEST"); err != nil {
		t.Fatal(err)
	}
	for _, bad := range []string{"", "[]", "[0]", "[300,60]", "[3601]"} {
		values["TEST_RETRY_DELAYS_SECONDS_JSON"] = bad
		if _, err := LoadPolicy(lookup, "TEST"); err == nil {
			t.Fatalf("accepted %q", bad)
		}
	}
}

// A superseded run left nothing behind, so the same head is reviewed again
// once its trigger asks for it, under a new attempt.
func TestSupersededRunIsDispatchedAgainWithoutANotification(t *testing.T) {
	target, now, policy := fixture()
	runs := []Run{{Target: target, WorkflowID: "run0", Status: "completed", Result: "failure", Failure: "[500] review_label_removed"}}
	d := Plan(Record{}, target, runs, now, policy)
	if d.Notify || d.Dispatch || d.Record.Phase != "waiting" || d.Record.Attempt != 1 {
		t.Fatalf("superseded run handling: %+v", d)
	}
	d = Plan(d.Record, target, runs, now.Add(policy.Debounce), policy)
	if !d.Dispatch || d.Record.Attempt != 1 {
		t.Fatalf("the head was not reviewed again: %+v", d)
	}
}

// Reviewing a head again after a supersede never spends the retry budget.
func TestSupersededRedispatchKeepsTheRetryBudget(t *testing.T) {
	target, now, policy := fixture()
	record := Record{}
	for attempt := 0; attempt < 3; attempt++ {
		runs := []Run{{Target: target, WorkflowID: "superseded", Attempt: attempt, Status: "completed", Result: "failure", Failure: "[500] review_label_removed"}}
		record.WorkflowID = ""
		record = Plan(record, target, runs, now, policy).Record
	}
	failed := []Run{{Target: target, WorkflowID: "failed", Attempt: record.Attempt, Status: "completed", Result: "failure", Failure: "[500] github_request_failed"}}
	record.Phase = "running"
	d := Plan(record, target, failed, now, policy)
	if d.Record.Phase != "retry_wait" || d.Record.Retries != 1 {
		t.Fatalf("supersedes consumed the retry budget: %+v", d.Record)
	}
}

// An active run of a retired generation holds the pull request without ever
// becoming its completion.
func TestRequestTimeoutIsRetryableAndExplained(t *testing.T) {
	if !Retryable("pohunek_request_timeout") {
		t.Fatal("a Pohunek request timeout must be retried")
	}
	if !strings.Contains(FailureExplanation("pohunek_request_timeout"), "did not answer within") {
		t.Fatal("request timeout lacks a specific explanation")
	}
	if got := Diagnostic(errors.New("exit status 1: pohunek error code request_timeout")); got != "timeout" {
		t.Fatalf("diagnostic = %s", got)
	}
}
