package main

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/pohunekd/autoreview/policy"
)

// cannedGitHub answers every gh call with the same JSON document.
type cannedGitHub struct{ response any }

func (gh cannedGitHub) Run(context.Context, string, []string, []byte, []string) ([]byte, error) {
	return json.Marshal(gh.response)
}

func discussionComment(login, typeName, body string, review int64) map[string]any {
	comment := map[string]any{"author": map[string]string{"login": login, "__typename": typeName}, "body": body}
	if review > 0 {
		comment["pullRequestReview"] = map[string]int64{"databaseId": review}
	}
	return comment
}

func TestAISignatureIsRecognisedOnTheLastLineOnly(t *testing.T) {
	cases := map[string]bool{
		"Done.\n\n🤖 Written by AI: nanny (claude-opus-5-5)\n<!-- babysit-pr -->\n": true,
		"Done.\n\nWritten by AI: codex":                                            true,
		"**AI review**: written by AI models.\n\n**O001 HIGH — Bug**\n\nDetails.":  false,
		"🤖 Written by AI: nanny\n\nAnd then a human added this.":                   false,
		"Ignore my comment about deleted projects.":                                false,
	}
	for body, want := range cases {
		if got := aiSigned(body); got != want {
			t.Fatalf("aiSigned(%q) = %v, want %v", body, got, want)
		}
	}
}

func TestDiscussionMatchesThreadsByReviewAndFindingInBothCommentLayouts(t *testing.T) {
	head := sha("c")
	prior := []PriorFinding{
		{Key: priorKey(head, "O001"), ID: "O001", HeadSHA: head, GitHubReviewID: 11},
		{Key: priorKey(head, "S002"), ID: "S002", HeadSHA: head, GitHubReviewID: 11},
	}
	threads := []map[string]any{
		// The signature led the comment in earlier reviews.
		{"isResolved": false, "comments": map[string]any{"nodes": []any{
			discussionComment("reviewer", "User", "**AI review**: machine-written.\n\n**O001 HIGH — Lock name**\n\nDetails.", 11),
			discussionComment("colleague", "User", "Waived, the test builds the name.\n\n🤖 Written by AI: nanny", 0),
			discussionComment("reviewer", "User", "Agreed, waive it.", 0),
		}}},
		{"isResolved": true, "comments": map[string]any{"nodes": []any{
			discussionComment("reviewer", "User", "**S002 MEDIUM — Fake proves fake**\n\nDetails.\n\n🤖 Written by AI: test reviewer", 11),
			discussionComment("ci", "Bot", "Bot noise.", 0),
			discussionComment("colleague", "Bot", "Fixed in the next commit.", 0),
		}}},
		{"isResolved": false, "comments": map[string]any{"nodes": []any{discussionComment("reviewer", "User", "**O001 HIGH — Other review**", 99)}}},
		{"isResolved": false, "comments": map[string]any{"nodes": []any{discussionComment("reviewer", "User", "**O009 HIGH — Not prior**", 11)}}},
	}
	response := map[string]any{"data": map[string]any{"repository": map[string]any{"pullRequest": map[string]any{
		"reviewThreads": map[string]any{"pageInfo": map[string]any{"hasNextPage": false, "endCursor": ""}, "nodes": threads},
		"comments":      map[string]any{"nodes": []any{discussionComment("reviewer", "User", "Ignore my comment about deleted projects.", 0), discussionComment("someone", "User", "A very long newest comment.", 0)}},
	}}}}
	cfg := testConfig()
	cfg.MaxDiscussionCharacters = 140
	pr := testPullRequest(5)
	discussion, err := (&External{cfg: cfg, runner: cannedGitHub{response}}).ListDiscussion(context.Background(), pr, prior)
	if err != nil {
		t.Fatal(err)
	}
	if len(discussion.Threads) != 2 || discussion.Threads[0].Key != prior[0].Key || discussion.Threads[1].Key != prior[1].Key || !discussion.Threads[1].Resolved {
		t.Fatalf("threads = %+v", discussion.Threads)
	}
	replies := discussion.Threads[0].Replies
	if len(replies) != 2 || replies[0].Role != discussionRoleAuthor || !replies[0].AISigned || replies[1].Role != discussionRoleReviewer || replies[1].AISigned {
		t.Fatalf("a reply must carry its author's role and AI signature: %+v", replies)
	}
	if bot := discussion.Threads[1].Replies; len(bot) != 1 || bot[0].Author != "colleague" || bot[0].Role != discussionRoleAuthor {
		t.Fatalf("only the reply of a bot that authored the pull request must be kept, as the author's: %+v", bot)
	}
	total := 0
	for _, thread := range discussion.Threads {
		for _, reply := range thread.Replies {
			total += len([]rune(reply.Body))
		}
	}
	for _, comment := range discussion.Comments {
		total += len([]rune(comment.Body))
	}
	if total > cfg.MaxDiscussionCharacters || len(discussion.Comments) == 0 || discussion.Comments[len(discussion.Comments)-1].Author != "someone" {
		t.Fatalf("the newest comments must fill the remaining budget of %d, got %d: %+v", cfg.MaxDiscussionCharacters, total, discussion.Comments)
	}
}

