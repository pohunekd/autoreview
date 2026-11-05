package schedule

import (
	"strings"
	"testing"
)

func TestFetchFailuresClassifyWithoutRetainingSecrets(t *testing.T) {
	for _, test := range []struct {
		detail, code string
		permanent    bool
	}{
		{"git command timed out after 30s", "pohunek_base_fetch_timeout", false},
		{"Authentication failed for https://token:secret@github.com/private", "pohunek_base_fetch_authentication_failed", true},
		{"git@ssh.github.com: Permission denied (publickey).", "pohunek_base_fetch_authentication_failed", true},
		{"fatal: couldn't find remote ref deleted", "pohunek_base_fetch_ref_missing", true},
		{"No space left on device", "pohunek_base_fetch_disk_full", true},
		{"cannot open FETCH_HEAD: Permission denied", "pohunek_base_fetch_permission_denied", true},
		{"Could not resolve host: github.com", "pohunek_base_fetch_dns_failed", false},
		{"error: cannot lock ref 'refs/remotes/origin/main': is at 49a1b61 but expected 9ff9c5d", "pohunek_base_fetch_ref_locked", false},
		{"token=secret prompt=private", "pohunek_base_fetch_failed", false},
		{"", "pohunek_base_fetch_failed", false},
	} {
		t.Run(test.code+test.detail[:min(len(test.detail), 3)], func(t *testing.T) {
			code, permanent := FetchFailure(test.detail)
			if code != test.code || permanent != test.permanent {
				t.Fatalf("classification = %s, %t", code, permanent)
			}
			if Retryable(code) == permanent {
				t.Fatalf("incorrect scheduling retry policy for %s", code)
			}
			explanation := FailureExplanation(code)
			if strings.Contains(explanation, "secret") || strings.Contains(explanation, "private") {
				t.Fatal("raw diagnostic leaked")
			}
		})
	}
}

func TestFailureNotificationsExplainCauseAndNextStep(t *testing.T) {
	target, now, _ := fixture()
	for _, phase := range []string{"retry_wait", "failed"} {
		message := Notification(target, Record{Phase: phase, ErrorCode: "pohunek_base_fetch_timeout", Attempt: 2, DueAt: now})
		for _, want := range []string{target.Key(), "Git timed out", "keyring", "Diagnostic: pohunek_base_fetch_timeout"} {
			if !strings.Contains(message, want) {
				t.Fatalf("notification lacks %q: %s", want, message)
			}
		}
		if phase == "retry_wait" && !strings.Contains(message, "Next automatic attempt: 3 at") {
			t.Fatal(message)
		}
		if phase == "failed" && !strings.Contains(message, "No further automatic attempt") {
			t.Fatal(message)
		}
	}
}
