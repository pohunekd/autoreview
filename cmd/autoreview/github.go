package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"path"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

const pullRequestFields = `id number title url state isDraft headRefOid headRefName baseRefOid baseRefName
      author { __typename login }
      labels(first: 100) { nodes { name } }
      reviewRequests(first: 100) { nodes { requestedReviewer { ... on User { login } } } }
      repository { nameWithOwner }`

const graphQLSearch = `query($searchQuery: String!, $after: String) {
  search(query: $searchQuery, type: ISSUE, first: 50, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes { ... on PullRequest { ` + pullRequestFields + ` } }
  }
}`

const graphQLPullRequest = `query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) { ` + pullRequestFields + ` }
  }
}`

var (
	githubHTTP422Pattern = regexp.MustCompile(`(?i)\bHTTP\s+422\b`)
	// An empty author is a deleted account; it is never the reviewing login.
	authorPattern = regexp.MustCompile(`^(?:[a-z0-9](?:[a-z0-9-]{0,38})(?:\[bot\])?)?$`)
	branchPattern = regexp.MustCompile(`^[A-Za-z0-9._/-]{1,255}$`)
	hunkPattern   = regexp.MustCompile(`^@@ -[0-9]+(?:,[0-9]+)? \+([0-9]+)(?:,([0-9]+))? @@`)
)

// CommentableLines maps a changed file to the RIGHT-side lines that accept an
// inline review comment.
type CommentableLines map[string][]int

type InlineComment struct {
	Path string `json:"path"`
	Line int    `json:"line"`
	Side string `json:"side"`
	Body string `json:"body"`
}

// GitHubReview is one review submission. Marker makes the submission
// idempotent: a review that already carries it is reused instead of posted.
type GitHubReview struct {
	Decision Decision        `json:"decision"`
	Body     string          `json:"body"`
	Comments []InlineComment `json:"comments"`
	Marker   string          `json:"marker"`
}

func (external *External) github(ctx context.Context, args []string, input any) (json.RawMessage, error) {
	ctx, cancel := context.WithTimeout(ctx, external.cfg.GHTimeout)
	defer cancel()
	var payload []byte
	if input != nil {
		var err error
		if payload, err = json.Marshal(input); err != nil {
			return nil, workflowError("github_request_invalid", true, err)
		}
	}
	output, err := external.runner.Run(ctx, external.cfg.GHBin, args, payload, cleanEnvironment("GH_TOKEN="+external.cfg.GitHubToken, "GH_PAGER=cat", "GH_PROMPT_DISABLED=1"))
	if err != nil {
		if githubHTTP422Pattern.MatchString(err.Error()) {
			return nil, workflowError("github_request_rejected", true, err)
		}
		return nil, workflowError("github_request_failed", false, err)
	}
	if !json.Valid(output) {
		return nil, workflowError("github_response_invalid", true, nil)
	}
	return output, nil
}

// ViewerLogin returns the login of the account behind the GitHub token.
func (external *External) ViewerLogin(ctx context.Context) (string, error) {
	output, err := external.github(ctx, []string{"api", "user"}, nil)
	if err != nil {
		return "", err
	}
	var viewer struct {
		Login string `json:"login"`
	}
	if json.Unmarshal(output, &viewer) != nil || viewer.Login == "" {
		return "", workflowError("github_response_invalid", true, nil)
	}
	return strings.ToLower(viewer.Login), nil
}

type githubPullRequest struct {
	ID         string `json:"id"`
	Number     int    `json:"number"`
	Title      string `json:"title"`
	URL        string `json:"url"`
	State      string `json:"state"`
	IsDraft    bool   `json:"isDraft"`
	HeadSHA    string `json:"headRefOid"`
	HeadBranch string `json:"headRefName"`
	BaseSHA    string `json:"baseRefOid"`
	BaseBranch string `json:"baseRefName"`
	Author     *struct {
		Type  string `json:"__typename"`
		Login string `json:"login"`
	} `json:"author"`
	Labels struct {
		Nodes []struct {
			Name string `json:"name"`
		} `json:"nodes"`
	} `json:"labels"`
	ReviewRequests struct {
		Nodes []struct {
			RequestedReviewer *struct {
				Login string `json:"login"`
			} `json:"requestedReviewer"`
		} `json:"nodes"`
	} `json:"reviewRequests"`
	Repository struct {
		NameWithOwner string `json:"nameWithOwner"`
	} `json:"repository"`
}

