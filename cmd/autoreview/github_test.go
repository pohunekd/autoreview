package main

import (
	"context"
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

const githubTestPolicy = `version: 1
reviewers:
  opus: {protocol: claude-structured, agent_profile: claude-profile, label: Opus, finding_prefix: O}
pipelines:
  main: {reviewers: [opus], incremental_reviewers: [opus], prompt: prompt.md, checkout: base, project_sources: [manual]}
repositories:
  - {match: acme/*, trigger: review-requested, pipeline: main, on_findings: request_changes, on_clean: await}
unmatched: skip
approval: {timeout_hours: 1}
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

// scriptedGitHub answers gh calls from a fixed set of pull requests, all of
// which request a review from the configured login.
type scriptedGitHub struct {
	t        *testing.T
	drafts   map[int]bool
	bots     map[int]bool
	searches [][]string
	events   []int
}

func (gh *scriptedGitHub) Run(_ context.Context, _ string, args []string, _ []byte, _ []string) ([]byte, error) {
	switch {
	case args[0] == "search":
		gh.searches = append(gh.searches, args)
		var found []map[string]any
		for number := range gh.drafts {
			found = append(found, map[string]any{"number": number, "repository": map[string]string{"nameWithOwner": "acme/widget"}})
		}
		return json.Marshal(found)
	case len(args) > 1 && args[0] == "api" && args[1] == "graphql":
		var number int
		for _, arg := range args {
			if value, ok := strings.CutPrefix(arg, "number="); ok {
				fmt.Sscanf(value, "%d", &number)
			}
		}
		return json.Marshal(map[string]any{"data": map[string]any{"repository": map[string]any{"pullRequest": map[string]any{
			"id": "PR_1", "number": number, "title": "Add widget", "url": fmt.Sprintf("https://github.com/acme/widget/pull/%d", number),
			"state": "OPEN", "isDraft": gh.drafts[number], "headRefOid": sha("a"), "headRefName": "feature", "baseRefOid": sha("b"), "baseRefName": "main",
			"author":         gh.author(number),
			"labels":         map[string]any{"nodes": []any{}},
			"reviewRequests": map[string]any{"nodes": []any{map[string]any{"requestedReviewer": map[string]string{"login": "reviewer"}}}},
			"repository":     map[string]string{"nameWithOwner": "acme/widget"},
		}}}})
	case len(args) > 1 && args[0] == "api" && strings.Contains(args[1], "/events"):
		var number int
		fmt.Sscanf(args[1], "repos/acme/widget/issues/%d/events", &number)
		gh.events = append(gh.events, number)
		return json.Marshal([]map[string]any{{"id": 100 + number, "event": "review_requested", "created_at": "2026-10-01T10:00:00Z", "requested_reviewer": map[string]string{"login": "reviewer"}}})
	}
	gh.t.Fatalf("unexpected gh call %v", args)
	return nil, nil
}

func (gh *scriptedGitHub) author(number int) map[string]string {
	if gh.bots[number] {
		return map[string]string{"__typename": "Bot", "login": "devin-ai-integration"}
	}
	return map[string]string{"__typename": "User", "login": "colleague"}
}

func TestAppAuthorIsMarkedAsABotAndStateIsReadWithoutATrigger(t *testing.T) {
	cfg := testConfig()
	cfg.GHTimeout, cfg.SearchPageLimit = time.Minute, 5
	gh := &scriptedGitHub{t: t, drafts: map[int]bool{}, bots: map[int]bool{3: true}}
	external := &External{cfg: cfg, runner: gh}
	trigger := policy.Trigger{Kind: policy.TriggerReviewRequested}
	bot, err := external.GetPullRequest(context.Background(), "acme/widget", 3, trigger)
	if err != nil || !bot.AuthorBot || bot.Author != "devin-ai-integration" {
		t.Fatalf("bot = %+v, %v", bot, err)
	}
	human, err := external.GetPullRequest(context.Background(), "acme/widget", 4, trigger)
	if err != nil || human.AuthorBot {
		t.Fatalf("human = %+v, %v", human, err)
	}
	state, err := external.ReadPullRequestState(context.Background(), "acme/widget", 4)
	if err != nil || state != (PullRequestState{Open: true, Requested: true, Title: "Add widget", URL: "https://github.com/acme/widget/pull/4", Author: "colleague", HeadSHA: sha("a")}) {
		t.Fatalf("state = %+v, %v", state, err)
	}
}

func TestDraftWithARequestedReviewIsNotACandidate(t *testing.T) {
	loaded, err := policy.Parse([]byte(githubTestPolicy), func(string) ([]byte, error) { return []byte("Review the pull request."), nil })
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig()
	cfg.Policy, cfg.GHTimeout, cfg.PollLimit, cfg.SearchPageLimit = loaded, time.Minute, 100, 5
	// The draft stands for a pull request converted to draft between the
	// search and its detail read.
	gh := &scriptedGitHub{t: t, drafts: map[int]bool{1: true, 2: false}}
	external := &External{cfg: cfg, runner: gh}

	list, err := external.ListCandidates(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(list.Candidates) != 1 || list.Candidates[0].PullRequest.Number != 2 || len(list.Rejected) != 0 {
		t.Fatalf("candidates %+v, rejected %+v", list.Candidates, list.Rejected)
	}
	if len(gh.searches) != 1 || !slices.Contains(gh.searches[0], "--draft=false") {
		t.Fatalf("the review request search must exclude drafts: %v", gh.searches)
	}
	if !slices.Equal(gh.events, []int{2}) {
		t.Fatalf("review request events were read for %v", gh.events)
	}

	if _, err := external.GetPullRequest(context.Background(), "acme/widget", 1, policy.Trigger{Kind: policy.TriggerReviewRequested}); errorCode(err) != "pull_request_draft" {
		t.Fatalf("a running review must see a draft as lapsed, got %v", err)
	}
}

// draftGitHub answers the searches and reads of drafts and records the drafts
// it is asked to mark ready for review.
type draftGitHub struct {
	t        *testing.T
	authors  map[int]string
	requests map[int]bool
	searches [][]string
	marked   []string
}

func (gh *draftGitHub) Run(_ context.Context, _ string, args []string, _ []byte, _ []string) ([]byte, error) {
	query := ""
	number := 0
	for _, arg := range args {
		if value, ok := strings.CutPrefix(arg, "query="); ok {
			query = value
		}
		if value, ok := strings.CutPrefix(arg, "number="); ok {
			fmt.Sscanf(value, "%d", &number)
		}
	}
	switch {
	case args[0] == "search":
		gh.searches = append(gh.searches, args)
		found := []map[string]any{}
		for number := range gh.authors {
			found = append(found, map[string]any{"number": number, "repository": map[string]string{"nameWithOwner": "acme/widget"}})
		}
		return json.Marshal(found)
	case strings.Contains(query, "markPullRequestReadyForReview"):
		for _, arg := range args {
			if id, ok := strings.CutPrefix(arg, "id="); ok {
				gh.marked = append(gh.marked, id)
			}
		}
		return json.Marshal(map[string]any{"data": map[string]any{"markPullRequestReadyForReview": map[string]any{"pullRequest": map[string]any{"isDraft": false}}}})
	case strings.Contains(query, "pullRequest(number"):
		requested := []any{}
		if gh.requests[number] {
			requested = append(requested, map[string]any{"requestedReviewer": map[string]string{"login": "reviewer"}})
		}
		return json.Marshal(map[string]any{"data": map[string]any{"repository": map[string]any{"pullRequest": map[string]any{
			"id": fmt.Sprintf("PR_%d", number), "number": number, "title": "Fix", "url": fmt.Sprintf("https://github.com/acme/widget/pull/%d", number),
			"state": "OPEN", "isDraft": true, "headRefOid": sha("a"), "headRefName": "devin/fix", "baseRefOid": sha("b"), "baseRefName": "main",
			"author": map[string]string{"login": gh.authors[number]}, "labels": map[string]any{"nodes": []any{}},
			"reviewRequests": map[string]any{"nodes": requested}, "repository": map[string]string{"nameWithOwner": "acme/widget"},
		}}}})
	}
	gh.t.Fatalf("unexpected gh call %v", args)
	return nil, nil
}

func TestDraftOfACodingAgentThatAsksForAReviewIsMarkedReady(t *testing.T) {
	document := githubTestPolicy + "ready_draft_authors: [devin-ai-integration]\n"
	loaded, err := policy.Parse([]byte(document), func(string) ([]byte, error) { return []byte("Review the pull request."), nil })
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig()
	cfg.Policy, cfg.GHTimeout, cfg.PollLimit, cfg.SearchPageLimit = loaded, time.Minute, 100, 5
	// 2 no longer asks for a review, 3 is a human's draft the search let through.
	gh := &draftGitHub{t: t, authors: map[int]string{1: "devin-ai-integration", 2: "devin-ai-integration", 3: "colleague"}, requests: map[int]bool{1: true, 3: true}}
	readied, err := (&External{cfg: cfg, runner: gh}).ReadyDrafts(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(readied) != 1 || readied[0].Number != 1 || !slices.Equal(gh.marked, []string{"PR_1"}) {
		t.Fatalf("readied %+v, marked %v", readied, gh.marked)
	}
	if len(gh.searches) != 1 || !slices.Contains(gh.searches[0], "--draft=true") || !slices.Contains(gh.searches[0], "app/devin-ai-integration") {
		t.Fatalf("the search must look for the agent's drafts only: %v", gh.searches)
	}
	gh = &draftGitHub{t: t, authors: map[int]string{1: "devin-ai-integration"}, requests: map[int]bool{1: true}}
	cfg.Policy, _ = policy.Parse([]byte(githubTestPolicy), func(string) ([]byte, error) { return []byte("Review the pull request."), nil })
	if readied, err := (&External{cfg: cfg, runner: gh}).ReadyDrafts(context.Background()); err != nil || len(readied) != 0 || len(gh.searches) != 0 {
		t.Fatalf("a policy without ready-draft authors must touch no draft: %+v, %v, %v", readied, err, gh.searches)
	}
}
