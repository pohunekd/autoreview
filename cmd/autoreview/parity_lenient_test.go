package main

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"testing"

	"github.com/pohunekd/autoreview/policy"
)

// parityLenientText holds the characters json.Marshal escapes, a quote and a
// backslash, so a value read back and written again must keep its escapes.
const parityLenientText = "kind<&>  \"\\"

// parityLenientInput is a review input that an older engine wrote: every
// string type of the policy carries a value the current policy does not define.
func parityLenientInput(pr PullRequest) ReviewInput {
	reviewer := policy.Reviewer{ID: "opus", Label: "Opus", Protocol: "legacy-protocol", AgentProfile: "claude-opus", FindingPrefix: "O"}
	notifications := map[policy.Event]policy.Level{"": "", "aaa_retired": "pager", "invocation_paused": "action", "zzz_retired": parityLenientText}
	for _, event := range policy.WorkflowEvents {
		notifications[event] = policy.LevelLog
	}
	return ReviewInput{
		Version:     inputVersion,
		PullRequest: pr,
		Attempt:     1,
		Rule: policy.Rule{
			Match:   "acme/*",
			Trigger: policy.Trigger{Kind: parityLenientText, Label: "ai-review"},
			Pipeline: policy.Pipeline{
				Name: "main", Protocol: "legacy-protocol", Reviewers: []policy.Reviewer{reviewer},
				Prompt: "Review the pull request.", Checkout: "merge",
			},
			OnFindings: "block",
			OnClean:    "hold",
			OnNotes:    "digest",
		},
		Reviewers:              []policy.Reviewer{reviewer},
		Scope:                  ReviewScope{Mode: scopeFull},
		Route:                  ProjectRoute{Host: "host", ProjectID: "project"},
		Notifications:          notifications,
		ApprovalTimeoutSeconds: 60,
		PolicyDigest:           "digest",
		PublisherLogin:         "reviewer-bot",
	}
}

// TestWriteStoreLenientGolden stores state with values the current engine does
// not define, as an older engine wrote it, and records what the store and the
// review command read back. It compares or writes store-lenient.json.
func TestWriteStoreLenientGolden(t *testing.T) {
	ctx := context.Background()
	clock := &parityStoreClock{instant: parityStoreInstants[0]}
	cfg := integrationConfig(t)
	store, err := OpenStore(cfg.StateDBPath, clock.now)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	engine := NewEngine(cfg, store, newFake(t), slog.New(slog.NewTextHandler(io.Discard, nil)), clock.now, sleepWithContext)

	pr := parityStorePR("acme/legacy", 7)
	workflowID := pr.WorkflowID(1)
	if err := store.inTx(ctx, func(tx *sql.Tx) error {
		return store.enqueueReview(ctx, tx, workflowID, parityLenientInput(pr))
	}); err != nil {
		t.Fatal(err)
	}
	if err := store.saveStep(ctx, workflowID, stepPlan, PublicationPlan{Decision: "DISMISS"}); err != nil {
		t.Fatal(err)
	}
	if err := store.saveStep(ctx, workflowID, stepPublication, PublishResult{Decision: parityLenientText, GitHubReviewID: 3, ReviewURL: pr.URL + "#review"}); err != nil {
		t.Fatal(err)
	}
	outcome := ReviewOutcome{Outcome: "abandoned", Publication: &PublishResult{Decision: "DISMISS"}}
	if err := store.completeReview(ctx, workflowID, "success", "", &outcome); err != nil {
		t.Fatal(err)
	}
	waiting := parityStorePR("acme/waiting", 8)
	if err := store.updateDiscovery(ctx, func(state discoveryState, tx *sql.Tx) error {
		state.published[pr.Key()] = PublishedRecord{Repository: pr.Repository, Number: pr.Number, HeadSHA: pr.HeadSHA, ReviewRequestID: pr.ReviewRequestID, Decision: "DISMISS", WorkflowID: workflowID, PublishedAt: parityStoreInstants[0]}
		state.awaiting[waiting.Key()] = AwaitRecord{Repository: waiting.Repository, Number: waiting.Number, HeadSHA: waiting.HeadSHA, ReviewRequestID: waiting.ReviewRequestID, Trigger: policy.Trigger{Kind: parityLenientText, Label: "ai-review"}, WorkflowID: waiting.WorkflowID(1)}
		return nil
	}); err != nil {
		t.Fatal(err)
	}

	reviews, err := store.listReviews(ctx, "", cfg.ReviewListLimit)
	if err != nil {
		t.Fatal(err)
	}
	status, err := engine.PullRequestStatus(ctx, pr.Repository, pr.Number)
	if err != nil {
		t.Fatal(err)
	}
	waitingStatus, err := engine.PullRequestStatus(ctx, waiting.Repository, waiting.Number)
	if err != nil {
		t.Fatal(err)
	}
	parityCheckStoreGolden(t, "store-lenient", map[string]any{
		"discovery":    parityStoreTable(t, store, "discovery", "name"),
		"reviews":      parityStoreTable(t, store, "reviews", "workflow_id"),
		"review_steps": parityStoreTable(t, store, "review_steps", "workflow_id, name"),
		"api": map[string]any{
			"list_reviews":   reviews,
			"review":         status,
			"review_waiting": waitingStatus,
		},
	})
}