func (raw githubPullRequest) hasLabel(label string) bool {
	for _, node := range raw.Labels.Nodes {
		if strings.EqualFold(node.Name, label) {
			return true
		}
	}
	return false
}

func (raw githubPullRequest) requests(login string) bool {
	for _, node := range raw.ReviewRequests.Nodes {
		if node.RequestedReviewer != nil && strings.EqualFold(node.RequestedReviewer.Login, login) {
			return true
		}
	}
	return false
}

// decodePullRequest validates the pull request and that the trigger still
// asks for a review. A pull request whose trigger lapsed is superseded.
func (external *External) decodePullRequest(ctx context.Context, raw githubPullRequest, trigger policy.Trigger) (PullRequest, error) {
	if raw.State != "OPEN" {
		return PullRequest{}, workflowError("pull_request_not_open", true, nil)
	}
	repository := strings.ToLower(raw.Repository.NameWithOwner)
	author, bot := "", false
	if raw.Author != nil {
		author, bot = strings.ToLower(raw.Author.Login), raw.Author.Type == "Bot"
	}
	pr := PullRequest{
		Repository: repository, Number: raw.Number, HeadSHA: raw.HeadSHA, Title: strings.TrimSpace(raw.Title), URL: raw.URL,
		Author: author, AuthorBot: bot, HeadBranch: raw.HeadBranch, BaseBranch: raw.BaseBranch, BaseSHA: raw.BaseSHA,
	}
	for _, node := range raw.Labels.Nodes {
		pr.Labels = append(pr.Labels, node.Name)
	}
	if !repositoryPattern.MatchString(pr.Repository) || pr.Number <= 0 || pr.Title == "" || len(pr.Title) > 1024 ||
		!strings.EqualFold(pr.URL, fmt.Sprintf("https://%s/%s/pull/%d", external.cfg.GitHubHost, pr.Repository, pr.Number)) ||
		!shaPattern.MatchString(pr.HeadSHA) || !shaPattern.MatchString(pr.BaseSHA) || !branchPattern.MatchString(pr.BaseBranch) || strings.Contains(pr.BaseBranch, "..") ||
		!authorPattern.MatchString(pr.Author) {
		return PullRequest{}, workflowError("github_response_invalid", true, nil)
	}
	// A draft is not reviewed under any trigger; it becomes a candidate once
	// it is marked ready for review.
	if raw.IsDraft {
		return PullRequest{}, workflowError("pull_request_draft", true, nil)
	}
	switch trigger.Kind {
	case policy.TriggerLabel:
		if !raw.hasLabel(trigger.Label) {
			return PullRequest{}, workflowError("review_label_removed", true, nil)
		}
	case policy.TriggerReviewRequested:
		if !raw.requests(external.cfg.GitHubLogin) {
			return PullRequest{}, workflowError("review_request_not_active", true, nil)
		}
		requestID, err := external.latestReviewRequestID(ctx, pr.Repository, pr.Number)
		if err != nil {
			return PullRequest{}, err
		}
		pr.ReviewRequestID = requestID
	default:
		return PullRequest{}, workflowError("review_trigger_invalid", true, nil)
	}
	return pr, nil
}

// PullRequestState is what the hand-off reminder and the queue need of a pull
// request: whether it is still open and whether the login is requested again.
type PullRequestState struct {
	Open      bool   `json:"open"`
	Requested bool   `json:"requested"`
	Title     string `json:"title"`
	URL       string `json:"url"`
	Author    string `json:"author"`
	HeadSHA   string `json:"head_sha"`
}

