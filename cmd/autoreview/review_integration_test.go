package main

import (
	"context"
	"database/sql"
	"strings"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

func TestCleanReviewOfAnApprovingRuleIsApprovedOnceWithoutTheUser(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("an automatic approval", func() bool { return h.publications() == 1 })
	h.waitFor("the approval notification", func() bool { return h.fake.hasNote(policy.LevelDigest, "Approved acme/widget PR") })
	published, notes, started, open := h.fake.snapshot()
	if published[0].Decision != DecisionApprove || len(published[0].Comments) != 0 || len(started) != 2 || open != 0 {
		t.Fatalf("decision=%s comments=%d started=%d open=%d", published[0].Decision, len(published[0].Comments), len(started), open)
	}
	for _, entry := range notes {
		if entry.Level == policy.LevelAction {
			t.Fatalf("an automatic approval must not ping the user: %+v", entry)
		}
	}
	for range 3 {
		h.poll()
	}
	if h.startedCount() != 2 || h.publications() != 1 {
		t.Fatal("a published head was reviewed again")
	}
}

func TestFindingsRequestChangesAndTheNextHeadGetsAFullReview(t *testing.T) {
	h := startHarness(t, nil)
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}}))
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("a request for changes", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionRequestChanges || len(published[0].Comments) != 2 {
		t.Fatalf("decision=%s comments=%d", published[0].Decision, len(published[0].Comments))
	}
	h.setOutput(reviewerOutput(t, nil))
	next := pr
	next.HeadSHA = sha("c")
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the review of the next head", func() bool { return h.publications() == 2 })
	_, _, started, _ := h.fake.snapshot()
	if len(started) != 4 {
		t.Fatalf("after a request for changes both reviewers must review in full, started %d sessions", len(started))
	}
	published, _, _, _ = h.fake.snapshot()
	if published[1].Decision != DecisionApprove || !strings.Contains(published[1].Body, "full review") {
		t.Fatalf("second review = %s: %s", published[1].Decision, published[1].Body)
	}
}

func TestApprovedHeadIsFollowedByAnIncrementalReview(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the first approval", func() bool { return h.publications() == 1 })
	next := pr
	next.HeadSHA = sha("c")
	next.ReviewRequestID = 12
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the incremental approval", func() bool { return h.publications() == 2 })
	published, _, started, _ := h.fake.snapshot()
	if len(started) != 4 || !strings.Contains(published[1].Body, "incremental review") {
		t.Fatalf("an automatically approved re-review must use every reviewer, started %d sessions: %s", len(started), published[1].Body)
	}
}

