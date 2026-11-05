package main

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

// queueConfig is the synthetic configuration, which covers the review policy
// rules the queue annotations depend on.
func queueConfig(t *testing.T) Config {
	t.Helper()
	cfg, err := loadDeployed(t, deployedEnvironment(t))
	if err != nil {
		t.Fatal(err)
	}
	return cfg
}

func queueRequest(repository string, number int, title string) ReviewRequest {
	return ReviewRequest{
		Repository: repository, Number: number, Title: title, Author: "someone",
		URL: "https://github.com/" + repository + "/pull/" + strconv.Itoa(number),
	}
}

func TestQueueStatusAnnotatesEachEngineState(t *testing.T) {
	cfg := queueConfig(t)
	const connection = "example-org/example-repo"
	const labelled = "example-owner/tool"
	const unlisted = "other/unlisted"
	cases := []struct {
		name       string
		repository string
		state      func(*discoveryState, string)
		status     string
		priority   int
	}{
		{"awaiting approval", connection, func(s *discoveryState, key string) {
			s.awaiting[key] = AwaitRecord{Repository: connection, Number: 7}
		}, "AI review clean, waiting for your approve", queueAwaitingApproval},
		{"running", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "running"}
		}, "AI review running", queueOther},
		{"waiting", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "waiting"}
		}, "AI review queued", queueOther},
		{"retry wait", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "retry_wait"}
		}, "AI review queued", queueOther},
		{"stopped with code", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "failed", ErrorCode: "pohunek_session_failed"}
		}, "AI review stopped: pohunek_session_failed", queueStopped},
		{"stopped without code", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "failed"}
		}, "AI review stopped: unknown", queueStopped},
		{"withdrawn", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "withdrawn"}
		}, "AI review withdrawn", queueOther},
		{"unknown phase", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "paused"}
		}, "AI review state unknown: paused", queueOther},
		{"published while still requested", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "completed"}
			s.published[key] = PublishedRecord{Decision: DecisionComment}
		}, "AI review published COMMENT", queueOther},
		{"completed without published record", connection, func(s *discoveryState, key string) {
			s.schedule[key] = schedule.Record{Phase: "completed"}
		}, "AI review completed", queueOther},
		{"covered and not yet started", connection, func(*discoveryState, string) {}, "AI review not started yet", queueOther},
		{"label trigger without engine state", labelled, func(*discoveryState, string) {}, "not covered by AI review", queueNotCovered},
		{"no policy rule", unlisted, func(*discoveryState, string) {}, "not covered by AI review", queueNotCovered},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			state := discoveryState{schedule: map[string]schedule.Record{}, published: map[string]PublishedRecord{}, awaiting: map[string]AwaitRecord{}}
			request := queueRequest(tc.repository, 7, "title")
			tc.state(&state, request.Repository+"#7")
			status, priority := queueStatus(cfg.Policy, state, request, request.Repository+"#7")
			if status != tc.status || priority != tc.priority {
				t.Fatalf("status %q priority %d, want %q priority %d", status, priority, tc.status, tc.priority)
			}
		})
	}
}

func TestQueueEntriesPutActionItemsFirstAndSortByKey(t *testing.T) {
	cfg := queueConfig(t)
	state := discoveryState{schedule: map[string]schedule.Record{}, published: map[string]PublishedRecord{}, awaiting: map[string]AwaitRecord{}}
	state.schedule["example-org/example-repo#1"] = schedule.Record{Phase: "running"}
	state.awaiting["example-org/zeta#9"] = AwaitRecord{Repository: "example-org/zeta", Number: 9}
	state.schedule["example-org/beta#2"] = schedule.Record{Phase: "failed", ErrorCode: "x"}
	requests := []ReviewRequest{
		queueRequest("example-org/example-repo", 1, "running"),
		queueRequest("other/unlisted", 3, "not covered"),
		queueRequest("example-org/beta", 2, "stopped"),
		queueRequest("example-org/zeta", 9, "awaiting"),
		queueRequest("example-org/alpha", 4, "not started"),
	}
	entries := queueEntries(cfg.Policy, state, requests)
	var order []string
	for _, entry := range entries {
		order = append(order, entry.request.Title)
	}
	want := []string{"awaiting", "not covered", "stopped", "not started", "running"}
	if strings.Join(order, ",") != strings.Join(want, ",") {
		t.Fatalf("order %v, want %v", order, want)
	}
}

