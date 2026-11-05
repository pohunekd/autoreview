package main

import (
	"context"
	"database/sql"
	"fmt"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

func openTestStore(t *testing.T) *Store {
	t.Helper()
	store, err := OpenStore(filepath.Join(t.TempDir(), "state.db"), time.Now)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	return store
}

func enqueueTestReview(t *testing.T, store *Store, workflowID string, input ReviewInput) {
	t.Helper()
	ctx := context.Background()
	if err := store.inTx(ctx, func(tx *sql.Tx) error { return store.enqueueReview(ctx, tx, workflowID, input) }); err != nil {
		t.Fatal(err)
	}
}

// Approve, supersede and expiry all race for one wait. Whichever writer
// commits first sets the decision, and every writer reports that decision.
func TestApprovalDecisionIsSetOnceAndEveryWriterReportsTheWinner(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(4))
	for round := range 20 {
		workflowID := fmt.Sprintf("prr-v2-race-%d", round)
		enqueueTestReview(t, store, workflowID, input)
		decisions := []string{approvalKindApprove, approvalKindSuperseded, approvalKindExpired}
		results := make([]string, len(decisions))
		errs := make([]error, len(decisions))
		start := make(chan struct{})
		var wg sync.WaitGroup
		for index, decision := range decisions {
			wg.Add(1)
			go func() {
				defer wg.Done()
				<-start
				results[index], errs[index] = store.decideApproval(ctx, workflowID, decision)
			}()
		}
		close(start)
		wg.Wait()
		stored, found, err := store.getReview(ctx, workflowID)
		if err != nil || !found {
			t.Fatalf("round %d: review missing: %v", round, err)
		}
		for index := range decisions {
			if errs[index] != nil || results[index] != stored.Approval {
				t.Fatalf("round %d: writer %d reported %q (err %v), stored %q", round, index, results[index], errs[index], stored.Approval)
			}
		}
	}
}

func TestApprovalDecisionKeepsTheFirstWriter(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(5))
	enqueueTestReview(t, store, "prr-v2-supersede-first", input)
	if decision, err := store.decideApproval(ctx, "prr-v2-supersede-first", approvalKindSuperseded); err != nil || decision != approvalKindSuperseded {
		t.Fatalf("first decision = %q, %v", decision, err)
	}
	if decision, err := store.decideApproval(ctx, "prr-v2-supersede-first", approvalKindApprove); err != nil || decision != approvalKindSuperseded {
		t.Fatalf("approval after a supersede reported %q, %v", decision, err)
	}
	enqueueTestReview(t, store, "prr-v2-approve-first", input)
	if decision, err := store.decideApproval(ctx, "prr-v2-approve-first", approvalKindApprove); err != nil || decision != approvalKindApprove {
		t.Fatalf("first decision = %q, %v", decision, err)
	}
	if decision, err := store.decideApproval(ctx, "prr-v2-approve-first", approvalKindExpired); err != nil || decision != approvalKindApprove {
		t.Fatalf("expiry after an approval reported %q, %v", decision, err)
	}
}

// runHistory is the only view the scheduler plans with, so it must expose the
// attempt and target stored in the review input.
func TestRunHistoryExposesTheStoredReviewToThePlanner(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	pr := testPullRequest(77)
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), pr)
	input.Attempt = 2
	enqueueTestReview(t, store, "prr-v2-history", input)
	runs, err := store.runHistory(ctx, 10)
	if err != nil || len(runs) != 1 || runs[0].Target != pr.Target() || runs[0].Attempt != 2 || runs[0].WorkflowID != "prr-v2-history" || !runs[0].Active() {
		t.Fatalf("runs = %+v, err = %v", runs, err)
	}
	if _, err := store.runHistory(ctx, 0); errorCode(err) != "review_history_limit_exceeded" {
		t.Fatalf("a history above the limit must fail closed, got %v", err)
	}
}

func TestBackupWritesAReadableCopyAndRefusesAnExistingPath(t *testing.T) {
	store := openTestStore(t)
	ctx := context.Background()
	enqueueTestReview(t, store, "prr-v2-backup", testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), testPullRequest(3)))
	target := filepath.Join(t.TempDir(), "backup.db")
	if err := store.backupTo(ctx, target); err != nil {
		t.Fatal(err)
	}
	copied, err := OpenStore(target, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	defer copied.Close()
	stored, found, err := copied.getReview(ctx, "prr-v2-backup")
	if err != nil || !found || stored.Input.PullRequest.Number != 3 {
		t.Fatalf("copied review = %+v, found %v, err %v", stored, found, err)
	}
	if err := store.backupTo(ctx, target); errorCode(err) != "backup_path_exists" {
		t.Fatalf("an existing backup path must be refused, got %v", err)
	}
	if err := store.backupTo(ctx, "relative/backup.db"); errorCode(err) != "backup_path_invalid" {
		t.Fatalf("a relative backup path must be refused, got %v", err)
	}
}