func (external *External) ReadPullRequestState(ctx context.Context, repository string, number int) (PullRequestState, error) {
	raw, err := external.fetchPullRequest(ctx, repository, number)
	if err != nil {
		if errorCode(err) == "pull_request_not_open" {
			return PullRequestState{}, nil
		}
		return PullRequestState{}, err
	}
	author := ""
	if raw.Author != nil {
		author = strings.ToLower(raw.Author.Login)
	}
	state := PullRequestState{
		Open: raw.State == "OPEN", Requested: raw.requests(external.cfg.GitHubLogin), Title: strings.TrimSpace(raw.Title), URL: raw.URL, Author: author, HeadSHA: raw.HeadSHA,
	}
	if !strings.EqualFold(raw.Repository.NameWithOwner, repository) || raw.Number != number || !shaPattern.MatchString(state.HeadSHA) || !authorPattern.MatchString(author) ||
		!strings.EqualFold(state.URL, fmt.Sprintf("https://%s/%s/pull/%d", external.cfg.GitHubHost, repository, number)) {
		return PullRequestState{}, workflowError("github_response_invalid", true, nil)
	}
	return state, nil
}

// latestReviewRequestID returns the newest review_requested event for the
// configured login. GitHub keeps one request active across head updates; the
// event tells a renewed request apart, which retries a review of the same head
// that ended without a publication.
func (external *External) latestReviewRequestID(ctx context.Context, repository string, number int) (int64, error) {
	var latestID int64
	var latestAt time.Time
	for page := 1; page <= external.cfg.SearchPageLimit; page++ {
		output, err := external.github(ctx, []string{"api", fmt.Sprintf("repos/%s/issues/%d/events?per_page=100&page=%d", repository, number, page)}, nil)
		if err != nil {
			return 0, err
		}
		var events []struct {
			ID                int64     `json:"id"`
			Event             string    `json:"event"`
			CreatedAt         time.Time `json:"created_at"`
			RequestedReviewer *struct {
				Login string `json:"login"`
			} `json:"requested_reviewer"`
		}
		if json.Unmarshal(output, &events) != nil || events == nil {
			return 0, workflowError("github_response_invalid", true, nil)
		}
		for _, event := range events {
			if event.Event != "review_requested" || event.RequestedReviewer == nil || !strings.EqualFold(event.RequestedReviewer.Login, external.cfg.GitHubLogin) {
				continue
			}
			if event.ID <= 0 || event.CreatedAt.IsZero() {
				return 0, workflowError("github_response_invalid", true, nil)
			}
			if event.CreatedAt.After(latestAt) || event.CreatedAt.Equal(latestAt) && event.ID > latestID {
				latestID, latestAt = event.ID, event.CreatedAt
			}
		}
		if len(events) < 100 {
			break
		}
		if page == external.cfg.SearchPageLimit {
			return 0, workflowError("github_search_limit_exceeded", true, nil)
		}
	}
	if latestID == 0 {
		return 0, workflowError("review_request_event_missing", true, nil)
	}
	return latestID, nil
}

// fetchPullRequest reads the raw pull request data of one pull request.
func (external *External) fetchPullRequest(ctx context.Context, repository string, number int) (githubPullRequest, error) {
	if !repositoryPattern.MatchString(repository) || number <= 0 {
		return githubPullRequest{}, workflowError("review_identity_invalid", true, nil)
	}
	owner, name, _ := strings.Cut(repository, "/")
	output, err := external.github(ctx, []string{"api", "graphql", "-f", "query=" + graphQLPullRequest, "-F", "owner=" + owner, "-F", "name=" + name, "-F", "number=" + strconv.Itoa(number)}, nil)
	if err != nil {
		return githubPullRequest{}, err
	}
	var response struct {
		Data struct {
			Repository *struct {
				PullRequest *githubPullRequest `json:"pullRequest"`
			} `json:"repository"`
		} `json:"data"`
		Errors []json.RawMessage `json:"errors"`
	}
	if err := json.Unmarshal(output, &response); err != nil || len(response.Errors) != 0 {
		return githubPullRequest{}, workflowError("github_response_invalid", true, err)
	}
	if response.Data.Repository == nil || response.Data.Repository.PullRequest == nil {
		return githubPullRequest{}, workflowError("pull_request_not_open", true, nil)
	}
	return *response.Data.Repository.PullRequest, nil
}