func TestRenderQueueEscapesTextAndLinksOnlyTheVerifiedURL(t *testing.T) {
	request := queueRequest("example-org/example-repo", 12, `<img src=x onerror="alert(1)"> & "quoted"`)
	request.Author = "dev<script>"
	entries := []queueEntry{{request: request, status: "AI review running", priority: queueOther}}
	plain, formatted, shown, dropped := renderQueue(entries, false, 8000, 512)
	if shown != 1 || dropped {
		t.Fatalf("shown %d dropped %v", shown, dropped)
	}
	if strings.Contains(formatted, "<img") || strings.Contains(formatted, "<script") {
		t.Fatalf("formatted body carries raw markup: %s", formatted)
	}
	if !strings.Contains(formatted, `<a href="https://github.com/example-org/example-repo/pull/12">`) {
		t.Fatalf("missing verified link: %s", formatted)
	}
	if !strings.Contains(formatted, "&lt;img src=x onerror=&#34;alert(1)&#34;&gt; &amp; &#34;quoted&#34;") {
		t.Fatalf("title not escaped: %s", formatted)
	}
	if !strings.Contains(plain, "Reviews waiting for you (1)") || !strings.Contains(plain, "https://github.com/example-org/example-repo/pull/12") {
		t.Fatalf("plain body lacks header or URL: %s", plain)
	}
}

func TestRenderQueueDropsWholeEntriesThatDoNotFit(t *testing.T) {
	var entries []queueEntry
	for number := 1; number <= 5; number++ {
		entries = append(entries, queueEntry{request: queueRequest("example-org/example-repo", number, strings.Repeat("t", 40)), status: "AI review running"})
	}
	plain, formatted, shown, dropped := renderQueue(entries, false, 600, 512)
	if shown == 0 || shown >= len(entries) || !dropped {
		t.Fatalf("shown %d dropped %v", shown, dropped)
	}
	if !strings.Contains(plain, "truncated") || !strings.Contains(formatted, "truncated") {
		t.Fatalf("truncation note missing")
	}
	if strings.Count(formatted, "<li>") != shown || !strings.HasSuffix(formatted, "</p>") {
		t.Fatalf("formatted body is not a complete list: %s", formatted)
	}
	if len([]rune(plain)) > 600 || len([]rune(formatted)) > 600 {
		t.Fatalf("bodies exceed the limit: %d %d", len([]rune(plain)), len([]rune(formatted)))
	}
}

func TestCleanTextCollapsesControlCharactersAndLimitsLength(t *testing.T) {
	if got := cleanText("a\n\tb\x00  c", 10); got != "a b c" {
		t.Fatalf("got %q", got)
	}
	if got := cleanText("abcdef", 4); got != "abc…" {
		t.Fatalf("got %q", got)
	}
}

// newQueueEngine builds an engine on a fresh store with the fake integration.
func newQueueEngine(t *testing.T, cfg Config, fake *fakeIntegration, at time.Time) (*Engine, *Store) {
	t.Helper()
	store, err := OpenStore(filepath.Join(t.TempDir(), "state.db"), time.Now)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	noSleep := func(context.Context, time.Duration) error { return nil }
	return NewEngine(cfg, store, fake, logger, func() time.Time { return at }, noSleep), store
}

