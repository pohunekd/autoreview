package main

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

const graphQLDiscussion = `query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 50, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          isResolved
          comments(first: 50) { nodes { author { login __typename } body pullRequestReview { databaseId } } }
        }
      }
      comments(last: 50) { nodes { author { login __typename } body } }
    }
  }
}`

// findingHeadingPattern finds the heading findingHeading renders. It matches
// anywhere in the body, because earlier comments led with the AI signature.
var findingHeadingPattern = regexp.MustCompile(`\*\*([A-Z][0-9]{3,}) (?:CRITICAL|HIGH|MEDIUM|LOW) — `)

const (
	discussionRoleReviewer = "reviewer"
	discussionRoleAuthor   = "author"
	discussionRoleOther    = "other"
)

type githubDiscussionAuthor struct {
	Login    string `json:"login"`
	TypeName string `json:"__typename"`
}

type githubDiscussionComment struct {
	Author            *githubDiscussionAuthor `json:"author"`
	Body              string                  `json:"body"`
	PullRequestReview *struct {
		DatabaseID int64 `json:"databaseId"`
	} `json:"pullRequestReview"`
}

// aiSigned reports whether the last line of the body, ignoring blank lines and
// HTML comment lines, starts with "Written by AI:" after an optional emoji.
func aiSigned(body string) bool {
	lines := strings.Split(body, "\n")
	for index := len(lines) - 1; index >= 0; index-- {
		line := strings.TrimSpace(lines[index])
		if line == "" || strings.HasPrefix(line, "<!--") {
			continue
		}
		return strings.HasPrefix(strings.TrimLeftFunc(line, isSignatureEmoji), "Written by AI:")
	}
	return false
}

func discussionRole(login string, pr PullRequest, publisher string) string {
	switch {
	case strings.EqualFold(login, publisher):
		return discussionRoleReviewer
	case strings.EqualFold(login, pr.Author):
		return discussionRoleAuthor
	}
	return discussionRoleOther
}

// discussionBudget hands out the characters of the discussion limit; a body
// over the remaining budget is cut, and nothing is added once it is spent.
type discussionBudget struct{ remaining int }

func (budget *discussionBudget) take(body string) (string, bool) {
	runes := []rune(body)
	if budget.remaining <= 0 {
		return "", false
	}
	if len(runes) > budget.remaining {
		runes = append(runes[:max(budget.remaining-1, 0)], '…')
	}
	budget.remaining -= len(runes)
	return string(runes), true
}

