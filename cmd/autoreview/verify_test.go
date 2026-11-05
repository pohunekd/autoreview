package main

import (
	"testing"

	"github.com/pohunekd/autoreview/policy"
)

func verifiedRule(onClean policy.OnClean) policy.Rule {
	rule := testRule(opus, policy.FindingsRequestChanges, onClean)
	rule.Pipeline.Verifier = &policy.Verifier{Reviewer: opus, Prompt: "Verify the claims.", EscalateSeverities: []string{"critical"}, EscalateCount: 2}
	return rule
}

// The escalation decides whether a review the verifier made clean is
// approved without the user.
func TestDemotedFindingsAskTheUserOnlyAboveThePolicyThreshold(t *testing.T) {
	demoted := func(severity string) Finding {
		return Finding{ID: "O001", Severity: severity, NonBlocking: true, Verdict: verdictRefuted}
	}
	cases := []struct {
		name     string
		version  int
		onClean  policy.OnClean
		findings []Finding
		want     PublicationPlan
	}{
		{"one demoted high finding is approved", inputVersion, policy.CleanApprove, []Finding{demoted("high")}, PublicationPlan{Decision: DecisionApprove}},
		{"a demoted critical finding waits", inputVersion, policy.CleanApprove, []Finding{demoted("critical")}, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"as many demoted findings as the count waits", inputVersion, policy.CleanApprove, []Finding{demoted("high"), demoted("high")}, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"a note the reviewer marked is not demoted", inputVersion, policy.CleanApprove, []Finding{{ID: "O001", Severity: "critical", NonBlocking: true}}, PublicationPlan{Decision: DecisionApprove}},
		{"an awaiting rule waits anyway", inputVersion, policy.CleanAwait, []Finding{demoted("high")}, PublicationPlan{Decision: DecisionApprove, Await: true}},
		{"a stored earlier input is not verified", priorFindingsVersion, policy.CleanApprove, []Finding{demoted("critical"), demoted("high")}, PublicationPlan{Decision: DecisionApprove}},
	}
	for _, tc := range cases {
		input := testInput(verifiedRule(tc.onClean), testPullRequest(1))
		input.Version = tc.version
		if plan := PlanPublication(input, Artifact{Findings: tc.findings}, "reviewer"); plan != tc.want {
			t.Fatalf("%s: plan = %+v, want %+v", tc.name, plan, tc.want)
		}
	}
}

func TestVerifierOutputNeedsExactlyOneVerdictPerClaim(t *testing.T) {
	pr := testPullRequest(3)
	input := testInput(verifiedRule(policy.CleanApprove), pr)
	claims := verificationClaims{Findings: []claimFinding{{ID: "O001"}, {ID: "S001"}}, Prior: []claimPrior{{Key: priorKey(sha("c"), "O001")}}}
	output := func(head string, findings, prior []map[string]any) []byte {
		return claudeStructuredOutput(map[string]any{"repository": pr.Repository, "number": pr.Number, "head_sha": head, "findings": findings, "prior_findings": prior})
	}
	check := func(id, verdict string) map[string]any {
		return map[string]any{"id": id, "verdict": verdict, "note": "Checked."}
	}
	priorCheck := map[string]any{"key": claims.Prior[0].Key, "verdict": verdictReopened, "note": "Checked."}
	cases := map[string][]byte{
		"missing verdict":   output(pr.HeadSHA, []map[string]any{check("O001", verdictConfirmed)}, []map[string]any{priorCheck}),
		"repeated verdict":  output(pr.HeadSHA, []map[string]any{check("O001", verdictConfirmed), check("O001", verdictRefuted)}, []map[string]any{priorCheck}),
		"invented finding":  output(pr.HeadSHA, []map[string]any{check("O001", verdictConfirmed), check("X009", verdictRefuted)}, []map[string]any{priorCheck}),
		"unknown verdict":   output(pr.HeadSHA, []map[string]any{check("O001", verdictConfirmed), check("S001", "maybe")}, []map[string]any{priorCheck}),
		"prior not settled": output(pr.HeadSHA, []map[string]any{check("O001", verdictConfirmed), check("S001", verdictRefuted)}, []map[string]any{}),
		"other head":        output(sha("d"), []map[string]any{check("O001", verdictConfirmed), check("S001", verdictRefuted)}, []map[string]any{priorCheck}),
	}
	for name, raw := range cases {
		if _, err := ParseVerifierOutput(testConfig(), input, claims, raw); errorCode(err) != "review_output_invalid" {
			t.Fatalf("%s: error = %v, want a retryable review_output_invalid", name, err)
		}
	}
	result, err := ParseVerifierOutput(testConfig(), input, claims, output(pr.HeadSHA, []map[string]any{check("S001", verdictRefuted), check("O001", verdictConfirmed)}, []map[string]any{priorCheck}))
	if err != nil || len(result.Findings) != 2 || len(result.Prior) != 1 {
		t.Fatalf("result = %+v, %v", result, err)
	}
}

func TestVerificationDemotesWithItsReasonAndKeepsEveryClaim(t *testing.T) {
	input := testInput(verifiedRule(policy.CleanAwait), testPullRequest(1))
	key := priorKey(sha("c"), "O001")
	artifact := Artifact{
		Findings: []Finding{
			{ID: "O001", Severity: "high", Body: "Breaks."},
			{ID: "O002", Severity: "high", Body: "Leaks."},
			{ID: "O003", Severity: "low", Body: "Naming.", NonBlocking: true},
		},
		PriorResults: []PriorResult{{Finding: PriorFinding{Key: key, ID: "O001"}, PriorVerdict: PriorVerdict{Key: key, Status: priorAnswered, Note: "The author explained it."}}},
	}
	claims := claimsOf(input, artifact)
	if len(claims.Findings) != 2 || claims.Findings[1].ID != "O002" || len(claims.Prior) != 1 {
		t.Fatalf("the claims are the blocking findings and the answered prior findings: %+v", claims)
	}
	verified := ApplyVerification(artifact, Verification{ReviewerLabel: "Opus",
		Findings: []FindingCheck{{ID: "O001", Verdict: verdictConfirmed, Note: "Reachable."}, {ID: "O002", Verdict: verdictRefuted, Note: "Guarded by the caller."}},
		Prior:    []PriorCheck{{Key: key, Verdict: verdictReopened, Note: "The code still does it."}},
	})
	if len(verified.Findings) != 3 || verified.Findings[0].NonBlocking || verified.Findings[0].Body != "Breaks." {
		t.Fatalf("a confirmed finding must stay as reported: %+v", verified.Findings[0])
	}
	if refuted := verified.Findings[1]; !refuted.NonBlocking || !refuted.demoted() || refuted.Body != "Refuted by the verification by Opus: Guarded by the caller.\n\nLeaks." {
		t.Fatalf("a refuted finding must become a note with the reason: %+v", refuted)
	}
	if prior := verified.PriorResults[0]; prior.Status != priorOpen || !prior.Reopened {
		t.Fatalf("a reopened prior finding must block again: %+v", prior)
	}
	if blockingCount(input, verified) != 2 || artifact.Findings[1].NonBlocking {
		t.Fatal("the confirmed and the reopened finding must block, and the input artifact stays unchanged")
	}
	failed := ApplyVerification(artifact, Verification{ReviewerLabel: "Opus", ErrorCode: "review_output_timeout"})
	if blockingCount(input, failed) != 2 || verificationLine(failed.Verification) == "" {
		t.Fatal("a failed verification must leave every claim as the reviewers made it and say so")
	}
}