func (external *External) GetPullRequest(ctx context.Context, repository string, number int, trigger policy.Trigger) (PullRequest, error) {
	raw, err := external.fetchPullRequest(ctx, repository, number)
	if err != nil {
		return PullRequest{}, err
	}
	pr, err := external.decodePullRequest(ctx, raw, trigger)
	if err != nil {
		return PullRequest{}, err
	}
	if pr.Repository != repository || pr.Number != number {
		return PullRequest{}, workflowError("github_response_invalid", true, nil)
	}
	return pr, nil
}

// ListCandidates runs every trigger search of the policy and keeps a pull
// request only when the trigger that found it is the trigger of its rule. A
// pull request whose data fails validation is rejected without failing the
// others; a failed request to GitHub fails the whole search.
func (external *External) ListCandidates(ctx context.Context) (CandidateList, error) {
	found := map[string]Candidate{}
	rejected := map[string]RejectedCandidate{}
	collect := func(list CandidateList) {
		for _, candidate := range list.Candidates {
			if _, exists := found[candidate.PullRequest.Key()]; !exists {
				found[candidate.PullRequest.Key()] = candidate
			}
		}
		for _, item := range list.Rejected {
			rejected[item.Key()] = item
		}
	}
	if scopes := external.cfg.Policy.ReviewRequestScopes(); len(scopes) > 0 {
		requested, err := external.reviewRequestedCandidates(ctx, scopes)
		if err != nil {
			return CandidateList{}, err
		}
		collect(requested)
	}
	for _, label := range external.cfg.Policy.Labels() {
		labelled, err := external.labelCandidates(ctx, label)
		if err != nil {
			return CandidateList{}, err
		}
		collect(labelled)
	}
	result := CandidateList{Candidates: []Candidate{}, Rejected: []RejectedCandidate{}}
	for _, candidate := range found {
		result.Candidates = append(result.Candidates, candidate)
	}
	for key, item := range rejected {
		if _, ok := found[key]; !ok {
			result.Rejected = append(result.Rejected, item)
		}
	}
	sort.Slice(result.Candidates, func(i, j int) bool {
		return result.Candidates[i].PullRequest.Key() < result.Candidates[j].PullRequest.Key()
	})
	sort.Slice(result.Rejected, func(i, j int) bool { return result.Rejected[i].Key() < result.Rejected[j].Key() })
	return result, nil
}

// classify sorts the outcome of reading one candidate: kept, skipped because
// its trigger lapsed, rejected because its data is invalid, or a failure.
func classify(list *CandidateList, repository string, number int, rule policy.Rule, pr PullRequest, err error) error {
	switch {
	case err == nil:
		list.Candidates = append(list.Candidates, Candidate{PullRequest: pr, Rule: rule})
	case skippableCandidateError(err):
	case errorCode(err) == "github_response_invalid" || errorCode(err) == "review_request_event_missing":
		list.Rejected = append(list.Rejected, RejectedCandidate{Repository: repository, Number: number, Code: errorCode(err)})
	default:
		return err
	}
	return nil
}

func repositoryName(value any) string {
	switch typed := value.(type) {
	case string:
		return typed
	case map[string]any:
		if name, ok := typed["nameWithOwner"].(string); ok {
			return name
		}
	}
	return ""
}

type pullRequestIdentity struct {
	Number     int `json:"number"`
	Repository any `json:"repository"`
}