func TestAwaitedApprovalWaitsForTheUserAndPublishesOnce(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	if !h.fake.hasNote(policy.LevelAction, "Review ID: "+pr.ReviewID()) || h.publications() != 0 {
		t.Fatal("the approval request must carry the Review ID and nothing may be posted yet")
	}
	for range 3 {
		h.poll()
	}
	if h.startedCount() != 2 {
		t.Fatal("a review waiting for approval was dispatched again")
	}
	resolved, err := h.resolve(pr)
	if err != nil || resolved.HeadSHA != pr.HeadSHA {
		t.Fatalf("resolve = %+v, %v", resolved, err)
	}
	result, err := h.approveWhenAwaiting(pr)
	if err != nil || result.PublicationStatus != "pending" {
		t.Fatalf("approve = %+v, %v", result, err)
	}
	h.waitFor("the approval", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionApprove {
		t.Fatalf("decision = %s", published[0].Decision)
	}
	h.waitFor("a repeated approval to report the publication", func() bool {
		again, err := h.resolve(pr)
		if err != nil {
			return false
		}
		repeated, err := h.approve(again.WorkflowID, pr)
		return err == nil && repeated.PublicationStatus == "published" && repeated.ReviewURL != ""
	})
	if h.publications() != 1 {
		t.Fatal("a repeated approval posted again")
	}
}

func TestNewerHeadSupersedesAWaitingApproval(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	resolved, err := h.resolve(pr)
	if err != nil {
		t.Fatal(err)
	}
	next := pr
	next.HeadSHA = sha("c")
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the old wait to end", func() bool { return h.fake.hasNote(policy.LevelLog, "Review "+pr.ReviewID()+" was superseded") })
	if _, err := h.approve(resolved.WorkflowID, pr); err == nil || !strings.Contains(err.Error(), "review_superseded") {
		t.Fatalf("approving a superseded review must fail, got %v", err)
	}
	h.pollUntil("the approval request of the new head", func() bool { return h.fake.hasNote(policy.LevelAction, "Review ID: "+next.ReviewID()) })
	if h.publications() != 0 {
		t.Fatal("nothing may be posted without an approval")
	}
}

func TestMissingFromSearchIsConfirmedBeforeAWaitEnds(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	h.fake.mu.Lock()
	delete(h.fake.candidates, pr.Key())
	h.fake.mu.Unlock()
	for range 3 {
		h.poll()
	}
	if _, err := h.resolve(pr); err != nil {
		t.Fatalf("a search gap must not end a wait whose pull request is still open: %v", err)
	}
	h.fake.lapse(pr, "pull_request_not_open")
	h.pollUntil("the wait to end after the pull request closed", func() bool { _, err := h.resolve(pr); return err != nil })
}

func TestApprovalExpiresAndALateApprovalIsRejected(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	input := testInput(h.rule(pr.Repository), pr)
	input.ApprovalTimeoutSeconds = 1
	input.Limits = runLimits(h.cfg)
	workflowID := pr.WorkflowID(0)
	ctx := context.Background()
	if err := h.store.inTx(ctx, func(tx *sql.Tx) error { return h.store.enqueueReview(ctx, tx, workflowID, input) }); err != nil {
		t.Fatal(err)
	}
	h.waitFor("the approval to expire", func() bool {
		return h.fake.hasNote(input.Notifications[policy.EventApprovalExpired], "expired after 1s without a reply")
	})
	if _, err := h.approve(workflowID, pr); err == nil || !strings.Contains(err.Error(), "approval_expired") {
		t.Fatalf("a late approval must be rejected, got %v", err)
	}
	if h.publications() != 0 {
		t.Fatal("an expired approval posted a review")
	}
}

func TestStuckSessionCleanupIsReportedAndTheReviewStillPublishes(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.mu.Lock()
	h.fake.removeFailing["s-1"] = true
	h.fake.mu.Unlock()
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the publication despite a stuck session", func() bool { return h.publications() == 1 })
	if !h.fake.hasNote(policy.LevelAction, "Pohunek could not remove reviewer sessions") || !h.fake.hasNote(policy.LevelAction, "runtime_supervision_ambiguous") {
		t.Fatal("the stuck session was not reported for manual removal")
	}
}

func TestUnusableReviewerOutputGetsAFreshRun(t *testing.T) {
	h := startHarness(t, nil)
	valid := reviewerOutput(t, nil)
	h.setOutput(func(request SessionRequest) []byte {
		if request.Reviewer.ID == "opus" && request.Run == 1 {
			return []byte("{\"protocol\":\"broken\"}\n")
		}
		return valid(request)
	})
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the publication after a reviewer retry", func() bool { return h.publications() == 1 })
	_, _, started, _ := h.fake.snapshot()
	runs := 0
	for _, request := range started {
		if request.Reviewer.ID == "opus" {
			runs++
		}
	}
	if runs != 2 {
		t.Fatalf("opus ran %d times", runs)
	}
}

// A reviewer output that passes the schema but fails the engine's own limits,
// such as a multiline title, is a model mistake a fresh run can fix.
func TestReviewerOutputOverTheLimitsGetsAFreshRun(t *testing.T) {
	h := startHarness(t, nil)
	valid := reviewerOutput(t, nil)
	invalid := reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Bug\nsecond line", "body": "Details."}})
	h.setOutput(func(request SessionRequest) []byte {
		if request.Reviewer.ID == "opus" && request.Run == 1 {
			return invalid(request)
		}
		return valid(request)
	})
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the publication after a reviewer retry", func() bool { return h.publications() == 1 })
	_, _, started, _ := h.fake.snapshot()
	runs := 0
	for _, request := range started {
		if request.Reviewer.ID == "opus" {
			runs++
		}
	}
	if runs != 2 {
		t.Fatalf("opus ran %d times", runs)
	}
}

func TestBaseFetchLockedByAConcurrentReviewIsRetriedOnAFreshBranch(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.mu.Lock()
	h.fake.startFail = func(request SessionRequest) error {
		if request.Reviewer.ID == "opus" && request.Start <= 1 {
			return workflowError("pohunek_base_fetch_ref_locked", false, nil)
		}
		return nil
	}
	h.fake.mu.Unlock()
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the publication after a locked base fetch", func() bool { return h.publications() == 1 })
	h.fake.mu.Lock()
	defer h.fake.mu.Unlock()
	branches := map[string]bool{}
	for _, request := range h.fake.attempts {
		if request.Reviewer.ID == "opus" {
			branches[request.Branch()] = true
		}
	}
	// Pohunek keeps the failed start's branch, so reusing it would check out a
	// stale branch instead of the fresh base.
	if len(branches) != 2 {
		t.Fatalf("opus started on branches %v", branches)
	}
}

func TestOwnPullRequestGetsACommentAndFindingsPingTheAuthor(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"a.go": {1}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "medium", "confidence": "high", "path": "a.go", "line": 1, "title": "Fix me", "body": "Details."}}))
	pr := integrationPullRequest("acme/widget", sha("a"))
	pr.Author = h.cfg.GitHubLogin
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the comment", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionComment || !strings.Contains(published[0].Body, "own pull request") {
		t.Fatalf("own pull request review = %s: %s", published[0].Decision, published[0].Body)
	}
	h.waitFor("the action notification", func() bool { return h.fake.hasNote(policy.LevelAction, "Commented on acme/widget") })
}