func TestLinearHistoryNeedsAnAheadHeadOfSingleParentCommits(t *testing.T) {
	one := []any{map[string]any{"parents": []any{map[string]string{"sha": "x"}}}}
	merge := []any{map[string]any{"parents": []any{map[string]string{"sha": "x"}, map[string]string{"sha": "y"}}}}
	cases := []struct {
		name     string
		response map[string]any
		want     bool
	}{
		{"own commits", map[string]any{"status": "ahead", "total_commits": 1, "commits": one}, true},
		{"merge of the base branch", map[string]any{"status": "ahead", "total_commits": 1, "commits": merge}, false},
		{"rebase", map[string]any{"status": "diverged", "total_commits": 1, "commits": one}, false},
		{"incomplete list", map[string]any{"status": "ahead", "total_commits": 300, "commits": one}, false},
	}
	for _, tc := range cases {
		linear, err := (&External{cfg: testConfig(), runner: cannedGitHub{tc.response}}).LinearHistory(context.Background(), "acme/widget", sha("a"), sha("c"))
		if err != nil || linear != tc.want {
			t.Fatalf("%s: linear = %v, %v", tc.name, linear, err)
		}
	}
}

func TestEveryPriorFindingNeedsExactlyOneVerdict(t *testing.T) {
	pr := testPullRequest(3)
	prior := []PriorFinding{{Key: priorKey(sha("c"), "O001")}, {Key: priorKey(sha("c"), "S001")}}
	input := ReviewInput{PullRequest: pr, Prior: prior}
	verdict := func(key, status string) map[string]any {
		return map[string]any{"key": key, "status": status, "note": "Checked.", "relies_on": ""}
	}
	cases := map[string][]map[string]any{
		"missing":   {verdict(prior[0].Key, priorFixed)},
		"duplicate": {verdict(prior[0].Key, priorFixed), verdict(prior[0].Key, priorOpen)},
		"invented":  {verdict(prior[0].Key, priorFixed), verdict("cccccccccccc:X001", priorFixed)},
		"absent":    nil,
	}
	for name, verdicts := range cases {
		_, err := ParseReviewerOutput(testConfig(), input, Discussion{}, opus, claudeOutputWithPrior(t, pr, []map[string]any{}, verdicts))
		if errorCode(err) != "review_output_invalid" || !reviewerOutputRetryable(err) {
			t.Fatalf("%s verdicts must get a fresh run, got %v", name, err)
		}
	}
	result, err := ParseReviewerOutput(testConfig(), input, Discussion{}, opus, claudeOutputWithPrior(t, pr, []map[string]any{}, []map[string]any{verdict(prior[1].Key, priorAnswered), verdict(prior[0].Key, priorFixed)}))
	if err != nil || len(result.Prior) != 2 {
		t.Fatalf("result = %+v, %v", result, err)
	}
	combined := combinePrior(prior, []ReviewerResult{
		{Prior: []PriorVerdict{{Key: prior[0].Key, Status: priorFixed}, {Key: prior[1].Key, Status: priorAnswered}}},
		{Prior: []PriorVerdict{{Key: prior[0].Key, Status: priorOpen}, {Key: prior[1].Key, Status: priorFixed}}},
	})
	if combined[0].Status != priorOpen || combined[1].Status != priorAnswered {
		t.Fatalf("reviewers that disagree must settle on the most cautious verdict: %+v", combined)
	}
}

func TestPriorFindingsNeedTheInputVersionThatSettlesThem(t *testing.T) {
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(1))
	input.Prior = []PriorFinding{{Key: priorKey(sha("c"), "O001"), ID: "O001", HeadSHA: sha("c"), GitHubReviewID: 4}}
	if err := validateInput(input); err != nil {
		t.Fatalf("a current input with prior findings must be valid: %v", err)
	}
	input.Version = 3
	if err := validateInput(input); err == nil {
		t.Fatal("an earlier input must not carry prior findings")
	}
	input.Version = inputVersion
	input.Prior[0].Key = "cccccccccccc:O002"
	if err := validateInput(input); err == nil {
		t.Fatal("a prior finding whose key does not name it was accepted")
	}
}