// searchRequested finds the open pull requests in the scopes that request a
// review from the configured login; extra narrows the search.
func (external *External) searchRequested(ctx context.Context, scopes []string, extra ...string) ([]pullRequestIdentity, error) {
	args := append([]string{"search", "prs", "--review-requested=@me", "--state=open", "--limit", strconv.Itoa(external.cfg.PollLimit), "--json", "number,repository"}, extra...)
	for _, scope := range scopes {
		if owner, name, _ := strings.Cut(scope, "/"); name == "*" {
			args = append(args, "--owner", owner)
		} else {
			args = append(args, "--repo", scope)
		}
	}
	output, err := external.github(ctx, args, nil)
	if err != nil {
		return nil, err
	}
	var identities []pullRequestIdentity
	if err := json.Unmarshal(output, &identities); err != nil || identities == nil {
		return nil, workflowError("github_response_invalid", true, err)
	}
	if len(identities) >= external.cfg.PollLimit {
		// A truncated search would silently drop requested reviews.
		return nil, workflowError("github_search_limit_exceeded", true, nil)
	}
	return identities, nil
}

func (external *External) reviewRequestedCandidates(ctx context.Context, scopes []string) (CandidateList, error) {
	identities, err := external.searchRequested(ctx, scopes, "--draft=false")
	if err != nil {
		return CandidateList{}, err
	}
	list := CandidateList{}
	for _, identity := range identities {
		repository := strings.ToLower(repositoryName(identity.Repository))
		rule, covered := external.cfg.Policy.Resolve(repository)
		if !repositoryPattern.MatchString(repository) || identity.Number <= 0 || !covered || rule.Trigger.Kind != policy.TriggerReviewRequested {
			slog.InfoContext(ctx, "requested review is not covered by the policy", "repository", repository, "pull_request", identity.Number)
			continue
		}
		pr, err := external.GetPullRequest(ctx, repository, identity.Number, rule.Trigger)
		if err := classify(&list, repository, identity.Number, rule, pr, err); err != nil {
			return CandidateList{}, err
		}
	}
	return list, nil
}

const graphQLMarkReady = `mutation($id: ID!) {
  markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { isDraft } }
}`

// ReadyDrafts marks the drafts of the policy's ready-draft authors that ask
// the configured login for a review as ready for review, so the next search
// finds them. Such authors are coding agents that never leave the draft state.
// A pull request that fails a check is left as it is.
func (external *External) ReadyDrafts(ctx context.Context) ([]ReadiedDraft, error) {
	scopes := external.cfg.Policy.ReviewRequestScopes()
	readied := []ReadiedDraft{}
	if len(scopes) == 0 {
		return readied, nil
	}
	for _, author := range external.cfg.Policy.ReadyDraftAuthors() {
		// GitHub search names an app author with the app/ prefix.
		identities, err := external.searchRequested(ctx, scopes, "--draft=true", "--author", "app/"+author)
		if err != nil {
			return readied, err
		}
		for _, identity := range identities {
			repository := strings.ToLower(repositoryName(identity.Repository))
			rule, covered := external.cfg.Policy.Resolve(repository)
			if !covered || rule.Trigger.Kind != policy.TriggerReviewRequested {
				continue
			}
			raw, err := external.fetchPullRequest(ctx, repository, identity.Number)
			if err != nil {
				return readied, err
			}
			if raw.State != "OPEN" || !raw.IsDraft || raw.Author == nil || !strings.EqualFold(raw.Author.Login, author) ||
				!strings.EqualFold(raw.Repository.NameWithOwner, repository) || raw.Number != identity.Number || !raw.requests(external.cfg.GitHubLogin) || raw.ID == "" {
				continue
			}
			output, err := external.github(ctx, []string{"api", "graphql", "-f", "query=" + graphQLMarkReady, "-F", "id=" + raw.ID}, nil)
			if err != nil {
				return readied, err
			}
			var response struct {
				Data struct {
					Mark *struct {
						PullRequest struct {
							IsDraft bool `json:"isDraft"`
						} `json:"pullRequest"`
					} `json:"markPullRequestReadyForReview"`
				} `json:"data"`
				Errors []json.RawMessage `json:"errors"`
			}
			if err := json.Unmarshal(output, &response); err != nil || len(response.Errors) != 0 || response.Data.Mark == nil || response.Data.Mark.PullRequest.IsDraft {
				return readied, workflowError("github_response_invalid", true, err)
			}
			readied = append(readied, ReadiedDraft{Repository: repository, Number: identity.Number, Author: author})
		}
	}
	return readied, nil
}