func TestRemovedLabelStopsTheReviewWithoutPublishing(t *testing.T) {
	h := startHarness(t, nil)
	h.setOutput(func(SessionRequest) []byte { return nil })
	pr := integrationPullRequest("solo/tool", sha("a"))
	pr.ReviewRequestID = 0
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the reviewer session", func() bool { return h.startedCount() == 1 })
	h.fake.lapse(pr, "review_label_removed")
	h.waitFor("the superseded notification", func() bool { return h.fake.hasNote(policy.LevelLog, "superseded (review_label_removed)") })
	_, _, _, open := h.fake.snapshot()
	if h.publications() != 0 || open != 0 {
		t.Fatalf("published=%d open sessions=%d", h.publications(), open)
	}
}

func TestStoppedReviewIsRetriedByHeadPrefix(t *testing.T) {
	h := startHarness(t, nil)
	valid := reviewerOutput(t, nil)
	broken := true
	h.setOutput(func(request SessionRequest) []byte {
		h.fake.mu.Lock()
		failing := broken
		h.fake.mu.Unlock()
		if failing {
			return []byte("{\"protocol\":\"broken\"}\n")
		}
		return valid(request)
	})
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the stop notification", func() bool { return h.fake.hasNote(policy.LevelAction, "No further automatic attempt is scheduled") })
	h.fake.mu.Lock()
	broken = false
	h.fake.mu.Unlock()
	result, err := h.retry(pr)
	if err != nil || result.HeadSHA != pr.HeadSHA || result.Attempt != 1 {
		t.Fatalf("retry = %+v, %v", result, err)
	}
	h.pollUntil("the retried review", func() bool { return h.publications() == 1 })
}

func TestReaddedLabelReviewsTheSameHeadAgain(t *testing.T) {
	h := startHarness(t, nil)
	valid := reviewerOutput(t, nil)
	running := true
	h.setOutput(func(request SessionRequest) []byte {
		h.fake.mu.Lock()
		waiting := running
		h.fake.mu.Unlock()
		if waiting {
			return nil
		}
		return valid(request)
	})
	pr := integrationPullRequest("solo/tool", sha("a"))
	pr.ReviewRequestID = 0
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the first reviewer session", func() bool { return h.startedCount() == 1 })
	h.fake.lapse(pr, "review_label_removed")
	h.waitFor("the superseded review", func() bool { return h.fake.hasNote(policy.LevelLog, "superseded (review_label_removed)") })
	for range 3 {
		h.poll()
	}
	h.fake.mu.Lock()
	running = false
	h.fake.mu.Unlock()
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the review after the label returned", func() bool { return h.publications() == 1 })
}

