package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"html"
	"sort"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

// Queue priorities. Requests that need the user's action sort first, and
// requests that only the engine works on follow.
const (
	queueAwaitingApproval = iota
	queueNotCovered
	queueStopped
	queueOther
	// queueWaitingAuthor lists notes handed to an author, apart from the
	// requests, because the author has to act first.
	queueWaitingAuthor
)

// QueueResult is what the review-queue command prints. Count is the number of
// requests in the message; Truncated reports requests that did not fit.
type QueueResult struct {
	Count     int  `json:"count"`
	Sent      bool `json:"sent"`
	Truncated bool `json:"truncated"`
}

type queueEntry struct {
	request  ReviewRequest
	status   string
	priority int
}

// ReviewQueue posts the review requests that wait for the configured login and
// the notes that wait for their author. Nothing is posted when nothing waits.
func (engine *Engine) ReviewQueue(ctx context.Context) (QueueResult, error) {
	limit := engine.cfg.QueueMaxItems
	list, err := retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, func(ctx context.Context) (ReviewRequestList, error) {
		return engine.integration.ListReviewRequests(ctx, limit)
	})
	if err != nil {
		return QueueResult{}, err
	}
	state, err := engine.store.loadDiscovery(ctx)
	if err != nil {
		return QueueResult{}, err
	}
	entries := append(queueEntries(engine.cfg.Policy, state, list.Requests), engine.waitingOnAuthor(ctx, state, list.Requests)...)
	if len(entries) == 0 {
		return QueueResult{}, nil
	}
	// Sorting happens before the cut, so a request that needs the user's
	// action is never dropped in favour of one that only waits for the engine.
	truncated := list.Truncated || len(entries) > limit
	if len(entries) > limit {
		entries = entries[:limit]
	}
	plain, formatted, shown, dropped := renderQueue(entries, truncated, engine.cfg.MatrixMaxCharacters, engine.cfg.MaxTitleCharacters)
	if shown == 0 {
		return QueueResult{}, workflowError("review_queue_too_long", true, nil)
	}
	// One transaction ID per run: a retry reuses it, so the homeserver returns
	// the original event instead of posting a second copy, while a later manual
	// run still posts its own message.
	transaction, err := queueTransactionID()
	if err != nil {
		return QueueResult{}, workflowError("matrix_request_invalid", true, err)
	}
	_, err = retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, func(ctx context.Context) (bool, error) {
		return true, engine.integration.SendQueueDigest(ctx, transaction, plain, formatted)
	})
	if err != nil {
		return QueueResult{}, err
	}
	return QueueResult{Count: shown, Sent: true, Truncated: dropped}, nil
}

// waitingOnAuthor returns the open pull requests whose notes were handed to
// their author, who has not requested the review again. One that cannot be
// read is left out of this digest.
func (engine *Engine) waitingOnAuthor(ctx context.Context, state discoveryState, requests []ReviewRequest) []queueEntry {
	requested := map[string]bool{}
	for _, request := range requests {
		requested[fmt.Sprintf("%s#%d", request.Repository, request.Number)] = true
	}
	keys := make([]string, 0, len(state.published))
	for key, record := range state.published {
		if record.AwaitsAuthor && !requested[key] {
			keys = append(keys, key)
		}
	}
	sort.Strings(keys)
	now := engine.now().UTC()
	entries := []queueEntry{}
	for _, key := range keys {
		record := state.published[key]
		current, err := retryStep(ctx, engine.stepPolicy(), engine.now, engine.sleep, func(ctx context.Context) (PullRequestState, error) {
			return engine.integration.ReadPullRequestState(ctx, record.Repository, record.Number)
		})
		if err != nil {
			engine.logger.WarnContext(ctx, "notes hand-off unreadable", "repository", record.Repository, "pull_request", record.Number, "error_code", errorCode(err))
			continue
		}
		if !current.Open || current.Requested {
			continue
		}
		status := "notes handed to the author " + handoffAge(record, now) + " ago"
		if record.Reminded {
			status += ", reminded"
		}
		entries = append(entries, queueEntry{
			request:  ReviewRequest{Repository: record.Repository, Number: record.Number, Title: current.Title, Author: current.Author, URL: current.URL},
			status:   status,
			priority: queueWaitingAuthor,
		})
	}
	return entries
}

func queueTransactionID() (string, error) {
	random := make([]byte, 16)
	if _, err := rand.Read(random); err != nil {
		return "", err
	}
	return "queue-" + hex.EncodeToString(random), nil
}

