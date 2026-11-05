package main

import (
	"fmt"
	"strings"
	"time"
)

// The approval title and the Review ID line identify an approval request: an
// approval is accepted only as a reply to a message carrying both.
const approvalRequestedTitle = "GitHub review completed with no blocking findings"

func excerpt(value string, limit int) string {
	normalized := strings.Join(strings.Fields(value), " ")
	if runes := []rune(normalized); len(runes) > limit {
		return strings.TrimSpace(string(runes[:limit-1])) + "…"
	}
	return normalized
}

func reviewHeader(title string, input ReviewInput) string {
	pr := input.PullRequest
	return fmt.Sprintf("%s\n\nReview ID: %s\nPR: %s\nTitle: %s\nReviewers: %s\nScope: %s", title, pr.ReviewID(), pr.URL, excerpt(pr.Title, 200), reviewerLabels(input.Reviewers), input.Scope.Mode)
}

func startedMessage(input ReviewInput) string {
	return reviewHeader("GitHub review started", input) + fmt.Sprintf("\nAttempt: %d", input.Attempt+1)
}

func summaryLines(artifact Artifact, limit int) string {
	lines := make([]string, 0, len(artifact.Summaries))
	for _, summary := range artifact.Summaries {
		lines = append(lines, fmt.Sprintf("%s: %s", summary.Label, excerpt(summary.Summary, limit)))
	}
	return strings.Join(lines, "\n")
}

func findingLines(input ReviewInput, findings []Finding, limit int) string {
	lines := make([]string, 0, len(findings))
	for _, finding := range findings {
		kind := strings.ToUpper(finding.Severity)
		switch {
		case finding.Verdict == verdictRefuted:
			kind = "NOTE, REFUTED BY VERIFICATION, " + kind
		case finding.Verdict == verdictNotBlocking:
			kind = "NOTE, NOT BLOCKING BY VERIFICATION, " + kind
		case !input.blocks(finding):
			kind = "NOTE, " + kind
		}
		lines = append(lines, fmt.Sprintf("%s [%s, confidence: %s] %s:%d %s", finding.ID, kind, finding.Confidence, finding.Path, finding.Line, excerpt(finding.Title, limit)))
	}
	return strings.Join(lines, "\n")
}

func approvalRequestedMessage(cfg Config, input ReviewInput, artifact Artifact) string {
	_, notes := input.splitFindings(artifact.Findings)
	// The reply instruction precedes the notes, so a long list cut at the
	// Matrix message limit never drops it.
	message := fmt.Sprintf("%s\nBlocking findings: 0\nNon-blocking notes: %d\n\nReply to this message with: approve\nNothing has been posted to GitHub. The approval expires after %s.\n\nSummary:\n%s",
		reviewHeader(approvalRequestedTitle, input), len(notes), approvalWindow(input), summaryLines(artifact, cfg.ExcerptCharacters))
	if input.Handoff != nil {
		message += fmt.Sprintf("\nThe author requested the review again after the notes were handed to them at `%.12s`.", input.Handoff.HeadSHA)
	}
	if line := verificationLine(artifact.Verification); line != "" {
		message += "\n" + line
	}
	if len(artifact.HumanReview) > 0 {
		message += "\nThis rule approves clean reviews without you, but the pull request matched its human review signals: " + humanReviewText(artifact.HumanReview, cfg.ExcerptCharacters) + "."
	}
	if escalates(input, artifact) {
		demoted := []Finding{}
		for _, finding := range notes {
			if finding.demoted() {
				demoted = append(demoted, finding)
			}
		}
		// Listed apart from the other notes, so the cut at the message limit
		// keeps them.
		message += "\nThis rule approves clean reviews without you, but the verification demoted these blocking findings, check before approving:\n" + findingLines(input, demoted, cfg.ExcerptCharacters)
	}
	if answered := answeredLines(artifact, cfg.ExcerptCharacters); answered != "" {
		// The reviewer closed these without a code change; the approval is the
		// only check of that judgement.
		message += "\nEarlier findings settled in the discussion, check before approving:\n" + answered
	}
	if len(notes) > 0 {
		message += "\nNotes:\n" + findingLines(input, notes, cfg.ExcerptCharacters)
	}
	return message
}

func answeredLines(artifact Artifact, limit int) string {
	lines := []string{}
	for _, result := range artifact.PriorResults {
		if result.Status != priorAnswered {
			continue
		}
		basis := "the code"
		if result.ReliesOn != "" {
			basis = "a comment by " + result.ReliesOn
		}
		lines = append(lines, fmt.Sprintf("%s %s (relies on %s): %s", result.Finding.Key, excerpt(result.Finding.Title, limit), basis, excerpt(result.Note, limit)))
	}
	return strings.Join(lines, "\n")
}