// skippableCandidateError reports a pull request that changed between the
// search and its detail read; it is simply not a candidate in this poll.
func skippableCandidateError(err error) bool {
	switch errorCode(err) {
	case "pull_request_not_open", "pull_request_draft", "review_request_not_active", "review_label_removed":
		return true
	}
	return false
}

// ReviewRequest is an open, non-draft pull request that asks the configured
// login for a review, in any repository.
type ReviewRequest struct {
	Repository string
	Number     int
	Title      string
	Author     string
	URL        string
}

// ReviewRequestList is the result of a search of requested reviews. Truncated
// reports that the search found more than the requested maximum.
type ReviewRequestList struct {
	Requests  []ReviewRequest
	Truncated bool
}

// ListReviewRequests searches every repository for requested reviews. It asks
// for one result more than max, so Truncated is decided on what GitHub returned
// before any result is skipped. Results whose URL does not name the repository
// and number are skipped, because the URL becomes a link in the queue message.
func (external *External) ListReviewRequests(ctx context.Context, max int) (ReviewRequestList, error) {
	args := []string{"search", "prs", "--review-requested=@me", "--state=open", "--draft=false", "--limit", strconv.Itoa(max + 1), "--json", "number,repository,title,url,author"}
	output, err := external.github(ctx, args, nil)
	if err != nil {
		return ReviewRequestList{}, err
	}
	var items []struct {
		Number     int    `json:"number"`
		Repository any    `json:"repository"`
		Title      string `json:"title"`
		URL        string `json:"url"`
		Author     *struct {
			Login string `json:"login"`
		} `json:"author"`
	}
	if err := json.Unmarshal(output, &items); err != nil || items == nil {
		return ReviewRequestList{}, workflowError("github_response_invalid", true, err)
	}
	list := ReviewRequestList{Requests: make([]ReviewRequest, 0, len(items)), Truncated: len(items) > max}
	for _, item := range items {
		repository := strings.ToLower(repositoryName(item.Repository))
		expectedURL := fmt.Sprintf("https://%s/%s/pull/%d", external.cfg.GitHubHost, repository, item.Number)
		if !repositoryPattern.MatchString(repository) || item.Number <= 0 || !strings.EqualFold(item.URL, expectedURL) {
			slog.WarnContext(ctx, "review request skipped: unexpected search result", "repository", repository, "pull_request", item.Number)
			continue
		}
		request := ReviewRequest{Repository: repository, Number: item.Number, Title: item.Title, URL: expectedURL}
		if item.Author != nil {
			request.Author = item.Author.Login
		}
		list.Requests = append(list.Requests, request)
	}
	return list, nil
}

// labelSearchQuery limits the label search to the owners and repositories of
// the rules that use the label, so foreign pull requests never enter.
func (external *External) labelSearchQuery(label string) string {
	return fmt.Sprintf("is:pr is:open draft:false label:%q %s sort:created-asc", label, strings.Join(external.cfg.Policy.LabelScopes(label), " "))
}

func (external *External) labelCandidates(ctx context.Context, label string) (CandidateList, error) {
	query := external.labelSearchQuery(label)
	after := ""
	list := CandidateList{}
	for page := 0; page < external.cfg.SearchPageLimit; page++ {
		args := []string{"api", "graphql", "-f", "query=" + graphQLSearch, "-F", "searchQuery=" + query}
		if after != "" {
			args = append(args, "-F", "after="+after)
		}
		output, err := external.github(ctx, args, nil)
		if err != nil {
			return CandidateList{}, err
		}
		var response struct {
			Data struct {
				Search struct {
					PageInfo struct {
						HasNextPage bool   `json:"hasNextPage"`
						EndCursor   string `json:"endCursor"`
					} `json:"pageInfo"`
					Nodes []githubPullRequest `json:"nodes"`
				} `json:"search"`
			} `json:"data"`
			Errors []json.RawMessage `json:"errors"`
		}
		if err := json.Unmarshal(output, &response); err != nil || response.Data.Search.Nodes == nil || len(response.Errors) != 0 {
			return CandidateList{}, workflowError("github_response_invalid", true, err)
		}
		for _, raw := range response.Data.Search.Nodes {
			repository := strings.ToLower(raw.Repository.NameWithOwner)
			rule, covered := external.cfg.Policy.Resolve(repository)
			if !covered || rule.Trigger.Kind != policy.TriggerLabel || rule.Trigger.Label != label {
				continue
			}
			pr, err := external.decodePullRequest(ctx, raw, rule.Trigger)
			if err := classify(&list, repository, raw.Number, rule, pr, err); err != nil {
				return CandidateList{}, err
			}
		}
		if !response.Data.Search.PageInfo.HasNextPage {
			return list, nil
		}
		if after = response.Data.Search.PageInfo.EndCursor; after == "" {
			return CandidateList{}, workflowError("github_response_invalid", true, nil)
		}
	}
	return CandidateList{}, workflowError("github_search_limit_exceeded", true, nil)
}

