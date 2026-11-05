package policy

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// The synthetic policy in testdata has the shapes of a real deployment: an
// organisation wildcard approved automatically, one repository with its own
// stricter pipeline, and a single-user scope triggered by a label.
func TestDeployedPolicyResolvesTheConfiguredRepositories(t *testing.T) {
	loaded, err := Load(filepath.Join("..", "testdata", "review-policy.yaml"))
	if err != nil {
		t.Fatal(err)
	}
	cases := []struct {
		repository string
		pipeline   string
		trigger    string
		onFindings OnFindings
		onClean    OnClean
		reviewers  []string
	}{
		{"example-org/example-repo", "strict", "review-requested", FindingsRequestChanges, CleanApprove, []string{"opus", "sonnet"}},
		{"example-org/example-api", "generic", "review-requested", FindingsRequestChanges, CleanApprove, []string{"opus", "sonnet"}},
		{"example-org/example-lib", "generic", "review-requested", FindingsRequestChanges, CleanApprove, []string{"opus", "sonnet"}},
		{"example-owner/tool", "personal", "label:ai:review", FindingsRequestChanges, CleanApprove, []string{"codex"}},
	}
	for _, tc := range cases {
		rule, ok := loaded.Resolve(tc.repository)
		if !ok {
			t.Fatalf("%s is not covered by the deployed policy", tc.repository)
		}
		if rule.Pipeline.Name != tc.pipeline || rule.Trigger.String() != tc.trigger || rule.OnFindings != tc.onFindings || rule.OnClean != tc.onClean {
			t.Fatalf("%s resolved to pipeline=%s trigger=%s findings=%s clean=%s", tc.repository, rule.Pipeline.Name, rule.Trigger, rule.OnFindings, rule.OnClean)
		}
		ids := []string{}
		for _, reviewer := range rule.Pipeline.Reviewers {
			ids = append(ids, reviewer.ID)
		}
		if strings.Join(ids, ",") != strings.Join(tc.reviewers, ",") {
			t.Fatalf("%s reviewers = %v", tc.repository, ids)
		}
	}
	if _, ok := loaded.Resolve("octocat/hello-world"); ok {
		t.Fatal("a repository outside every rule must not be reviewed")
	}
	strict, _ := loaded.Resolve("example-org/example-repo")
	generic, _ := loaded.Resolve("example-org/example-api")
	if strict.OnNotes != "" || generic.OnNotes != "" {
		t.Fatalf("notes-only reviews must be approved, not handed to the author: strict=%q generic=%q", strict.OnNotes, generic.OnNotes)
	}
	for _, rule := range []Rule{strict, generic} {
		if len(rule.Pipeline.IncrementalReviewers) != len(rule.Pipeline.Reviewers) {
			t.Fatalf("%s: an automatically approved re-review must use every reviewer", rule.Match)
		}
	}
	if strict.Pipeline.Prompt == generic.Pipeline.Prompt {
		t.Fatal("the strict pipeline must use its own prompt, not the generic one")
	}
	if strict.Pipeline.Checkout != CheckoutBase || strict.Pipeline.Protocol != ProtocolClaudeStructured {
		t.Fatal("the strict pipeline reviewers must start from the trusted base with the structured protocol")
	}
	personal, _ := loaded.Resolve("example-owner/anything")
	if personal.Pipeline.Checkout != CheckoutHead || personal.Pipeline.Protocol != ProtocolCodexEnvelope {
		t.Fatal("personal reviews must use the Codex envelope on the pull request head")
	}
	if got := loaded.Labels(); len(got) != 1 || got[0] != "ai:review" || strings.Join(loaded.ReviewRequestScopes(), ",") != "example-org/*,example-org/example-repo" {
		t.Fatalf("triggers = labels %v, review-requested %v", got, loaded.ReviewRequestScopes())
	}
	if loaded.Level(EventApprovalRequested) != LevelAction || loaded.Level(EventChangesRequested) != LevelDigest || loaded.Level(EventStarted) != LevelLog {
		t.Fatal("only events that need the user may reach the action room")
	}
	if got := strings.Join(loaded.AgentProfiles(), ","); got != "claude-noninteractive,claude-noninteractive-sonnet,codex-review" {
		t.Fatalf("agent profiles = %s", got)
	}
	if got := strings.Join(loaded.LabelScopes("ai:review"), ","); got != "user:example-owner" {
		t.Fatalf("label scopes = %s", got)
	}
	if loaded.HandoffReminder() != 0 || loaded.Level(EventHandoffReminder) != LevelAction {
		t.Fatalf("handoff reminder = %s at %s", loaded.HandoffReminder(), loaded.Level(EventHandoffReminder))
	}
	if loaded.ApprovalTimeout() != 168*time.Hour {
		t.Fatalf("approval timeout = %s", loaded.ApprovalTimeout())
	}
	escalation := map[string]string{"example-org/example-repo": "critical,high/1", "example-org/*": "critical/3"}
	for _, rule := range []Rule{strict, generic} {
		verifier := rule.Pipeline.Verifier
		if verifier == nil || verifier.Reviewer.ID != "opus" || verifier.Prompt == rule.Pipeline.Prompt ||
			fmt.Sprintf("%s/%d", strings.Join(verifier.EscalateSeverities, ","), verifier.EscalateCount) != escalation[rule.Match] {
			t.Fatalf("%s: the blocking findings must be verified by Opus with its own prompt and escalate at %s: %+v", rule.Match, escalation[rule.Match], verifier)
		}
	}
	if human := strict.HumanReview; human == nil || strings.Join(human.Labels, ",") != "needs-human-review" || strings.Join(human.DeletedTests, ",") != "**/tests/**" {
		t.Fatalf("the strict repository must hold clean reviews for the opt-in label and deleted tests: %+v", human)
	}
	if generic.HumanReview != nil {
		t.Fatal("other organisation repositories have no human review signals")
	}
	if got := strings.Join(loaded.ReadyDraftAuthors(), ","); got != "devin-ai-integration" {
		t.Fatalf("ready draft authors = %s", got)
	}
	if personal.Pipeline.Verifier != nil {
		t.Fatal("the Codex pipeline has no verifier")
	}
}

