package main

import (
	"slices"
	"testing"

	"github.com/pohunekd/autoreview/policy"
)

func TestHumanReviewSignalsNameMatchedLabelsAndDeletedTestFiles(t *testing.T) {
	signals := policy.HumanReview{Labels: []string{"needs-human", "risky"}, DeletedTests: []string{"tests/**", "**/tests/**"}}
	cases := []struct {
		name    string
		labels  []string
		deleted []string
		want    []string
	}{
		{"nothing matches", []string{"bug"}, []string{"src/a.go", "testsuite/a.go"}, []string{}},
		{"label case is ignored", []string{"NEEDS-HUMAN"}, nil, []string{"label needs-human"}},
		{"deleted tests are sorted", nil, []string{"pkg/tests/b.php", "tests/a.php", "src/c.go"}, []string{"deleted test files pkg/tests/b.php, tests/a.php"}},
		{"both", []string{"risky"}, []string{"tests/a.php"}, []string{"label risky", "deleted test files tests/a.php"}},
	}
	for _, tc := range cases {
		if got := HumanReviewSignals(signals, tc.labels, tc.deleted); !slices.Equal(got, tc.want) {
			t.Errorf("%s: got %q, want %q", tc.name, got, tc.want)
		}
	}
}

func TestHumanReviewSignalMakesACleanApprovalWait(t *testing.T) {
	pr := PullRequest{Repository: "acme/widget", Number: 7, HeadSHA: sha("a"), Author: "author"}
	input := testInput(testRule(opus, policy.FindingsRequestChanges, policy.CleanApprove), pr)
	if plan := PlanPublication(input, Artifact{}, "reviewer"); plan.Await {
		t.Fatal("a clean review without signals must approve without the user")
	}
	if plan := PlanPublication(input, Artifact{HumanReview: []string{"label needs-human"}}, "reviewer"); plan.Decision != DecisionApprove || !plan.Await {
		t.Fatalf("a matched signal must wait for the user, plan = %+v", plan)
	}
}