func commentableLinesFromPatch(patch string) ([]int, error) {
	commentable := []int{}
	lineNumber := 0
	inHunk := false
	emptyNewRange := false
	for _, line := range strings.Split(patch, "\n") {
		if strings.HasPrefix(line, "@@") {
			matches := hunkPattern.FindStringSubmatch(line)
			if matches == nil {
				return nil, workflowError("github_response_invalid", true, nil)
			}
			parsed, err := strconv.Atoi(matches[1])
			if err != nil {
				return nil, workflowError("github_response_invalid", true, nil)
			}
			newCount := 1
			if matches[2] != "" {
				if newCount, err = strconv.Atoi(matches[2]); err != nil {
					return nil, workflowError("github_response_invalid", true, nil)
				}
			}
			// Deleted or emptied files use +0,0 and have no right-side anchors.
			if parsed == 0 && newCount != 0 {
				return nil, workflowError("github_response_invalid", true, nil)
			}
			emptyNewRange = newCount == 0
			lineNumber = parsed
			inHunk = true
			continue
		}
		if !inHunk || line == "" || strings.HasPrefix(line, "\\ No newline at end of file") {
			continue
		}
		if emptyNewRange && (line[0] == '+' || line[0] == ' ') {
			return nil, workflowError("github_response_invalid", true, nil)
		}
		switch line[0] {
		case '+':
			commentable = append(commentable, lineNumber)
			lineNumber++
		case ' ':
			lineNumber++
		case '-':
		default:
			return nil, workflowError("github_response_invalid", true, nil)
		}
	}
	return commentable, nil
}

// githubFile is one entry of the files of a pull request.
type githubFile struct {
	Filename string `json:"filename"`
	Status   string `json:"status"`
	Patch    string `json:"patch"`
}

// listFiles visits every changed file of the pull request once, with a
// validated relative path.
func (external *External) listFiles(ctx context.Context, pr PullRequest, visit func(githubFile) error) error {
	seen := map[string]bool{}
	for page := 1; page <= external.cfg.SearchPageLimit; page++ {
		output, err := external.github(ctx, []string{"api", fmt.Sprintf("repos/%s/pulls/%d/files?per_page=100&page=%d", pr.Repository, pr.Number, page)}, nil)
		if err != nil {
			return err
		}
		var files []githubFile
		if json.Unmarshal(output, &files) != nil || files == nil {
			return workflowError("github_response_invalid", true, nil)
		}
		for _, file := range files {
			cleanPath := path.Clean(file.Filename)
			if cleanPath != file.Filename || path.IsAbs(cleanPath) || strings.HasPrefix(cleanPath, "../") || seen[file.Filename] {
				return workflowError("github_response_invalid", true, nil)
			}
			seen[file.Filename] = true
			if err := visit(file); err != nil {
				return err
			}
		}
		if len(files) < 100 {
			return nil
		}
	}
	return workflowError("github_search_limit_exceeded", true, nil)
}