func TestFailedPublicationAfterApprovalIsReportedAndRetried(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	h.fake.mu.Lock()
	h.fake.publishFail = "github_request_rejected"
	h.fake.mu.Unlock()
	if _, err := h.approveWhenAwaiting(pr); err != nil {
		t.Fatal(err)
	}
	h.waitFor("the failure report", func() bool {
		return h.fake.hasNote(policy.LevelAction, "Review for "+pr.Key()+" at "+pr.HeadSHA[:12]+" stopped after your approval: it was not published (github_request_rejected)")
	})
	h.fake.mu.Lock()
	h.fake.publishFail = ""
	h.fake.mu.Unlock()
	h.waitFor("an accepted retry", func() bool {
		_, err := h.retry(pr)
		if err != nil {
			t.Logf("retry: %v", err)
		}
		return err == nil
	})
	h.pollUntil("the approval request of the retried review", func() bool {
		_, notes, _, _ := h.fake.snapshot()
		return countNotes(notes, policy.LevelAction, approvalRequestedTitle) == 2
	})
}

func TestRejectedPullRequestIsReportedOnceAndDoesNotBlockOthers(t *testing.T) {
	h := startHarness(t, nil)
	broken := integrationPullRequest("acme/widget", sha("e"))
	h.fake.mu.Lock()
	h.fake.rejected = []RejectedCandidate{{Repository: broken.Repository, Number: broken.Number, Code: "github_response_invalid"}}
	h.fake.mu.Unlock()
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the review of the valid pull request", func() bool { return h.publications() == 1 })
	_, notes, _, _ := h.fake.snapshot()
	if reports := countNotes(notes, policy.LevelAction, broken.Key()+" is not started"); reports != 1 {
		t.Fatalf("the rejected pull request was reported %d times", reports)
	}
}

// A transient outage longer than the step retry window ends the step with its
// own retryable code instead of pausing the review.
func TestOutageLongerThanTheStepRetryWindowIsRetriedLater(t *testing.T) {
	h := startHarness(t, func(cfg *Config) { cfg.StepRetry = 2 * time.Second })
	h.setOutput(func(SessionRequest) []byte { return nil })
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the reviewer sessions", func() bool { return h.startedCount() == 2 })
	h.fake.mu.Lock()
	h.fake.transientUntil = time.Now().Add(6 * time.Second)
	h.fake.mu.Unlock()
	h.waitFor("the retry notice", func() bool {
		// The outage makes discovery fail; the notice is what this test waits for.
		_, _ = h.engine.Poll(context.Background())
		return h.fake.hasNote(policy.LevelLog, "Next automatic attempt") && h.fake.hasNote(policy.LevelLog, "github_request_failed")
	})
	h.setOutput(reviewerOutput(t, nil))
	h.pollUntil("the retried review", func() bool { return h.publications() == 1 })
}

// A restart while reviewers still run resumes the review from its stored
// sessions: no reviewer starts again and the review publishes exactly once.
func TestRestartDuringCollectResumesWithoutStartingReviewersAgain(t *testing.T) {
	h := startHarness(t, nil)
	h.setOutput(func(SessionRequest) []byte { return nil })
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("both reviewer sessions", func() bool { return h.startedCount() == 2 })
	// Stopping before the started notice is stored sends it again after the restart.
	h.waitFor("the started notice to be stored", func() bool { return h.stepRecorded(notifyStepPrefix + "started") })
	h.stopEngine()
	h.start()
	h.setOutput(reviewerOutput(t, nil))
	h.pollUntil("the publication after the restart", func() bool { return h.publications() == 1 })
	published, notes, started, _ := h.fake.snapshot()
	if len(published) != 1 || published[0].Decision != DecisionApprove {
		t.Fatalf("published = %+v", published)
	}
	if len(started) != 2 {
		t.Fatalf("the restart started %d sessions in total, want 2", len(started))
	}
	if started := countNotes(notes, policy.LevelLog, "GitHub review started"); started != 1 {
		t.Fatalf("the started notice was sent %d times", started)
	}
	h.waitFor("the completed sessions to be removed", func() bool { _, _, _, open := h.fake.snapshot(); return open == 0 })
}