const validPolicy = `version: 1
reviewers:
  opus: {protocol: claude-structured, agent_profile: claude-profile, label: Opus, finding_prefix: O}
  codex: {protocol: codex-envelope, agent_profile: codex-profile, label: Codex, finding_prefix: X}
pipelines:
  main: {reviewers: [opus], incremental_reviewers: [opus], prompt: prompts/main.md, checkout: base, verifier: {reviewer: opus, prompt: prompts/verifier.md, escalate_severities: [critical], escalate_count: 2}, project_sources: [manual]}
  side: {reviewers: [codex], incremental_reviewers: [codex], prompt: prompts/side.md, checkout: head, project_sources: [manual, auto]}
repositories:
  - {match: acme/special, trigger: review-requested, pipeline: main, on_findings: request_changes, on_clean: await}
  - {match: acme/*, trigger: review-requested, pipeline: main, on_findings: request_changes, on_clean: approve}
  - {match: solo/*, trigger: "label:ai:review", pipeline: side, on_findings: comment, on_clean: comment}
unmatched: skip
approval: {timeout_hours: 24}
notifications:
  approval_requested: action
  findings_on_own_pr: action
  review_stopped: action
  cleanup_manual: action
  command_failed: action
  approved: digest
  changes_requested: digest
  commented: digest
  approval_expired: digest
  started: log
  retry_scheduled: log
  superseded: log
  handoff_reminder: action
`

func prompts(name string) ([]byte, error) {
	switch name {
	case "prompts/main.md", "prompts/side.md", "prompts/verifier.md":
		return []byte("Review the pull request.\n"), nil
	}
	return nil, os.ErrNotExist
}

func TestValidPolicyParses(t *testing.T) {
	parsed, err := Parse([]byte(validPolicy), prompts)
	if err != nil {
		t.Fatal(err)
	}
	if rule, _ := parsed.Resolve("acme/special"); rule.OnClean != CleanAwait {
		t.Fatalf("exact rule must win over the later owner rule, got %s", rule.OnClean)
	}
	if rule, _ := parsed.Resolve("acme/other"); rule.OnClean != CleanApprove {
		t.Fatalf("owner rule must cover other repositories, got %s", rule.OnClean)
	}
	again, _ := Parse([]byte(validPolicy), prompts)
	if parsed.Digest() != again.Digest() || len(parsed.Digest()) != 64 {
		t.Fatal("the policy digest must be stable for identical input")
	}
}

func TestPolicyDigestChangesWithTheVerifier(t *testing.T) {
	first, _ := Parse([]byte(validPolicy), prompts)
	second, err := Parse([]byte(strings.Replace(validPolicy, "escalate_count: 2", "escalate_count: 3", 1)), prompts)
	if err != nil {
		t.Fatal(err)
	}
	if first.Digest() == second.Digest() {
		t.Fatal("a verifier change must change the policy digest")
	}
}