// queueEntries annotates every request with its engine state and sorts the
// entries by priority, then by repository and number.
func queueEntries(pol *policy.Policy, state discoveryState, requests []ReviewRequest) []queueEntry {
	entries := make([]queueEntry, 0, len(requests))
	for _, request := range requests {
		key := fmt.Sprintf("%s#%d", request.Repository, request.Number)
		status, priority := queueStatus(pol, state, request, key)
		entries = append(entries, queueEntry{request: request, status: status, priority: priority})
	}
	sort.SliceStable(entries, func(i, j int) bool {
		a, b := entries[i], entries[j]
		if a.priority != b.priority {
			return a.priority < b.priority
		}
		if a.request.Repository != b.request.Repository {
			return a.request.Repository < b.request.Repository
		}
		return a.request.Number < b.request.Number
	})
	return entries
}

// queueStatus explains where the engine stands with one request. The state
// records are kept current by the serving engine, so they are read as they are.
func queueStatus(pol *policy.Policy, state discoveryState, request ReviewRequest, key string) (string, int) {
	if _, ok := state.awaiting[key]; ok {
		return "AI review clean, waiting for your approve", queueAwaitingApproval
	}
	if record, ok := state.schedule[key]; ok && record.Phase != "completed" {
		return scheduleStatus(record)
	}
	if record, ok := state.published[key]; ok {
		return "AI review published " + string(record.Decision), queueOther
	}
	rule, covered := pol.Resolve(request.Repository)
	if !covered || rule.Trigger.Kind != policy.TriggerReviewRequested {
		return "not covered by AI review", queueNotCovered
	}
	if _, ok := state.schedule[key]; ok {
		return "AI review completed", queueOther
	}
	return "AI review not started yet", queueOther
}

func scheduleStatus(record schedule.Record) (string, int) {
	switch record.Phase {
	case "running":
		return "AI review running", queueOther
	case "waiting", "retry_wait":
		return "AI review queued", queueOther
	case "failed":
		code := record.ErrorCode
		if code == "" {
			code = "unknown"
		}
		return "AI review stopped: " + code, queueStopped
	case "withdrawn":
		return "AI review withdrawn", queueOther
	default:
		return "AI review state unknown: " + record.Phase, queueOther
	}
}

// renderQueue builds the plain-text and HTML bodies with as many entries as
// fit in limit characters each. A message never cuts an entry in half, because
// a cut HTML tag would break formatted_body.
func renderQueue(entries []queueEntry, truncated bool, limit, titleLimit int) (string, string, int, bool) {
	build := func(count int) (string, string) {
		note := truncated || count < len(entries)
		var plain, formatted strings.Builder
		waiting := 0
		for _, entry := range entries[:count] {
			if entry.priority == queueWaitingAuthor {
				waiting++
			}
		}
		section := func(header string) {
			if plain.Len() > 0 {
				formatted.WriteString("</ul>\n")
			}
			plain.WriteString(header + "\n")
			formatted.WriteString("<p><strong>" + html.EscapeString(header) + "</strong></p>\n<ul>\n")
		}
		if count > waiting {
			section(fmt.Sprintf("Reviews waiting for you (%d)", count-waiting))
		}
		for index, entry := range entries[:count] {
			if entry.priority == queueWaitingAuthor && (index == 0 || entries[index-1].priority != queueWaitingAuthor) {
				section(fmt.Sprintf("Waiting on author (%d)", waiting))
			}
			title := cleanText(entry.request.Title, titleLimit)
			author := cleanText(entry.request.Author, titleLimit)
			if author == "" {
				author = "unknown"
			}
			reference := fmt.Sprintf("%s#%d", entry.request.Repository, entry.request.Number)
			plain.WriteString(fmt.Sprintf("- %s (%s, by %s): %s\n  %s\n", title, reference, author, entry.status, entry.request.URL))
			formatted.WriteString(fmt.Sprintf("<li><a href=\"%s\">%s</a> (%s, by %s): %s</li>\n",
				html.EscapeString(entry.request.URL), html.EscapeString(title), html.EscapeString(reference),
				html.EscapeString(author), html.EscapeString(entry.status)))
		}
		formatted.WriteString("</ul>")
		if note {
			const text = "The list is truncated: more requests wait for you than are shown."
			plain.WriteString(text + "\n")
			formatted.WriteString("\n<p><em>" + html.EscapeString(text) + "</em></p>")
		}
		return strings.TrimRight(plain.String(), "\n"), formatted.String()
	}
	shown, plain, formatted := 0, "", ""
	for count := 1; count <= len(entries); count++ {
		candidatePlain, candidateFormatted := build(count)
		if utf8.RuneCountInString(candidatePlain) > limit || utf8.RuneCountInString(candidateFormatted) > limit {
			break
		}
		shown, plain, formatted = count, candidatePlain, candidateFormatted
	}
	return plain, formatted, shown, truncated || shown < len(entries)
}

// cleanText turns GitHub-provided text into one line of at most max characters.
func cleanText(text string, max int) string {
	line := strings.Join(strings.FieldsFunc(text, func(r rune) bool { return unicode.IsSpace(r) || unicode.IsControl(r) }), " ")
	if runes := []rune(line); len(runes) > max {
		line = string(runes[:max-1]) + "…"
	}
	return line
}