func (external *External) ListCommentableLines(ctx context.Context, pr PullRequest) (CommentableLines, error) {
	result := CommentableLines{}
	err := external.listFiles(ctx, pr, func(file githubFile) error {
		lines, err := commentableLinesFromPatch(file.Patch)
		if err != nil {
			return err
		}
		result[file.Filename] = lines
		return nil
	})
	if err != nil {
		return nil, err
	}
	return result, nil
}

func (external *External) ListDeletedFiles(ctx context.Context, pr PullRequest) ([]string, error) {
	deleted := []string{}
	err := external.listFiles(ctx, pr, func(file githubFile) error {
		if file.Status == "removed" {
			deleted = append(deleted, file.Filename)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return deleted, nil
}

type githubReviewRecord struct {
	ID       int64  `json:"id"`
	URL      string `json:"html_url"`
	Body     string `json:"body"`
	CommitID string `json:"commit_id"`
	State    string `json:"state"`
	User     struct {
		Login string `json:"login"`
	} `json:"user"`
}

func reviewState(decision Decision) string {
	switch decision {
	case DecisionApprove:
		return "APPROVED"
	case DecisionRequestChanges:
		return "CHANGES_REQUESTED"
	}
	return "COMMENTED"
}

// findPublishedReview returns the review of this login that carries marker on
// the exact head with the expected state.
func (external *External) findPublishedReview(ctx context.Context, pr PullRequest, review GitHubReview) (PublishResult, bool, error) {
	for page := 1; page <= external.cfg.SearchPageLimit; page++ {
		output, err := external.github(ctx, []string{"api", fmt.Sprintf("repos/%s/pulls/%d/reviews?per_page=100&page=%d", pr.Repository, pr.Number, page)}, nil)
		if err != nil {
			return PublishResult{}, false, err
		}
		var reviews []githubReviewRecord
		if json.Unmarshal(output, &reviews) != nil || reviews == nil {
			return PublishResult{}, false, workflowError("github_response_invalid", true, nil)
		}
		for _, existing := range reviews {
			if strings.Contains(existing.Body, review.Marker) && existing.ID > 0 && existing.URL != "" &&
				existing.CommitID == pr.HeadSHA && strings.EqualFold(existing.User.Login, external.cfg.GitHubLogin) {
				if existing.State != reviewState(review.Decision) && existing.State != "DISMISSED" {
					return PublishResult{}, false, workflowError("github_review_state_conflict", true, nil)
				}
				return PublishResult{Decision: review.Decision, GitHubReviewID: existing.ID, ReviewURL: existing.URL, Reused: true}, true, nil
			}
		}
		if len(reviews) < 100 {
			return PublishResult{}, false, nil
		}
	}
	return PublishResult{}, false, workflowError("github_search_limit_exceeded", true, nil)
}

func (external *External) PublishReview(ctx context.Context, pr PullRequest, review GitHubReview) (PublishResult, error) {
	if review.Marker == "" || !strings.Contains(review.Body, review.Marker) {
		return PublishResult{}, workflowError("github_review_invalid", true, nil)
	}
	if existing, found, err := external.findPublishedReview(ctx, pr, review); err != nil || found {
		return existing, err
	}
	payload := map[string]any{"commit_id": pr.HeadSHA, "event": string(review.Decision), "body": review.Body}
	if len(review.Comments) > 0 {
		payload["comments"] = review.Comments
	}
	output, postErr := external.github(ctx, []string{"api", "--method", "POST", fmt.Sprintf("repos/%s/pulls/%d/reviews", pr.Repository, pr.Number), "--input", "-"}, payload)
	if postErr != nil {
		// A lost acknowledgement may follow a successful write; reconcile by marker.
		if existing, found, lookupErr := external.findPublishedReview(ctx, pr, review); lookupErr == nil && found {
			return existing, nil
		}
		return PublishResult{}, postErr
	}
	var posted githubReviewRecord
	if json.Unmarshal(output, &posted) != nil || posted.ID <= 0 || posted.URL == "" {
		return PublishResult{}, workflowError("github_response_invalid", true, nil)
	}
	return PublishResult{Decision: review.Decision, GitHubReviewID: posted.ID, ReviewURL: posted.URL}, nil
}