func TestReviewQueueSendsNothingWhenNoRequestWaits(t *testing.T) {
	fake := newFake(t)
	engine, _ := newQueueEngine(t, queueConfig(t), fake, time.Date(2026, 10, 19, 14, 0, 0, 0, time.UTC))
	result, err := engine.ReviewQueue(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if result != (QueueResult{}) || len(fake.queued) != 0 {
		t.Fatalf("result %+v queued %d", result, len(fake.queued))
	}
}

func TestReviewQueueListsTheRequestsAndTruncatesAtTheLimit(t *testing.T) {
	cfg := queueConfig(t)
	cfg.QueueMaxItems = 2
	fake := newFake(t)
	fake.requests = []ReviewRequest{
		queueRequest("example-org/example-repo", 1, "first"),
		queueRequest("example-org/example-repo", 2, "second"),
		queueRequest("example-org/example-repo", 3, "third"),
	}
	engine, _ := newQueueEngine(t, cfg, fake, time.Date(2026, 10, 19, 14, 0, 0, 0, time.UTC))
	result, err := engine.ReviewQueue(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if result != (QueueResult{Count: 2, Sent: true, Truncated: true}) {
		t.Fatalf("result %+v", result)
	}
	if len(fake.queued) != 1 || !strings.Contains(fake.queued[0].plain, "Reviews waiting for you (2)") || !strings.Contains(fake.queued[0].plain, "truncated") {
		t.Fatalf("queued %+v", fake.queued)
	}
}

func TestReviewQueueRetriesWithOneTransactionPerRun(t *testing.T) {
	fake := newFake(t)
	fake.requests = []ReviewRequest{queueRequest("example-org/example-repo", 1, "first")}
	fake.queueFailures = 2
	engine, _ := newQueueEngine(t, queueConfig(t), fake, time.Date(2026, 10, 19, 14, 35, 0, 0, time.UTC))
	result, err := engine.ReviewQueue(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if !result.Sent || fake.queueSends != 3 {
		t.Fatalf("result %+v sends %d", result, fake.queueSends)
	}
	first := fake.queueTransactions[0]
	if !regexp.MustCompile(`^queue-[0-9a-f]{32}$`).MatchString(first) {
		t.Fatalf("transaction %q", first)
	}
	for _, transaction := range fake.queueTransactions {
		if transaction != first {
			t.Fatalf("retries used transactions %v", fake.queueTransactions)
		}
	}
	// A second run in the same hour posts its own message.
	if _, err := engine.ReviewQueue(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(fake.queued) != 2 || fake.queued[1].transaction == first {
		t.Fatalf("queued %+v", fake.queued)
	}
}

func TestReviewQueueGivesUpWhenTheRetryWindowEnds(t *testing.T) {
	cfg := queueConfig(t)
	cfg.StepRetry = 0
	fake := newFake(t)
	fake.requests = []ReviewRequest{queueRequest("example-org/example-repo", 1, "first")}
	fake.queueFailures = 1000
	engine, _ := newQueueEngine(t, cfg, fake, time.Date(2026, 10, 19, 14, 0, 0, 0, time.UTC))
	if _, err := engine.ReviewQueue(context.Background()); err == nil || errorCode(err) != "matrix_unavailable" {
		t.Fatalf("error %v", err)
	}
	if fake.queueSends != 1 || len(fake.queued) != 0 {
		t.Fatalf("sends %d queued %d", fake.queueSends, len(fake.queued))
	}
}

// roundTripFunc answers HTTP requests without touching the network.
type roundTripFunc func(*http.Request) (*http.Response, error)

func (fn roundTripFunc) RoundTrip(request *http.Request) (*http.Response, error) { return fn(request) }

// recordingRunner answers the gh command with a fixed output and keeps the arguments.
type recordingRunner struct {
	args   []string
	output string
}

func (runner *recordingRunner) Run(_ context.Context, _ string, args []string, _ []byte, _ []string) ([]byte, error) {
	runner.args = args
	return []byte(runner.output), nil
}

func TestSendQueueDigestPostsWithoutMentionsOnItsOwnRoom(t *testing.T) {
	cfg := queueConfig(t)
	cfg.MatrixHomeserverURL = "https://matrix.example.org"
	var method, path string
	var payload map[string]any
	external := NewExternal(cfg)
	external.httpClient = &http.Client{Transport: roundTripFunc(func(request *http.Request) (*http.Response, error) {
		method, path = request.Method, request.URL.Path
		if err := json.NewDecoder(request.Body).Decode(&payload); err != nil {
			t.Fatal(err)
		}
		return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(bytes.NewReader([]byte("{}"))), Header: http.Header{}}, nil
	})}
	if err := external.SendQueueDigest(context.Background(), "queue-2026-10-19T14", "plain", "<p>html</p>"); err != nil {
		t.Fatal(err)
	}
	// request.URL.Path is decoded, so the room is compared as configured.
	if method != http.MethodPut || path != "/_matrix/client/v3/rooms/"+cfg.QueueMatrixRoom+"/send/m.room.message/queue-2026-10-19T14" {
		t.Fatalf("request %s %s", method, path)
	}
	mentions, ok := payload["m.mentions"].(map[string]any)
	if !ok || len(mentions) != 0 {
		t.Fatalf("mentions %v", payload["m.mentions"])
	}
	if payload["msgtype"] != "m.notice" || payload["format"] != "org.matrix.custom.html" || payload["formatted_body"] != "<p>html</p>" || payload["body"] != "plain" {
		t.Fatalf("payload %v", payload)
	}
}

func TestSendMatrixStillMentionsOnlyOnActionLevel(t *testing.T) {
	cfg := queueConfig(t)
	cfg.MatrixHomeserverURL = "https://matrix.example.org"
	var mentions []map[string]any
	external := NewExternal(cfg)
	external.httpClient = &http.Client{Transport: roundTripFunc(func(request *http.Request) (*http.Response, error) {
		var payload map[string]any
		if err := json.NewDecoder(request.Body).Decode(&payload); err != nil {
			t.Fatal(err)
		}
		mentions = append(mentions, payload["m.mentions"].(map[string]any))
		return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(bytes.NewReader([]byte("{}"))), Header: http.Header{}}, nil
	})}
	if err := external.sendMatrix(context.Background(), policy.LevelAction, "action"); err != nil {
		t.Fatal(err)
	}
	if err := external.sendMatrix(context.Background(), policy.LevelDigest, "digest"); err != nil {
		t.Fatal(err)
	}
	if _, ok := mentions[0]["user_ids"]; !ok || len(mentions[1]) != 0 {
		t.Fatalf("mentions %v", mentions)
	}
}

func TestListReviewRequestsSearchesAllRepositoriesAndSkipsForeignURLs(t *testing.T) {
	cfg := queueConfig(t)
	runner := &recordingRunner{output: `[
	  {"number":5,"repository":{"nameWithOwner":"Example-Org/Example-Repo"},"title":"ok","url":"https://github.com/example-org/example-repo/pull/5","author":{"login":"alice"}},
	  {"number":6,"repository":{"nameWithOwner":"example-org/example-repo"},"title":"bad","url":"https://evil.example/example-org/example-repo/pull/6","author":{"login":"bob"}}
	]`}
	external := NewExternal(cfg)
	external.runner = runner
	list, err := external.ListReviewRequests(context.Background(), 50)
	if err != nil {
		t.Fatal(err)
	}
	if list.Truncated || len(list.Requests) != 1 || list.Requests[0].Repository != "example-org/example-repo" || list.Requests[0].Number != 5 || list.Requests[0].Author != "alice" {
		t.Fatalf("list %+v", list)
	}
	args := strings.Join(runner.args, " ")
	for _, want := range []string{"search prs", "--review-requested=@me", "--draft=false", "--limit 51"} {
		if !strings.Contains(args, want) {
			t.Fatalf("args %q lack %q", args, want)
		}
	}
	if strings.Contains(args, "--repo") || strings.Contains(args, "--owner") {
		t.Fatalf("search is scoped to policy repositories: %q", args)
	}
}

func TestReviewQueueConfigurationIsRequired(t *testing.T) {
	for _, name := range []string{"HERMES_PR_REVIEW_QUEUE_MATRIX_ROOM", "HERMES_PR_REVIEW_QUEUE_MAX_ITEMS"} {
		values := deployedEnvironment(t)
		delete(values, name)
		if _, err := loadDeployed(t, values); err == nil || !strings.Contains(err.Error(), name) {
			t.Fatalf("missing %s: error = %v", name, err)
		}
	}
	values := deployedEnvironment(t)
	values["HERMES_PR_REVIEW_QUEUE_MAX_ITEMS"] = "1000"
	if _, err := loadDeployed(t, values); err == nil {
		t.Fatal("queue limit at the gh search cap was accepted")
	}
	values = deployedEnvironment(t)
	values["HERMES_PR_REVIEW_QUEUE_MATRIX_ROOM"] = "not-a-room"
	if _, err := loadDeployed(t, values); err == nil {
		t.Fatal("invalid queue room was accepted")
	}
}

func TestListReviewRequestsReportsTruncationBeforeSkippingResults(t *testing.T) {
	cfg := queueConfig(t)
	external := NewExternal(cfg)
	external.runner = &recordingRunner{output: `[
	  {"number":5,"repository":{"nameWithOwner":"example-org/example-repo"},"title":"ok","url":"https://github.com/example-org/example-repo/pull/5","author":{"login":"alice"}},
	  {"number":6,"repository":{"nameWithOwner":"example-org/example-repo"},"title":"bad","url":"https://evil.example/x","author":{"login":"bob"}}
	]`}
	list, err := external.ListReviewRequests(context.Background(), 1)
	if err != nil {
		t.Fatal(err)
	}
	if !list.Truncated || len(list.Requests) != 1 {
		t.Fatalf("a skipped result hid the truncation: %+v", list)
	}
}

// handOff records pr as published notes handed to its author at publishedAt
// and makes it an open pull request that no longer requests the review.
func handOff(t *testing.T, engine *Engine, fake *fakeIntegration, pr PullRequest, publishedAt time.Time, reminded bool) {
	t.Helper()
	fake.mu.Lock()
	fake.current[pr.Key()] = pr
	fake.mu.Unlock()
	record := PublishedRecord{Repository: pr.Repository, Number: pr.Number, HeadSHA: pr.HeadSHA, ReviewRequestID: pr.ReviewRequestID, Decision: DecisionComment,
		WorkflowID: pr.WorkflowID(0), PublishedAt: publishedAt, AwaitsAuthor: true, Reminded: reminded}
	if err := engine.markPublished(context.Background(), record); err != nil {
		t.Fatal(err)
	}
}

func TestUnansweredNotesHandoffRemindsOnceAndEndsWhenClosed(t *testing.T) {
	at := time.Date(2026, 10, 23, 12, 0, 0, 0, time.UTC)
	fake := newFake(t)
	cfg := integrationConfig(t)
	cfg.PublishedRetention = 90 * 24 * time.Hour
	engine, store := newQueueEngine(t, cfg, fake, at)
	fresh, stale, closed := integrationPullRequest("acme/notes", sha("a")), integrationPullRequest("acme/notes", sha("a")), integrationPullRequest("acme/notes", sha("a"))
	fresh.Number, stale.Number, closed.Number = 101, 102, 103
	handOff(t, engine, fake, fresh, at.Add(-23*time.Hour), false)
	handOff(t, engine, fake, stale, at.Add(-25*time.Hour), false)
	handOff(t, engine, fake, closed, at.Add(-30*time.Hour), false)
	fake.close(closed)
	for range 2 {
		if _, err := engine.Poll(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	reminders := []note{}
	_, notes, _, _ := fake.snapshot()
	for _, entry := range notes {
		if strings.Contains(entry.Message, "wait for the author") {
			reminders = append(reminders, entry)
		}
	}
	if len(reminders) != 1 || reminders[0].Level != policy.LevelAction || !strings.Contains(reminders[0].Message, "acme/notes PR #102 wait for the author for 25 h") || strings.Contains(reminders[0].Message, "Review ID") {
		t.Fatalf("reminders = %+v", reminders)
	}
	state, err := store.loadDiscovery(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got := state.published[stale.Key()]; !got.AwaitsAuthor || !got.Reminded {
		t.Fatalf("stale = %+v", got)
	}
	if got := state.published[fresh.Key()]; !got.AwaitsAuthor || got.Reminded {
		t.Fatalf("fresh = %+v", got)
	}
	if got := state.published[closed.Key()]; got.AwaitsAuthor {
		t.Fatalf("a closed pull request must end its hand-off: %+v", got)
	}
}

func TestReviewQueueListsNotesWaitingOnTheirAuthorApart(t *testing.T) {
	at := time.Date(2026, 10, 23, 12, 0, 0, 0, time.UTC)
	fake := newFake(t)
	engine, _ := newQueueEngine(t, queueConfig(t), fake, at)
	waiting := PullRequest{Repository: "example-org/example-repo", Number: 5, HeadSHA: sha("a"), ReviewRequestID: 3, Title: "Optional polish", URL: "https://github.com/example-org/example-repo/pull/5", Author: "colleague"}
	handOff(t, engine, fake, waiting, at.Add(-30*time.Hour), true)
	result, err := engine.ReviewQueue(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if !result.Sent || result.Count != 1 || strings.Contains(fake.queued[0].plain, "Reviews waiting for you") ||
		!strings.Contains(fake.queued[0].plain, "Waiting on author (1)\n- Optional polish (example-org/example-repo#5, by colleague): notes handed to the author 30 h ago, reminded") {
		t.Fatalf("result %+v queued %+v", result, fake.queued)
	}
	fake.requests = []ReviewRequest{queueRequest("example-org/example-repo", 1, "first")}
	if _, err := engine.ReviewQueue(context.Background()); err != nil {
		t.Fatal(err)
	}
	plain, formatted := fake.queued[1].plain, fake.queued[1].formatted
	if !strings.Contains(plain, "Reviews waiting for you (1)") || strings.Index(plain, "Waiting on author (1)") < strings.Index(plain, "first") ||
		strings.Count(formatted, "<ul>") != 2 || strings.Count(formatted, "</ul>") != 2 {
		t.Fatalf("plain=%s formatted=%s", plain, formatted)
	}
}