// ListDiscussion reads the reply threads of the prior findings' inline
// comments and the newest conversation comments of the pull request. Thread
// replies come first in the budget: they answer the findings directly.
func (external *External) ListDiscussion(ctx context.Context, pr PullRequest, prior []PriorFinding) (Discussion, error) {
	heads := map[int64]string{}
	keys := map[string]bool{}
	for _, finding := range prior {
		heads[finding.GitHubReviewID] = finding.HeadSHA
		keys[finding.Key] = true
	}
	owner, name, _ := strings.Cut(pr.Repository, "/")
	discussion := Discussion{Threads: []DiscussionThread{}, Comments: []DiscussionComment{}}
	budget := &discussionBudget{remaining: external.cfg.MaxDiscussionCharacters}
	comment := func(source githubDiscussionComment) (DiscussionComment, bool) {
		// A bot that authored the pull request, such as a coding agent, answers
		// the findings; other bots only add noise.
		if source.Author == nil || source.Author.TypeName == "Bot" && !strings.EqualFold(source.Author.Login, pr.Author) {
			return DiscussionComment{}, false
		}
		body, ok := budget.take(source.Body)
		return DiscussionComment{Author: source.Author.Login, Role: discussionRole(source.Author.Login, pr, external.cfg.GitHubLogin), AISigned: aiSigned(source.Body), Body: body}, ok
	}
	var conversation []githubDiscussionComment
	after := ""
	for page := 1; ; page++ {
		args := []string{"api", "graphql", "-f", "query=" + graphQLDiscussion, "-F", "owner=" + owner, "-F", "name=" + name, "-F", "number=" + strconv.Itoa(pr.Number)}
		if after != "" {
			args = append(args, "-f", "after="+after)
		}
		output, err := external.github(ctx, args, nil)
		if err != nil {
			return Discussion{}, err
		}
		var response struct {
			Data struct {
				Repository *struct {
					PullRequest *struct {
						ReviewThreads struct {
							PageInfo struct {
								HasNextPage bool   `json:"hasNextPage"`
								EndCursor   string `json:"endCursor"`
							} `json:"pageInfo"`
							Nodes []struct {
								IsResolved bool `json:"isResolved"`
								Comments   struct {
									Nodes []githubDiscussionComment `json:"nodes"`
								} `json:"comments"`
							} `json:"nodes"`
						} `json:"reviewThreads"`
						Comments struct {
							Nodes []githubDiscussionComment `json:"nodes"`
						} `json:"comments"`
					} `json:"pullRequest"`
				} `json:"repository"`
			} `json:"data"`
			Errors []json.RawMessage `json:"errors"`
		}
		if err := json.Unmarshal(output, &response); err != nil || len(response.Errors) != 0 || response.Data.Repository == nil || response.Data.Repository.PullRequest == nil {
			return Discussion{}, workflowError("github_response_invalid", true, err)
		}
		pull := response.Data.Repository.PullRequest
		if page == 1 {
			conversation = pull.Comments.Nodes
		}
		for _, thread := range pull.ReviewThreads.Nodes {
			if len(thread.Comments.Nodes) == 0 || thread.Comments.Nodes[0].PullRequestReview == nil {
				continue
			}
			first := thread.Comments.Nodes[0]
			head, known := heads[first.PullRequestReview.DatabaseID]
			match := findingHeadingPattern.FindStringSubmatch(first.Body)
			if !known || match == nil || !keys[priorKey(head, match[1])] {
				continue
			}
			entry := DiscussionThread{Key: priorKey(head, match[1]), Resolved: thread.IsResolved, Replies: []DiscussionComment{}}
			for _, reply := range thread.Comments.Nodes[1:] {
				if converted, ok := comment(reply); ok {
					entry.Replies = append(entry.Replies, converted)
				}
			}
			discussion.Threads = append(discussion.Threads, entry)
		}
		if !pull.ReviewThreads.PageInfo.HasNextPage {
			break
		}
		if page == external.cfg.SearchPageLimit {
			return Discussion{}, workflowError("github_search_limit_exceeded", true, nil)
		}
		after = pull.ReviewThreads.PageInfo.EndCursor
	}
	// The newest comments get the remaining budget; the result keeps their order.
	for index := len(conversation) - 1; index >= 0; index-- {
		if converted, ok := comment(conversation[index]); ok {
			discussion.Comments = append([]DiscussionComment{converted}, discussion.Comments...)
		}
	}
	return discussion, nil
}

// LinearHistory reports whether head extends previous by single-parent
// commits. A rebase, a force push or a merge of the base branch makes the
// diff between the two heads something other than the author's own change.
func (external *External) LinearHistory(ctx context.Context, repository, previous, head string) (bool, error) {
	if !repositoryPattern.MatchString(repository) || !shaPattern.MatchString(previous) || !shaPattern.MatchString(head) {
		return false, workflowError("review_identity_invalid", true, nil)
	}
	output, err := external.github(ctx, []string{"api", fmt.Sprintf("repos/%s/compare/%s...%s", repository, previous, head)}, nil)
	if err != nil {
		return false, err
	}
	var comparison struct {
		Status       string `json:"status"`
		TotalCommits int    `json:"total_commits"`
		Commits      []struct {
			Parents []json.RawMessage `json:"parents"`
		} `json:"commits"`
	}
	if err := json.Unmarshal(output, &comparison); err != nil {
		return false, workflowError("github_response_invalid", true, err)
	}
	// GitHub lists at most 250 commits; an incomplete list proves nothing.
	if comparison.Status != "ahead" || comparison.TotalCommits != len(comparison.Commits) {
		return false, nil
	}
	for _, commit := range comparison.Commits {
		if len(commit.Parents) != 1 {
			return false, nil
		}
	}
	return true, nil
}

// commentsBy returns the comments login wrote on the thread of key or on the
// pull request's conversation.
func (discussion Discussion) commentsBy(key, login string) []DiscussionComment {
	found := []DiscussionComment{}
	for _, thread := range discussion.Threads {
		if thread.Key != key {
			continue
		}
		for _, reply := range thread.Replies {
			if strings.EqualFold(reply.Author, login) {
				found = append(found, reply)
			}
		}
	}
	for _, comment := range discussion.Comments {
		if strings.EqualFold(comment.Author, login) {
			found = append(found, comment)
		}
	}
	return found
}

// openThread reports whether the inline comment of key still has an
// unresolved thread the author can answer.
func (discussion Discussion) openThread(key string) bool {
	for _, thread := range discussion.Threads {
		if thread.Key == key && !thread.Resolved {
			return true
		}
	}
	return false
}