// A pull request author re-requests the review after every push; a renewed
// request of an unchanged head must not start another sample of the review.
func TestRenewedRequestOfAPublishedHeadStartsNoReview(t *testing.T) {
	h := startHarness(t, nil)
	pr := integrationPullRequest("acme/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the first publication", func() bool { return h.publications() == 1 })
	renewed := pr
	renewed.ReviewRequestID = 99
	h.fake.open(renewed, h.rule(pr.Repository))
	h.settle()
	if h.startedCount() != 2 || h.publications() != 1 {
		t.Fatalf("a renewed request of the published head started %d sessions and %d publications", h.startedCount(), h.publications())
	}
}

func TestRenewedRequestKeepsARunningReviewAndAWaitingApproval(t *testing.T) {
	h := startHarness(t, nil)
	valid := reviewerOutput(t, nil)
	running := true
	h.setOutput(func(request SessionRequest) []byte {
		h.fake.mu.Lock()
		waiting := running
		h.fake.mu.Unlock()
		if waiting {
			return nil
		}
		return valid(request)
	})
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the reviewer sessions", func() bool { return h.startedCount() == 2 })
	renewed := pr
	renewed.ReviewRequestID = 99
	h.fake.open(renewed, h.rule(pr.Repository))
	h.poll()
	h.fake.mu.Lock()
	running = false
	h.fake.mu.Unlock()
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	again := renewed
	again.ReviewRequestID = 100
	h.fake.open(again, h.rule(pr.Repository))
	h.settle()
	if h.fake.hasNote(policy.LevelLog, "was superseded") || h.startedCount() != 2 {
		t.Fatalf("a renewed request superseded the review or started another one: %d sessions", h.startedCount())
	}
	if _, err := h.resolve(pr); err != nil {
		t.Fatalf("the wait must survive a renewed request: %v", err)
	}
}

func TestNotesOnlyReviewOfAnAwaitRuleAsksForApprovalAndPublishesTheNotes(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go", "line": 3, "title": "Naming", "body": "Could be clearer."}}))
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	if !h.fake.hasNote(policy.LevelAction, "Non-blocking notes: 2") || h.publications() != 0 {
		t.Fatal("the approval request must list the notes and nothing may be posted yet")
	}
	if _, err := h.approveWhenAwaiting(pr); err != nil {
		t.Fatal(err)
	}
	h.waitFor("the approval", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionApprove || len(published[0].Comments) != 0 || !strings.Contains(published[0].Body, "2 non-blocking note(s)") {
		t.Fatalf("decision=%s comments=%d body=%s", published[0].Decision, len(published[0].Comments), published[0].Body)
	}
}

// handNotesToTheAuthor publishes a notes-only review of a hand-off rule and
// checks it went to the author as a comment without pinging the user.
func handNotesToTheAuthor(t *testing.T, h *harness) PullRequest {
	t.Helper()
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "low", "confidence": "high", "blocking": false, "path": "src/a.go", "line": 3, "title": "Naming", "body": "Could be clearer."}}))
	pr := integrationPullRequest("acme/notes", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the hand-off comment", func() bool { return h.publications() == 1 })
	h.waitFor("the hand-off notification", func() bool { return h.fake.hasNote(policy.LevelDigest, "Handed 2 non-blocking note(s) on acme/notes") })
	published, notes, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionComment || len(published[0].Comments) != 0 || !strings.Contains(published[0].Body, "Request this review again once you have decided") {
		t.Fatalf("decision=%s comments=%d body=%s", published[0].Decision, len(published[0].Comments), published[0].Body)
	}
	for _, entry := range notes {
		if entry.Level == policy.LevelAction {
			t.Fatalf("notes handed to the author must not ping the user: %+v", entry)
		}
	}
	return pr
}