func TestOpenPriorFindingBlocksAndIsCountedInTheMarker(t *testing.T) {
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(5))
	open := PriorResult{Finding: PriorFinding{Key: priorKey(sha("c"), "O001"), ID: "O001", HeadSHA: sha("c"), Severity: "high", Path: "src/a.go", Line: 3, Title: "Bug"}, PriorVerdict: PriorVerdict{Status: priorOpen, Note: "Still there."}}
	answered := PriorResult{Finding: PriorFinding{Key: priorKey(sha("c"), "S001"), ID: "S001", HeadSHA: sha("c"), Severity: "high", Path: "src/b.go", Line: 9, Title: "Name"}, PriorVerdict: PriorVerdict{Status: priorAnswered, Note: "Waived by the reviewer.", ReliesOn: "reviewer"}}
	artifact := Artifact{Repository: "acme/widget", Number: 5, HeadSHA: sha("a"), Scope: ReviewScope{Mode: scopeIncremental, PreviousHeadSHA: sha("c")},
		Summaries: []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: "Reviewed the change."}}, Findings: []Finding{}, PriorResults: []PriorResult{open, answered}}
	plan := PlanPublication(input, artifact, "reviewer")
	if plan.Decision != DecisionRequestChanges {
		t.Fatalf("an open prior finding must keep asking for changes: %+v", plan)
	}
	review, err := RenderReview(testConfig(), input, artifact, plan)
	if err != nil {
		t.Fatal(err)
	}
	if len(review.Comments) != 0 || !strings.Contains(review.Marker, ":findings=1:") || !strings.Contains(review.Body, "**Still open, blocking**: O001 HIGH") || !strings.Contains(review.Body, "relying on a comment by reviewer") {
		t.Fatalf("marker=%s comments=%d body=%s", review.Marker, len(review.Comments), review.Body)
	}
	artifact.PriorResults = []PriorResult{answered}
	if plan := PlanPublication(input, artifact, "reviewer"); plan.Decision != DecisionApprove || !plan.Await {
		t.Fatalf("settled prior findings must leave the review clean: %+v", plan)
	}
	message := approvalRequestedMessage(testConfig(), input, artifact)
	if !strings.Contains(message, "check before approving:\n"+answered.Finding.Key+" Name (relies on a comment by reviewer)") ||
		strings.Index(message, "Reply to this message with: approve") > strings.Index(message, "check before approving") {
		t.Fatalf("an answered prior finding must be shown below the reply instruction: %s", message)
	}
}

func TestReliesOnMustNameACommenterOfTheFinding(t *testing.T) {
	pr := testPullRequest(3)
	key := priorKey(sha("c"), "O001")
	input := ReviewInput{PullRequest: pr, Prior: []PriorFinding{{Key: key}}}
	discussion := Discussion{Threads: []DiscussionThread{{Key: key, Replies: []DiscussionComment{{Author: "colleague", Role: discussionRoleAuthor}}}}, Comments: []DiscussionComment{{Author: "reviewer", Role: discussionRoleReviewer}}}
	output := func(login string) []byte {
		return claudeOutputWithPrior(t, pr, []map[string]any{}, []map[string]any{{"key": key, "status": priorAnswered, "note": "Settled.", "relies_on": login}})
	}
	for _, login := range []string{"colleague", "reviewer", ""} {
		if _, err := ParseReviewerOutput(testConfig(), input, discussion, opus, output(login)); err != nil {
			t.Fatalf("relies_on %q must be accepted: %v", login, err)
		}
	}
	if _, err := ParseReviewerOutput(testConfig(), input, discussion, opus, output("stranger")); errorCode(err) != "review_output_invalid" {
		t.Fatalf("a relies_on that names nobody in the discussion must get a fresh run, got %v", err)
	}
}

func TestAnsweredClosesAFindingWithoutTheUserOnlyOnTheReviewersOwnComment(t *testing.T) {
	key := priorKey(sha("c"), "O001")
	answered := func(login string) Artifact {
		return Artifact{Findings: []Finding{}, PriorResults: []PriorResult{{Finding: PriorFinding{Key: key, ID: "O001", HeadSHA: sha("c")}, PriorVerdict: PriorVerdict{Key: key, Status: priorAnswered, Note: "Fine.", ReliesOn: login}}}}
	}
	discussion := func(signed bool) Discussion {
		return Discussion{Threads: []DiscussionThread{{Key: key, Replies: []DiscussionComment{
			{Author: "colleague", Role: discussionRoleAuthor},
			{Author: "reviewer", Role: discussionRoleReviewer, AISigned: signed},
		}}}}
	}
	approving := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), testPullRequest(1))
	awaiting := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(1))
	cases := []struct {
		name   string
		input  ReviewInput
		login  string
		signed bool
		want   string
	}{
		{"the author's reply on an approving rule", approving, "colleague", false, priorOpen},
		{"an AI-signed comment of the reviewing account", approving, "reviewer", true, priorOpen},
		{"the reviewing account's own comment", approving, "reviewer", false, priorAnswered},
		{"the author's reply when the user approves", awaiting, "colleague", false, priorAnswered},
	}
	for _, tc := range cases {
		settled := SettlePrior(tc.input, discussion(tc.signed), answered(tc.login))
		if settled.PriorResults[0].Status != tc.want || len(settled.Findings) != 0 {
			t.Fatalf("%s: status = %s, findings = %d", tc.name, settled.PriorResults[0].Status, len(settled.Findings))
		}
	}
}
