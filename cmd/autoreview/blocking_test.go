package main

import (
	"slices"
	"strings"
	"testing"

	"github.com/pohunekd/autoreview/policy"
)

func TestOnlyBlockingFindingsDecideThePublication(t *testing.T) {
	note := Finding{ID: "O001", NonBlocking: true}
	blocking := Finding{ID: "S001"}
	cases := []struct {
		name     string
		version  int
		onClean  policy.OnClean
		findings []Finding
		want     PublicationPlan
	}{
		{"notes only follow the clean outcome", inputVersion, policy.CleanAwait, []Finding{note}, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"notes only are approved on an approving rule", inputVersion, policy.CleanApprove, []Finding{note}, PublicationPlan{Decision: DecisionApprove}},
		{"one blocking finding asks for changes", inputVersion, policy.CleanAwait, []Finding{note, blocking}, PublicationPlan{Decision: DecisionRequestChanges}},
		{"a stored earlier input keeps every finding blocking", 2, policy.CleanAwait, []Finding{note}, PublicationPlan{Decision: DecisionRequestChanges}},
	}
	for _, tc := range cases {
		input := testInput(testRule(opus, policy.FindingsRequestChanges, tc.onClean), testPullRequest(1))
		input.Version = tc.version
		if err := validateInput(input); err != nil {
			t.Fatalf("%s: input version %d must stay valid: %v", tc.name, tc.version, err)
		}
		if plan := PlanPublication(input, Artifact{Findings: tc.findings}, "reviewer"); plan != tc.want {
			t.Fatalf("%s: plan = %+v, want %+v", tc.name, plan, tc.want)
		}
	}
}

func TestNotesGoToTheAuthorOnceBeforeTheUserApproves(t *testing.T) {
	note := Finding{ID: "O001", NonBlocking: true}
	blocking := Finding{ID: "S001"}
	handoff := &NotesHandoff{WorkflowID: "pr-review-earlier", HeadSHA: strings.Repeat("e", 40)}
	cases := []struct {
		name     string
		author   string
		bot      bool
		handoff  *NotesHandoff
		findings []Finding
		want     PublicationPlan
	}{
		{"notes only go to the author", "colleague", false, nil, []Finding{note}, PublicationPlan{Decision: DecisionComment, NotesToAuthor: true}},
		{"a clean review waits for the user", "colleague", false, nil, nil, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"a blocking finding asks for changes", "colleague", false, nil, []Finding{note, blocking}, PublicationPlan{Decision: DecisionRequestChanges}},
		{"notes after a hand-off wait for the user", "colleague", false, handoff, []Finding{note}, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"notes of a bot wait for the user", "devin-ai-integration", true, nil, []Finding{note}, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"an own pull request gets a plain comment", "reviewer", false, nil, []Finding{note}, PublicationPlan{Decision: DecisionComment, OwnPullRequest: true}},
	}
	for _, tc := range cases {
		rule := testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait)
		rule.OnNotes = policy.NotesComment
		pr := testPullRequest(1)
		pr.Author, pr.AuthorBot = tc.author, tc.bot
		input := testInput(rule, pr)
		input.Handoff = tc.handoff
		if plan := PlanPublication(input, Artifact{Findings: tc.findings}, "reviewer"); plan != tc.want {
			t.Fatalf("%s: plan = %+v, want %+v", tc.name, plan, tc.want)
		}
	}
}

func TestNotesHandedToTheAuthorAskForARenewedRequest(t *testing.T) {
	rule := testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait)
	rule.OnNotes = policy.NotesComment
	input := testInput(rule, testPullRequest(1))
	artifact := Artifact{Findings: []Finding{{ID: "O001", NonBlocking: true, Severity: "low", Confidence: "high", Path: "a.go", Line: 1, Title: "Naming", Body: "Could be clearer."}}}
	plan := PlanPublication(input, artifact, "reviewer")
	review, err := RenderReview(testConfig(), input, artifact, plan)
	if err != nil {
		t.Fatal(err)
	}
	if review.Decision != DecisionComment || len(review.Comments) != 0 || !strings.Contains(review.Body, "Request this review again once you have decided") || !strings.Contains(review.Body, "1 non-blocking note(s)") {
		t.Fatalf("decision=%s comments=%d body=%s", review.Decision, len(review.Comments), review.Body)
	}
}

func TestClaudeFindingBlocksUnlessItSaysOtherwise(t *testing.T) {
	pr := testPullRequest(3)
	output := claudeOutput(t, pr, []map[string]any{
		{"id": "C001", "severity": "high", "confidence": "high", "blocking": true, "path": "a.go", "line": 1, "side": "RIGHT", "title": "Bug", "body": "Details."},
		{"id": "C002", "severity": "low", "confidence": "high", "blocking": false, "path": "a.go", "line": 2, "side": "RIGHT", "title": "Note", "body": "Details."},
		{"id": "C003", "severity": "medium", "confidence": "high", "path": "a.go", "line": 3, "side": "RIGHT", "title": "Unflagged", "body": "Details."},
		{"id": "C004", "severity": "medium", "confidence": "high", "blocking": true, "path": "a.go", "line": 4, "side": "RIGHT", "title": "Below the floor", "body": "Details."},
	})
	result, err := ParseReviewerOutput(testConfig(), ReviewInput{PullRequest: pr}, Discussion{}, opus, output)
	if err != nil {
		t.Fatal(err)
	}
	got := []bool{}
	for _, finding := range result.Findings {
		got = append(got, !finding.NonBlocking)
	}
	if want := []bool{true, false, true, false}; !slices.Equal(got, want) {
		t.Fatalf("blocking = %v, want %v: a flagless finding blocks, a blocking one needs high severity", got, want)
	}
}