// withHandoff sets on_notes comment on the acme/special rule.
func withHandoff(document string, hours string) string {
	document = strings.Replace(document, "on_clean: await}", "on_clean: await, on_notes: comment}", 1)
	return strings.Replace(document, "approval: {timeout_hours: 24}\n", "approval: {timeout_hours: 24}\nnotes_handoff: {reminder_hours: "+hours+"}\n", 1)
}

func TestOnNotesCommentParsesAndChangesTheDigest(t *testing.T) {
	first, _ := Parse([]byte(validPolicy), prompts)
	second, err := Parse([]byte(withHandoff(validPolicy, "24")), prompts)
	if err != nil {
		t.Fatal(err)
	}
	if first.HandoffReminder() != 0 || second.HandoffReminder() != 24*time.Hour {
		t.Fatalf("handoff reminder = %s and %s", first.HandoffReminder(), second.HandoffReminder())
	}
	third, _ := Parse([]byte(withHandoff(validPolicy, "48")), prompts)
	if third == nil || third.Digest() == second.Digest() {
		t.Fatal("a reminder change must change the policy digest")
	}
	if rule, _ := second.Resolve("acme/special"); rule.OnNotes != NotesComment {
		t.Fatalf("on_notes = %q", rule.OnNotes)
	}
	if rule, _ := second.Resolve("acme/other"); rule.OnNotes != "" {
		t.Fatalf("a rule without on_notes must follow its clean outcome, got %q", rule.OnNotes)
	}
	if first.Digest() == second.Digest() {
		t.Fatal("an on_notes change must change the policy digest")
	}
}

// withHumanReview sets human_review on the approving acme/* rule.
func withHumanReview(document, value string) string {
	return strings.Replace(document, "on_findings: request_changes, on_clean: approve}", "on_findings: request_changes, on_clean: approve, human_review: "+value+"}", 1)
}

func TestHumanReviewParsesAndChangesTheDigest(t *testing.T) {
	first, _ := Parse([]byte(validPolicy), prompts)
	second, err := Parse([]byte(withHumanReview(validPolicy, `{labels: [needs-human], deleted_tests: ["tests/**"]}`)), prompts)
	if err != nil {
		t.Fatal(err)
	}
	rule, _ := second.Resolve("acme/other")
	if rule.HumanReview == nil || strings.Join(rule.HumanReview.Labels, ",") != "needs-human" || strings.Join(rule.HumanReview.DeletedTests, ",") != "tests/**" {
		t.Fatalf("human_review = %+v", rule.HumanReview)
	}
	if special, _ := second.Resolve("acme/special"); special.HumanReview != nil {
		t.Fatal("a rule without human_review must not get one")
	}
	if first.Digest() == second.Digest() {
		t.Fatal("a human_review change must change the policy digest")
	}
}

func TestMatchGlobSpansSegmentsOnlyWithDoubleStar(t *testing.T) {
	cases := []struct {
		glob, name string
		want       bool
	}{
		{"tests/**", "tests/a.php", true},
		{"tests/**", "tests/unit/deep/a.php", true},
		{"tests/**", "src/tests/a.php", false},
		{"**/tests/**", "src/tests/a.php", true},
		{"**/tests/**", "tests/a.php", true},
		{"**/tests/**", "src/testsuite/a.php", false},
		{"src/*.go", "src/a.go", true},
		{"src/*.go", "src/sub/a.go", false},
		{"**/*Test.php", "a/b/OwnerTest.php", true},
		{"**", "anything/at/all", true},
	}
	for _, tc := range cases {
		if got := MatchGlob(tc.glob, tc.name); got != tc.want {
			t.Errorf("MatchGlob(%q, %q) = %v, want %v", tc.glob, tc.name, got, tc.want)
		}
	}
}

func TestPolicyDigestChangesWithThePrompt(t *testing.T) {
	first, _ := Parse([]byte(validPolicy), prompts)
	second, err := Parse([]byte(validPolicy), func(name string) ([]byte, error) { return []byte("Different instructions."), nil })
	if err != nil {
		t.Fatal(err)
	}
	if first.Digest() == second.Digest() {
		t.Fatal("a prompt change must change the policy digest")
	}
}

