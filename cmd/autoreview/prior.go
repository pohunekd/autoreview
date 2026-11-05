package main

import (
	"fmt"

	"github.com/pohunekd/autoreview/policy"
)

// republishedPrefix numbers the prior findings a review publishes again, so
// they never collide with the findings of its reviewers.
const republishedPrefix = "P"

// SettlePrior applies what the reviewers' verdicts cannot decide alone:
//   - On a rule that approves without the user, an answered verdict closes a
//     prior finding only when it rests on the reviewing account's own comment
//     that is not AI-signed; a reply by the author alone would approve.
//   - An open prior finding without an unresolved thread is published again as
//     a new finding, so the author has a thread to answer.
func SettlePrior(input ReviewInput, discussion Discussion, artifact Artifact) Artifact {
	if len(artifact.PriorResults) == 0 {
		return artifact
	}
	results := make([]PriorResult, len(artifact.PriorResults))
	copy(results, artifact.PriorResults)
	findings := append([]Finding(nil), artifact.Findings...)
	republished := 0
	for index := range results {
		result := &results[index]
		if result.Status == priorAnswered && input.Rule.OnClean == policy.CleanApprove && !decidedByReviewer(discussion, *result, input.PublisherLogin) {
			result.Status = priorOpen
			result.Note = "Settled in the discussion, but a rule that approves without the user needs a comment of the reviewing account itself: " + result.Note
		}
		if result.Status != priorOpen || discussion.openThread(result.Finding.Key) {
			continue
		}
		prior := result.Finding
		republished++
		result.Republished = fmt.Sprintf("%s%03d", republishedPrefix, republished)
		findings = append(findings, Finding{
			ID: result.Republished, SourceID: result.Republished, ReviewerLabel: "earlier review",
			Severity: prior.Severity, Confidence: "high", Path: prior.Path, Line: prior.Line, Title: prior.Title,
			Body: fmt.Sprintf("Still open from %s of the review of `%.12s`, whose thread is resolved or missing.\n\n%s\n\n%s", prior.ID, prior.HeadSHA, result.Note, prior.Body),
		})
	}
	artifact.PriorResults = results
	artifact.Findings = findings
	return artifact
}

func decidedByReviewer(discussion Discussion, result PriorResult, publisher string) bool {
	if result.ReliesOn == "" || result.ReliesOn != publisher {
		return false
	}
	for _, comment := range discussion.commentsBy(result.Finding.Key, publisher) {
		if !comment.AISigned {
			return true
		}
	}
	return false
}