func TestRenewedRequestAfterANotesHandoffAsksForApprovalWithoutAReview(t *testing.T) {
	h := startHarness(t, nil)
	pr := handNotesToTheAuthor(t, h)
	h.settle()
	if h.startedCount() != 2 || h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) {
		t.Fatal("a hand-off must wait for the author's renewed request")
	}
	renewed := pr
	renewed.ReviewRequestID = 99
	h.fake.open(renewed, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	if h.startedCount() != 2 || h.publications() != 1 || !h.fake.hasNote(policy.LevelAction, "The author requested the review again") || !h.fake.hasNote(policy.LevelAction, "Non-blocking notes: 2") {
		t.Fatalf("the renewed request must reuse the hand-off review: %d sessions, %d publications", h.startedCount(), h.publications())
	}
	// The notification precedes the stored wait by a moment.
	h.waitFor("the approval to be accepted", func() bool {
		resolved, err := h.resolve(renewed)
		if err != nil {
			return false
		}
		_, err = h.approve(resolved.WorkflowID, renewed)
		return err == nil
	})
	h.waitFor("the approval", func() bool { return h.publications() == 2 })
	published, _, _, _ := h.fake.snapshot()
	if published[1].Decision != DecisionApprove || len(published[1].Comments) != 0 || !strings.Contains(published[1].Body, "2 non-blocking note(s)") {
		t.Fatalf("decision=%s comments=%d body=%s", published[1].Decision, len(published[1].Comments), published[1].Body)
	}
	h.settle()
	if h.startedCount() != 2 || h.publications() != 2 {
		t.Fatal("an approved head was reviewed or published again")
	}
}

func TestNewCommitsAfterANotesHandoffAreReviewedAndWaitForTheUser(t *testing.T) {
	h := startHarness(t, nil)
	pr := handNotesToTheAuthor(t, h)
	next := pr
	next.HeadSHA = sha("c")
	next.ReviewRequestID = 99
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	if h.startedCount() != 3 || h.publications() != 1 {
		t.Fatalf("new commits must get an incremental review that waits for the user: %d sessions, %d publications", h.startedCount(), h.publications())
	}
	if _, err := h.resolve(next); err != nil {
		t.Fatalf("the new head must wait for approval: %v", err)
	}
}

// On a rule the user approves, a request for changes is followed by a review
// of the new commits only, which settles the earlier blocking findings.
func TestOpenPriorFindingCarriesOverUntilItIsFixed(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}}))
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })

	h.fake.mu.Lock()
	h.fake.discussion.Threads = []DiscussionThread{{Key: priorKey(pr.HeadSHA, "O001"), Replies: []DiscussionComment{}}, {Key: priorKey(pr.HeadSHA, "S001"), Replies: []DiscussionComment{}}}
	h.fake.mu.Unlock()
	h.setOutput(settlingOutput(t, nil, func(PriorFinding) string { return priorOpen }))
	second := pr
	second.HeadSHA = sha("c")
	h.fake.open(second, h.rule(pr.Repository))
	h.pollUntil("the second review", func() bool { return h.publications() == 2 })
	published, _, started, _ := h.fake.snapshot()
	if len(started) != 3 || !strings.Contains(published[1].Body, "incremental review") {
		t.Fatalf("the re-review must be incremental with the incremental reviewer only, started %d sessions: %s", len(started), published[1].Body)
	}
	if published[1].Decision != DecisionRequestChanges || len(published[1].Comments) != 0 || !strings.Contains(published[1].Marker, ":findings=2:") || strings.Count(published[1].Body, "**Still open, blocking**") != 2 {
		t.Fatalf("open prior findings must keep blocking without new threads: decision=%s comments=%d marker=%s body=%s", published[1].Decision, len(published[1].Comments), published[1].Marker, published[1].Body)
	}

	h.setOutput(reviewerOutput(t, nil))
	third := pr
	third.HeadSHA = sha("d")
	h.fake.open(third, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, "Review ID: "+third.ReviewID()) })
	if !h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) {
		t.Fatal("fixed prior findings must leave the review clean")
	}
	h.fake.mu.Lock()
	reads := h.fake.discussionReads
	h.fake.mu.Unlock()
	if len(reads) != 2 || len(reads[1]) != 2 || reads[1][0].HeadSHA != pr.HeadSHA {
		t.Fatalf("the third review must still carry the findings of the first, read %+v", reads)
	}
}