func TestFindingOutsideTheChangedFilesNeverBlocks(t *testing.T) {
	artifact := AnchorFindings(Artifact{Findings: []Finding{
		{ID: "O001", Path: "src/a.go", Line: 14},
		{ID: "S001", Path: "src/base-pull-request.go", Line: 3},
	}}, CommentableLines{"src/a.go": {10}})
	if artifact.Findings[0].NonBlocking || !artifact.Findings[1].NonBlocking {
		t.Fatalf("findings = %+v", artifact.Findings)
	}
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(1))
	if plan := PlanPublication(input, Artifact{Findings: artifact.Findings[1:]}, "reviewer"); plan.Decision != DecisionApprove {
		t.Fatalf("a finding in a file the pull request does not change asked for changes: %+v", plan)
	}
}

func TestNotesAreCollapsedWithoutInlineCommentsOrCount(t *testing.T) {
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(5))
	artifact := Artifact{
		Repository: "acme/widget", Number: 5, HeadSHA: sha("a"), Scope: ReviewScope{Mode: scopeFull},
		Summaries: []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: "Reviewed the change."}},
		Findings: []Finding{
			{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/a.go", Line: 3, Title: "Bug", Body: "Breaks.", Anchored: true},
			{ID: "O002", ReviewerLabel: "Opus", Severity: "low", Confidence: "high", Path: "src/a.go", Line: 4, Title: "Naming", Body: "Rename it:\n```go\nfunc better() {", Anchored: true, NonBlocking: true},
		},
	}
	review, err := RenderReview(testConfig(), input, artifact, PublicationPlan{Decision: DecisionRequestChanges})
	if err != nil {
		t.Fatal(err)
	}
	if len(review.Comments) != 1 || review.Comments[0].Line != 3 {
		t.Fatalf("a note must not open an inline thread: %+v", review.Comments)
	}
	if !strings.Contains(review.Marker, ":findings=1:") {
		t.Fatalf("the marker must count blocking findings only: %s", review.Marker)
	}
	notes := review.Body[strings.Index(review.Body, "<details>"):]
	if !strings.Contains(notes, "1 non-blocking note(s): no action required in this pull request") || !strings.Contains(notes, "O002") {
		t.Fatalf("notes section = %s", notes)
	}
	if !strings.Contains(notes, "```\n\n</details>") {
		t.Fatalf("an unclosed fence of a note must be closed before the section ends:\n%s", notes)
	}
	message := approvalRequestedMessage(testConfig(), input, Artifact{Findings: artifact.Findings[1:]})
	if !strings.HasPrefix(message, approvalRequestedTitle) || !strings.Contains(message, "Blocking findings: 0\nNon-blocking notes: 1") || !strings.Contains(message, "O002 [NOTE, LOW") {
		t.Fatalf("approval request = %s", message)
	}
	if strings.Index(message, "Reply to this message with: approve") > strings.Index(message, "Notes:") {
		t.Fatalf("the reply instruction must precede the notes, which a message limit may cut: %s", message)
	}
}

// signedLastLine requires the last line, ignoring blank and HTML comment
// lines, to start with "Written by AI:" after an optional emoji and to be
// outside a code block.
func signedLastLine(t *testing.T, text string) {
	t.Helper()
	lines := strings.Split(text, "\n")
	last := len(lines) - 1
	for last >= 0 && (strings.TrimSpace(lines[last]) == "" || strings.HasPrefix(strings.TrimSpace(lines[last]), "<!--")) {
		last--
	}
	if last < 0 || !strings.HasPrefix(strings.TrimLeft(lines[last], "🤖️ "), "Written by AI:") {
		t.Fatalf("not signed on its last line:\n%s", text)
	}
	if closeOpenFence(strings.Join(lines[:last], "\n")) != strings.Join(lines[:last], "\n") {
		t.Fatalf("the signature is inside an unclosed code block:\n%s", text)
	}
}

func TestEveryPublishedBodyEndsWithTheSignatureOutsideModelCode(t *testing.T) {
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanAwait), testPullRequest(5))
	unclosed := "Use this:\n```go\nfunc open() {"
	summary := []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: "Reviewed the change."}}
	finding := Finding{ID: "O001", ReviewerLabel: "Opus", Severity: "high", Confidence: "high", Path: "src/a.go", Line: 3, Title: "Bug", Body: unclosed}
	anchored, note := finding, finding
	anchored.Anchored = true
	note.Anchored, note.NonBlocking = true, true
	// Each source of model text on its own, so no later block closes an
	// earlier one by chance.
	cases := map[string]Artifact{
		"summary":            {Summaries: []ReviewerSummary{{ReviewerID: "opus", Label: "Opus", Summary: unclosed}}, Findings: []Finding{}},
		"inline finding":     {Summaries: summary, Findings: []Finding{anchored}},
		"unanchored finding": {Summaries: summary, Findings: []Finding{finding}},
		"note":               {Summaries: summary, Findings: []Finding{note}},
	}
	for name, artifact := range cases {
		artifact.Repository, artifact.Number, artifact.HeadSHA, artifact.Scope = "acme/widget", 5, sha("a"), ReviewScope{Mode: scopeFull}
		review, err := RenderReview(testConfig(), input, artifact, PublicationPlan{Decision: DecisionRequestChanges})
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		signedLastLine(t, review.Body)
		for _, comment := range review.Comments {
			signedLastLine(t, comment.Body)
		}
	}
}