func publishedMessage(cfg Config, input ReviewInput, artifact Artifact, plan PublicationPlan, result PublishResult) string {
	pr := input.PullRequest
	_, notes := input.splitFindings(artifact.Findings)
	blocking := blockingCount(input, artifact)
	var title string
	switch {
	case plan.Decision == DecisionApprove:
		title = fmt.Sprintf("Approved %s PR #%d after a review without blocking findings", pr.Repository, pr.Number)
	case plan.Decision == DecisionRequestChanges:
		title = fmt.Sprintf("Requested changes on %s PR #%d with %d blocking finding(s)", pr.Repository, pr.Number, blocking)
	case plan.NotesToAuthor:
		title = fmt.Sprintf("Handed %d non-blocking note(s) on %s PR #%d to the author; their next review request asks for your approval", len(notes), pr.Repository, pr.Number)
	default:
		title = fmt.Sprintf("Commented on %s PR #%d with %d blocking finding(s)", pr.Repository, pr.Number, blocking)
	}
	if len(notes) > 0 && !plan.NotesToAuthor {
		title += fmt.Sprintf(" and %d non-blocking note(s)", len(notes))
	}
	message := fmt.Sprintf("%s\n\nReview ID: %s\nReview: %s\nReviewers: %s\nScope: %s", title, pr.ReviewID(), result.ReviewURL, reviewerLabels(input.Reviewers), artifact.Scope.Mode)
	if line := verificationLine(artifact.Verification); line != "" {
		message += "\n" + line
	}
	if open := artifact.openPrior(); len(open) > 0 {
		lines := make([]string, 0, len(open))
		for _, result := range open {
			lines = append(lines, fmt.Sprintf("%s [STILL OPEN] %s:%d %s", result.Finding.Key, result.Finding.Path, result.Finding.Line, excerpt(result.Finding.Title, cfg.ExcerptCharacters)))
		}
		message += "\n\n" + strings.Join(lines, "\n")
	}
	if len(artifact.Findings) > 0 {
		message += "\n\n" + findingLines(input, artifact.Findings, cfg.ExcerptCharacters)
	} else {
		message += "\nSummary:\n" + summaryLines(artifact, cfg.ExcerptCharacters)
	}
	return message
}

// approvalWindow states the approval timeout in the hours the policy uses.
func approvalWindow(input ReviewInput) string {
	if input.ApprovalTimeoutSeconds%3600 == 0 {
		return fmt.Sprintf("%d hours", input.ApprovalTimeoutSeconds/3600)
	}
	return (time.Duration(input.ApprovalTimeoutSeconds) * time.Second).String()
}

func supersededMessage(input ReviewInput, code string) string {
	pr := input.PullRequest
	return fmt.Sprintf("Review %s was superseded (%s). Nothing was posted to GitHub.", pr.ReviewID(), code)
}

func approvalExpiredMessage(input ReviewInput) string {
	pr := input.PullRequest
	return fmt.Sprintf("Approval for review %s expired after %s without a reply. Nothing was posted to GitHub. A new commit or a renewed review request starts a new review.", pr.ReviewID(), approvalWindow(input))
}

func approvedReviewFailedMessage(input ReviewInput, code string) string {
	pr := input.PullRequest
	return fmt.Sprintf("Review for %s at %.12s stopped after your approval: it was not published (%s). Nothing was posted to GitHub. Retry this PR to review and publish it again.", pr.Key(), pr.HeadSHA, code)
}

func orphanedSessionMessage(request SessionRequest, code string) string {
	return fmt.Sprintf("Pohunek may have started reviewer session %s on %s for review %s after the start request failed, and it could not be found or removed (%s). Check `pohunek --host %s session list --json` and remove the session if it exists.", request.Name(), request.Route.Host, request.PullRequest.ReviewID(), code, request.Route.Host)
}

type sessionCleanupFailure struct {
	Session SessionIdentity
	Code    string
}

// sessionCleanupFailedMessage names only identifiers and allowlisted codes;
// raw CLI output may contain prompts or credentials.
func sessionCleanupFailedMessage(pr PullRequest, failures []sessionCleanupFailure, attempts int) string {
	details := make([]string, 0, len(failures))
	for _, failure := range failures {
		details = append(details, fmt.Sprintf("%s session %s on %s (%s)", failure.Session.ReviewerID, failure.Session.SessionID, failure.Session.Host, failure.Code))
	}
	return fmt.Sprintf("Pohunek could not remove reviewer sessions of review %s after %d attempts: %s. The review continues and the sessions stay on the host until removed. Run `pohunek --host <host> session rm <session> --json` to list the processes that block removal; when none belongs to the session, add --accept-unconfirmed-cleanup.", pr.ReviewID(), attempts, strings.Join(details, "; "))
}

// handoffAge states how long notes have waited for their author, in hours.
func handoffAge(record PublishedRecord, now time.Time) string {
	return fmt.Sprintf("%d h", int(now.Sub(record.PublishedAt).Hours()))
}

// handoffReminderMessage carries no Review ID: the review waits for its
// author, so nothing can be approved by replying to it.
func handoffReminderMessage(cfg Config, record PublishedRecord, current PullRequestState, now time.Time) string {
	return fmt.Sprintf("Non-blocking notes on %s PR #%d wait for the author for %s\n\n%s (by %s)\n%s\n\nThe author has not requested the review again. Remind them, or review the pull request on GitHub yourself.",
		record.Repository, record.Number, handoffAge(record, now), excerpt(current.Title, cfg.ExcerptCharacters), current.Author, current.URL)
}
