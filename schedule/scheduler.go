package schedule

import (
	"fmt"
	"regexp"
	"sort"
	"strings"
	"time"
)

type Policy struct {
	Debounce    time.Duration
	RetryDelays []time.Duration
}

type Target struct {
	Repository string `json:"repository"`
	Number     int    `json:"number"`
	HeadSHA    string `json:"head_sha"`
	RequestID  int64  `json:"review_request_id"`
}

func (t Target) Key() string      { return fmt.Sprintf("%s#%d", t.Repository, t.Number) }
func (t Target) Identity() string { return fmt.Sprintf("%s@%s@%d", t.Key(), t.HeadSHA, t.RequestID) }

type Record struct {
	BlockingWorkflows []string `json:"blocking_workflows,omitempty"`
	Identity          string   `json:"identity"`
	Phase             string   `json:"phase"`
	// Attempt numbers the runs of this identity and keeps their workflow IDs
	// unique; Retries counts only the automatic retries after a failure.
	Attempt    int       `json:"attempt"`
	Retries    int       `json:"retries"`
	WorkflowID string    `json:"workflow_id,omitempty"`
	DueAt      time.Time `json:"due_at"`
	ErrorCode  string    `json:"error_code,omitempty"`
}

type Run struct {
	Target         Target
	WorkflowID     string
	Attempt        int
	Status         string
	Result         string
	Failure        string
	Released       bool
	CleanupPending bool
}

func (r Run) Active() bool { return (r.Status != "completed" && !r.Released) || r.CleanupPending }

type Decision struct {
	Record   Record
	Dispatch bool
	Notify   bool
}

// ReconcileCompleted applies durable completion evidence recorded after
// workflow cleanup. Run history can be cleaned up before a later review
// request arrives, so the scheduler cannot rely on run history alone.
func ReconcileCompleted(record Record, workflowIDs map[string]bool) Record {
	if record.WorkflowID != "" && workflowIDs[record.WorkflowID] {
		record.Phase = "completed"
		record.ErrorCode = ""
	}
	blockers := record.BlockingWorkflows[:0]
	for _, workflowID := range record.BlockingWorkflows {
		if !workflowIDs[workflowID] {
			blockers = append(blockers, workflowID)
		}
	}
	record.BlockingWorkflows = blockers
	return record
}

// Plan runs under the discovery object's exclusive lock. A missing result for
// a dispatched run is uncertain, never permission to start a duplicate.
func Plan(previous Record, target Target, runs []Run, now time.Time, policy Policy) Decision {
	r := previous
	fresh := r.Identity != target.Identity() || r.Phase == "withdrawn"
	if fresh {
		r = Record{Identity: target.Identity(), Phase: "waiting", DueAt: now.Add(policy.Debounce), BlockingWorkflows: previous.BlockingWorkflows}
		if previous.Phase == "running" && previous.WorkflowID != "" {
			r.BlockingWorkflows = append(r.BlockingWorkflows, previous.WorkflowID)
		}
	}
	var latest *Run
	blockers := map[string]bool{}
	for _, id := range r.BlockingWorkflows {
		blockers[id] = true
	}
	blocked := false
	for i := range runs {
		run := &runs[i]
		if run.Target.Key() != target.Key() {
			continue
		}
		blocked = blocked || run.Active()
		if run.Active() {
			blockers[run.WorkflowID] = true
		} else {
			delete(blockers, run.WorkflowID)
		}
		if run.Target.Identity() == target.Identity() && (latest == nil || run.Attempt > latest.Attempt) {
			latest = run
		}
	}
	r.BlockingWorkflows = nil
	for id := range blockers {
		r.BlockingWorkflows = append(r.BlockingWorkflows, id)
	}
	sort.Strings(r.BlockingWorkflows)
	blocked = blocked || len(r.BlockingWorkflows) > 0
	if latest != nil {
		if !fresh && r.Phase == "running" && r.Attempt > latest.Attempt {
			// Dispatch is durable but may not yet be visible in the admin query.
			return Decision{Record: r}
		}
		if latest.Active() {
			r.Phase, r.WorkflowID, r.Attempt = "running", latest.WorkflowID, latest.Attempt
			return Decision{Record: r}
		}
		if latest.Result == "success" || latest.Released {
			r.Phase, r.WorkflowID, r.Attempt = "completed", latest.WorkflowID, latest.Attempt
			return Decision{Record: r}
		}
		// Handle each terminal result once; subsequent polls preserve its retry deadline.
		if fresh || r.WorkflowID != latest.WorkflowID || r.Phase == "running" {
			r.WorkflowID, r.Attempt = latest.WorkflowID, latest.Attempt
			if code := FailureCode(latest.Failure); Superseded(code) {
				// A superseded run left nothing behind. The pull request is
				// still or again a candidate, so its head is reviewed anew.
				r.Phase, r.Attempt, r.ErrorCode, r.DueAt = "waiting", latest.Attempt+1, "", now.Add(policy.Debounce)
				return Decision{Record: r}
			}
			r = AfterFailure(r, FailureCode(latest.Failure), now, policy)
			return Decision{Record: r, Notify: true}
		}
	} else if r.Phase == "running" {
		r.ErrorCode = "workflow_state_unknown"
		return Decision{Record: r}
	}
	if blocked || (r.Phase != "waiting" && r.Phase != "retry_wait") || now.Before(r.DueAt) {
		return Decision{Record: r}
	}
	r.Phase = "running"
	return Decision{Record: r, Dispatch: true}
}

func AfterFailure(r Record, code string, now time.Time, policy Policy) Record {
	r.ErrorCode = FailureCode(code)
	if Retryable(r.ErrorCode) && r.Retries < len(policy.RetryDelays) {
		r.Phase = "retry_wait"
		r.DueAt = now.Add(policy.RetryDelays[r.Retries])
		r.Retries++
		r.Attempt++
	} else {
		r.Phase = "failed"
	}
	return r
}

func FailureCode(failure string) string {
	failure = strings.TrimPrefix(failure, "[500] ")
	code, _, _ := strings.Cut(failure, ":")
	code = strings.TrimSpace(code)
	if !regexp.MustCompile(`^[a-z][a-z0-9_]*$`).MatchString(code) {
		return "workflow_failed"
	}
	return code
}

func Retryable(code string) bool {
	switch code {
	case "pohunek_session_start_failed", "pohunek_request_failed", "pohunek_request_timeout", "pohunek_base_fetch_failed", "pohunek_base_fetch_timeout", "pohunek_base_fetch_dns_failed", "pohunek_base_fetch_ref_locked", "github_request_failed":
		return true
	default:
		return false
	}
}

func Superseded(code string) bool {
	switch code {
	case "head_changed", "review_label_removed", "review_request_changed", "review_request_not_active", "pull_request_not_open", "pull_request_draft":
		return true
	default:
		return false
	}
}

func Notification(target Target, record Record) string {
	description := FailureExplanation(record.ErrorCode)
	if record.Phase == "retry_wait" {
		return fmt.Sprintf("Review for %s at %.12s: %s Next automatic attempt: %d at %s. Diagnostic: %s.", target.Key(), target.HeadSHA, description, record.Attempt+1, record.DueAt.UTC().Format(time.RFC3339), record.ErrorCode)
	}
	return fmt.Sprintf("Review for %s at %.12s stopped after attempt %d. %s No further automatic attempt is scheduled. Fix the cause, then retry this PR. Diagnostic: %s.", target.Key(), target.HeadSHA, record.Attempt+1, description, record.ErrorCode)
}