func TestRewrittenHistoryGetsAFullReviewThatSettlesPriorFindings(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}}))
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })
	h.setOutput(reviewerOutput(t, nil))
	rebased := pr
	rebased.HeadSHA = sha("c")
	h.fake.mu.Lock()
	h.fake.rewritten[rebased.HeadSHA] = true
	h.fake.mu.Unlock()
	h.fake.open(rebased, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, "Review ID: "+rebased.ReviewID()) })
	_, _, started, _ := h.fake.snapshot()
	if len(started) != 4 || !h.fake.hasNote(policy.LevelAction, "Scope: full") {
		t.Fatalf("a rebased head must get a full review by every reviewer, started %d sessions", len(started))
	}
	h.fake.mu.Lock()
	reads := len(h.fake.discussionReads)
	h.fake.mu.Unlock()
	if reads != 1 {
		t.Fatal("the full review must still settle the prior findings")
	}
}

func TestMissingPreviousReviewFallsBackToAFullReview(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}}))
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })
	if _, err := h.store.db.Exec("DELETE FROM review_steps"); err != nil {
		t.Fatal(err)
	}
	h.setOutput(reviewerOutput(t, nil))
	next := pr
	next.HeadSHA = sha("c")
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, "Review ID: "+next.ReviewID()) })
	_, _, started, _ := h.fake.snapshot()
	h.fake.mu.Lock()
	reads := len(h.fake.discussionReads)
	h.fake.mu.Unlock()
	if len(started) != 4 || reads != 0 {
		t.Fatalf("without the previous result the review must be full and carry no prior findings: started %d, discussion reads %d", len(started), reads)
	}
}

// An open prior finding whose thread was resolved has no thread left to
// answer, so the review publishes it again instead of only naming it.
func TestOpenPriorFindingWithoutAnOpenThreadIsPublishedAgain(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}}))
	pr := integrationPullRequest("acme/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })
	h.fake.mu.Lock()
	h.fake.discussion.Threads = []DiscussionThread{{Key: priorKey(pr.HeadSHA, "O001"), Resolved: true, Replies: []DiscussionComment{}}, {Key: priorKey(pr.HeadSHA, "S001"), Replies: []DiscussionComment{}}}
	h.fake.mu.Unlock()
	h.setOutput(settlingOutput(t, nil, func(PriorFinding) string { return priorOpen }))
	next := pr
	next.HeadSHA = sha("c")
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the second review", func() bool { return h.publications() == 2 })
	published, _, _, _ := h.fake.snapshot()
	review := published[1]
	if review.Decision != DecisionRequestChanges || len(review.Comments) != 1 || !strings.Contains(review.Comments[0].Body, "Still open from O001") || !strings.Contains(review.Marker, ":findings=2:") {
		t.Fatalf("decision=%s comments=%+v marker=%s", review.Decision, review.Comments, review.Marker)
	}
}

func TestHumanReviewSignalHoldsACleanAutomaticApprovalForTheUser(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.deleted = []string{"src/kept.go", "pkg/tests/OwnerTest.php"}
	pr := integrationPullRequest("risky/widget", sha("a"))
	pr.Labels = []string{"Needs-Human"}
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	if !h.fake.hasNote(policy.LevelAction, "human review signals: label needs-human; deleted test files pkg/tests/OwnerTest.php.") || h.publications() != 0 {
		t.Fatal("the approval request must name the matched signals and nothing may be posted yet")
	}
	if _, err := h.approveWhenAwaiting(pr); err != nil {
		t.Fatal(err)
	}
	h.waitFor("the approval", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionApprove || !strings.Contains(published[0].Body, "waited for a human approval because the pull request matched: label needs-human") {
		t.Fatalf("decision=%s body=%s", published[0].Decision, published[0].Body)
	}
}

func TestHumanReviewWithoutAMatchApprovesWithoutTheUser(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.deleted = []string{"src/removed.go"}
	pr := integrationPullRequest("risky/widget", sha("a"))
	pr.Labels = []string{"enhancement"}
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("an automatic approval", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionApprove || strings.Contains(published[0].Body, "waited for a human") || h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) {
		t.Fatalf("an unmatched review must approve without the user: %s", published[0].Body)
	}
}
