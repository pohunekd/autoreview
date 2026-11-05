package main

import (
	"strings"
	"testing"

	"github.com/pohunekd/autoreview/policy"
)

// opusReports lets the opus reviewer report one blocking finding of the
// severity and every other reviewer report none.
func opusReports(t *testing.T, severity string) func(SessionRequest) []byte {
	finding := reviewerOutput(t, []map[string]any{{"severity": severity, "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}})
	clean := reviewerOutput(t, nil)
	return func(request SessionRequest) []byte {
		if request.Reviewer.ID == "opus" {
			return finding(request)
		}
		return clean(request)
	}
}

func every(verdict string) func(claimFinding) string {
	return func(claimFinding) string { return verdict }
}

func everyPrior(verdict string) func(claimPrior) string {
	return func(claimPrior) string { return verdict }
}

func TestRefutedFindingIsPublishedAsANoteAndTheReviewApproved(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(verifying(t, opusReports(t, "high"), every(verdictRefuted), everyPrior(verdictUpheld)))
	pr := integrationPullRequest("checked/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the automatic approval", func() bool { return h.publications() == 1 })
	published, notes, started, open := h.fake.snapshot()
	review := published[0]
	if review.Decision != DecisionApprove || len(review.Comments) != 0 || !strings.Contains(review.Marker, ":findings=0:") || len(started) != 3 || open != 0 {
		t.Fatalf("decision=%s comments=%d marker=%s started=%d open=%d", review.Decision, len(review.Comments), review.Marker, len(started), open)
	}
	if !strings.Contains(review.Body, "1 non-blocking note(s)") || !strings.Contains(review.Body, "Refuted by the verification by Opus: Traced the scenario at the head.") ||
		!strings.Contains(review.Body, "Verification by Opus: blocking findings 0 confirmed, 0 not blocking, 1 refuted.") {
		t.Fatalf("the refuted finding must stay visible as a note with the reason:\n%s", review.Body)
	}
	if countNotes(notes, policy.LevelAction, "") != 0 {
		t.Fatalf("one refuted high finding must not ping the user: %+v", notes)
	}
}

func TestDemotedCriticalFindingOnAnApprovingRuleWaitsForTheUser(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(verifying(t, opusReports(t, "critical"), every(verdictNotBlocking), everyPrior(verdictUpheld)))
	pr := integrationPullRequest("checked/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the approval request", func() bool { return h.fake.hasNote(policy.LevelAction, approvalRequestedTitle) })
	if !h.fake.hasNote(policy.LevelAction, "the verification demoted these blocking findings, check before approving:\nO001 [NOTE, NOT BLOCKING BY VERIFICATION, CRITICAL") || h.publications() != 0 {
		t.Fatal("the approval request must name the demoted critical finding and nothing may be posted yet")
	}
	if _, err := h.approveWhenAwaiting(pr); err != nil {
		t.Fatal(err)
	}
	h.waitFor("the approval", func() bool { return h.publications() == 1 })
	published, _, _, _ := h.fake.snapshot()
	if published[0].Decision != DecisionApprove || !strings.Contains(published[0].Body, "Not blocking according to the verification by Opus") {
		t.Fatalf("decision=%s body=%s", published[0].Decision, published[0].Body)
	}
}

func TestConfirmedFindingsStillRequestChanges(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(reviewerOutput(t, []map[string]any{{"severity": "high", "confidence": "high", "blocking": true, "path": "src/a.go", "line": 3, "title": "Broken", "body": "It breaks."}}))
	pr := integrationPullRequest("checked/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })
	published, _, started, _ := h.fake.snapshot()
	if published[0].Decision != DecisionRequestChanges || len(published[0].Comments) != 2 || !strings.Contains(published[0].Body, "blocking findings 2 confirmed") {
		t.Fatalf("decision=%s comments=%d body=%s", published[0].Decision, len(published[0].Comments), published[0].Body)
	}
	if len(started) != 3 || started[2].Reviewer.ID != policy.VerifierReviewerID || started[2].Reviewer.AgentProfile != "claude-profile" {
		t.Fatalf("one Opus verifier session must follow the reviewers: %+v", started)
	}
	ids := []string{}
	for _, claim := range requestClaims(t, started[2].Prompt).Findings {
		ids = append(ids, claim.ID)
	}
	if strings.Join(ids, ",") != "O001,S001" {
		t.Fatalf("the verifier must check every blocking finding, got %v", ids)
	}
}

func TestFailedVerifierPublishesTheFindingsUnverified(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	reviewers := opusReports(t, "high")
	h.setOutput(func(request SessionRequest) []byte {
		if request.Reviewer.ID == policy.VerifierReviewerID {
			return []byte("not a claude result\n")
		}
		return reviewers(request)
	})
	pr := integrationPullRequest("checked/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })
	published, _, started, open := h.fake.snapshot()
	review := published[0]
	if review.Decision != DecisionRequestChanges || len(review.Comments) != 1 || !strings.Contains(review.Body, "did not finish (review_wrapper_invalid)") {
		t.Fatalf("a failed verification must keep the finding blocking: decision=%s comments=%d body=%s", review.Decision, len(review.Comments), review.Body)
	}
	if len(started) != 4 || open != 0 {
		t.Fatalf("the verifier must get its fresh run and leave no session: started=%d open=%d", len(started), open)
	}
}

func TestRestartDuringVerificationResumesTheSameSession(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	reviewers := opusReports(t, "high")
	h.setOutput(func(request SessionRequest) []byte {
		if request.Reviewer.ID == policy.VerifierReviewerID {
			return nil
		}
		return reviewers(request)
	})
	pr := integrationPullRequest("checked/widget", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the verifier session", func() bool { return h.startedCount() == 3 })
	h.stopEngine()
	h.start()
	h.setOutput(verifying(t, reviewers, every(verdictConfirmed), everyPrior(verdictUpheld)))
	h.pollUntil("the publication after the restart", func() bool { return h.publications() == 1 })
	published, _, started, _ := h.fake.snapshot()
	if len(started) != 3 || published[0].Decision != DecisionRequestChanges {
		t.Fatalf("the restart must resume the verifier session: started=%d decision=%s", len(started), published[0].Decision)
	}
	h.waitFor("the verifier session to be removed", func() bool { _, _, _, open := h.fake.snapshot(); return open == 0 })
}

// An answered prior finding the verifier reopens has no thread left, so the
// review publishes it again: the verifier runs before the republication.
func TestReopenedAnsweredPriorFindingIsPublishedAgain(t *testing.T) {
	h := startHarness(t, nil)
	h.fake.commentable = CommentableLines{"src/a.go": {3}}
	h.setOutput(opusReports(t, "high"))
	pr := integrationPullRequest("checked/manual", sha("a"))
	h.fake.open(pr, h.rule(pr.Repository))
	h.pollUntil("the request for changes", func() bool { return h.publications() == 1 })
	h.fake.mu.Lock()
	h.fake.discussion.Threads = []DiscussionThread{{Key: priorKey(pr.HeadSHA, "O001"), Resolved: true, Replies: []DiscussionComment{}}}
	h.fake.mu.Unlock()
	h.setOutput(verifying(t, settlingOutput(t, nil, func(PriorFinding) string { return priorAnswered }), every(verdictConfirmed), everyPrior(verdictReopened)))
	next := pr
	next.HeadSHA = sha("c")
	h.fake.open(next, h.rule(pr.Repository))
	h.pollUntil("the second review", func() bool { return h.publications() == 2 })
	published, _, _, _ := h.fake.snapshot()
	review := published[1]
	if review.Decision != DecisionRequestChanges || len(review.Comments) != 1 || !strings.Contains(review.Comments[0].Body, "Still open from O001") ||
		!strings.Contains(review.Body, "**Still open, blocking**, reopened by the verification, published again as P001") {
		t.Fatalf("decision=%s comments=%+v body=%s", review.Decision, review.Comments, review.Body)
	}
}