func TestInvalidPolicyIsRejected(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(string) string
		want   string
	}{
		{"unknown field", func(s string) string {
			return strings.Replace(s, "unmatched: skip", "unmatched: skip\nauto_approve: true", 1)
		}, "field auto_approve not found"},
		{"version", func(s string) string { return strings.Replace(s, "version: 1", "version: 2", 1) }, "version must be 1"},
		{"unmatched", func(s string) string { return strings.Replace(s, "unmatched: skip", "unmatched: review", 1) }, "unmatched must be skip"},
		{"shadowed rule", func(s string) string {
			return strings.Replace(s, "  - {match: acme/special,", "  - {match: acme/*, trigger: review-requested, pipeline: main, on_findings: comment, on_clean: comment}\n  - {match: acme/special,", 1)
		}, "unreachable"},
		{"duplicate rule", func(s string) string {
			return strings.Replace(s, "  - {match: solo/*,", "  - {match: acme/special, trigger: review-requested, pipeline: main, on_findings: comment, on_clean: comment}\n  - {match: solo/*,", 1)
		}, "unreachable"},
		{"uppercase match", func(s string) string { return strings.Replace(s, "match: acme/special", "match: Acme/special", 1) }, "lowercase"},
		{"bad trigger", func(s string) string { return strings.Replace(s, `trigger: "label:ai:review"`, "trigger: assigned", 1) }, "trigger"},
		{"unknown pipeline", func(s string) string { return strings.Replace(s, "pipeline: side", "pipeline: missing", 1) }, "unknown pipeline"},
		{"bad on_clean", func(s string) string { return strings.Replace(s, "on_clean: await", "on_clean: merge", 1) }, "on_clean"},
		{"on_notes without notes_handoff", func(s string) string {
			return strings.Replace(s, "on_clean: await}", "on_clean: await, on_notes: comment}", 1)
		}, "needs notes_handoff.reminder_hours"},
		{"notes_handoff without on_notes", func(s string) string {
			return strings.Replace(s, "approval: {timeout_hours: 24}\n", "approval: {timeout_hours: 24}\nnotes_handoff: {reminder_hours: 24}\n", 1)
		}, "no rule sets on_notes"},
		{"notes_handoff reminder out of range", func(s string) string { return withHandoff(s, "0") }, "reminder_hours must be between"},
		{"human_review on an awaiting rule", func(s string) string {
			return strings.Replace(s, "on_clean: await}", "on_clean: await, human_review: {labels: [needs-human]}}", 1)
		}, "human_review needs on_clean approve"},
		{"empty human_review", func(s string) string { return withHumanReview(s, "{}") }, "must set labels or deleted_tests"},
		{"invalid human_review label", func(s string) string { return withHumanReview(s, "{labels: [\"needs human\"]}") }, "human_review labels"},
		{"duplicate human_review label", func(s string) string { return withHumanReview(s, "{labels: [needs-human, Needs-Human]}") }, "human_review labels"},
		{"absolute deleted_tests glob", func(s string) string { return withHumanReview(s, "{deleted_tests: [/tests/**]}") }, "human_review deleted_tests"},
		{"partial double star", func(s string) string { return withHumanReview(s, "{deleted_tests: [tests/**.php]}") }, "human_review deleted_tests"},
		{"parent segment", func(s string) string { return withHumanReview(s, "{deleted_tests: [../tests/*]}") }, "human_review deleted_tests"},
		{"broken class", func(s string) string { return withHumanReview(s, "{deleted_tests: [\"tests/[a\"]}") }, "human_review deleted_tests"},
		{"bad on_notes", func(s string) string {
			return strings.Replace(s, "on_clean: await}", "on_clean: await, on_notes: approve}", 1)
		}, "on_notes must be comment"},
		{"on_notes without a manual approval", func(s string) string {
			return strings.Replace(s, "on_clean: approve}", "on_clean: approve, on_notes: comment}", 1)
		}, "on_notes comment needs on_clean await"},
		{"on_notes on a label trigger", func(s string) string {
			return strings.Replace(s, "on_clean: comment}", "on_clean: await, on_notes: comment}", 1)
		}, "on_notes comment needs on_clean await"},
		{"bad on_findings", func(s string) string { return strings.Replace(s, "on_findings: comment", "on_findings: ignore", 1) }, "on_findings"},
		{"mixed protocols", func(s string) string {
			return strings.Replace(s, "reviewers: [opus], incremental", "reviewers: [opus, codex], incremental", 1)
		}, "same protocol"},
		{"incremental outside reviewers", func(s string) string {
			return strings.Replace(s, "incremental_reviewers: [opus]", "incremental_reviewers: [codex]", 1)
		}, "is not one of its reviewers"},
		{"unknown reviewer", func(s string) string {
			return strings.Replace(s, "reviewers: [opus], incremental", "reviewers: [gpt], incremental", 1)
		}, "unknown or repeated reviewer"},
		{"shared prefix", func(s string) string { return strings.Replace(s, "finding_prefix: X", "finding_prefix: O", 1) }, "share finding_prefix"},
		{"bad protocol", func(s string) string { return strings.Replace(s, "protocol: codex-envelope", "protocol: plain", 1) }, "protocol must be"},
		{"bad checkout", func(s string) string { return strings.Replace(s, "checkout: head", "checkout: merge", 1) }, "checkout must be"},
		{"prompt escapes directory", func(s string) string { return strings.Replace(s, "prompt: prompts/side.md", "prompt: ../side.md", 1) }, "inside the policy directory"},
		{"missing prompt", func(s string) string {
			return strings.Replace(s, "prompt: prompts/side.md", "prompt: prompts/none.md", 1)
		}, "prompts/none.md"},
		{"missing event", func(s string) string { return strings.Replace(s, "  superseded: log\n", "", 1) }, "event superseded needs a level"},
		{"unknown event", func(s string) string {
			return strings.Replace(s, "  superseded: log\n", "  superseded: log\n  merged: log\n", 1)
		}, "unknown event"},
		{"bad level", func(s string) string { return strings.Replace(s, "superseded: log", "superseded: telegram", 1) }, "level must be"},
		{"approval timeout", func(s string) string { return strings.Replace(s, "timeout_hours: 24", "timeout_hours: 0", 1) }, "timeout_hours"},
		{"no source", func(s string) string {
			return strings.Replace(s, "project_sources: [manual]}", "project_sources: []}", 1)
		}, "project_sources"},
		{"second document", func(s string) string { return s + "---\nversion: 1\n" }, "exactly one YAML document"},
		{"verifier of another protocol", func(s string) string {
			return strings.Replace(s, "verifier: {reviewer: opus,", "verifier: {reviewer: codex,", 1)
		}, "verifier must use protocol"},
		{"unknown verifier", func(s string) string {
			return strings.Replace(s, "verifier: {reviewer: opus,", "verifier: {reviewer: gpt,", 1)
		}, "verifier names unknown reviewer"},
		{"verifier escalating a note", func(s string) string {
			return strings.Replace(s, "escalate_severities: [critical]", "escalate_severities: [medium]", 1)
		}, "critical or high once each"},
		{"verifier without escalation", func(s string) string { return strings.Replace(s, "escalate_count: 2", "escalate_count: 0", 1) }, "escalate_count of at least 1"},
		{"missing verifier prompt", func(s string) string {
			return strings.Replace(s, "prompt: prompts/verifier.md", "prompt: prompts/none.md", 1)
		}, "prompts/none.md"},
		{"reserved reviewer id", func(s string) string { return strings.Replace(s, "  codex: {protocol", "  verifier: {protocol", 1) }, "invalid or reserved"},
		{"ready draft author with the app prefix", func(s string) string { return s + "ready_draft_authors: [app/devin-ai-integration]\n" }, "ready_draft_authors"},
		{"repeated ready draft author", func(s string) string { return s + "ready_draft_authors: [devin, devin]\n" }, "ready_draft_authors"},
		{"weaker re-review before automatic approval", func(s string) string {
			s = strings.Replace(s, "  main: {reviewers: [opus], incremental_reviewers: [opus],", "  main: {reviewers: [opus, extra], incremental_reviewers: [opus],", 1)
			return strings.Replace(s, "  codex: {protocol: codex-envelope,", "  extra: {protocol: claude-structured, agent_profile: extra-profile, label: Extra, finding_prefix: E}\n  codex: {protocol: codex-envelope,", 1)
		}, "incremental_reviewers equal to reviewers"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			document := tc.mutate(validPolicy)
			if document == validPolicy {
				t.Fatal("mutation did not change the policy")
			}
			_, err := Parse([]byte(document), prompts)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want it to mention %q", err, tc.want)
			}
		})
	}
}

func TestLoadRejectsOversizedPrompt(t *testing.T) {
	directory := t.TempDir()
	if err := os.MkdirAll(filepath.Join(directory, "prompts"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(directory, "prompts", "main.md"), []byte(strings.Repeat("a", maxPromptBytes+1)), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(directory, "prompts", "side.md"), []byte("Review."), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(directory, "policy.yaml"), []byte(validPolicy), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := Load(filepath.Join(directory, "policy.yaml")); err == nil || !strings.Contains(err.Error(), "exceeds") {
		t.Fatalf("error = %v", err)
	}
	if _, err := Load(filepath.Join(directory, "missing.yaml")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing policy error = %v", err)
	}
}
