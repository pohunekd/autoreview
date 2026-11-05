package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"sort"
	"strings"

	"github.com/pohunekd/autoreview/policy"
)

// AnchorFindings marks every finding on a changed RIGHT-side line as anchored.
// A finding elsewhere in a changed file moves to the closest changed line and
// says where it was reported. A finding in an unchanged file stays unanchored,
// is listed in the review body and never blocks: the reviewer prompt anchors a
// problem the change causes elsewhere to the changed line that causes it.
func AnchorFindings(artifact Artifact, commentable CommentableLines) Artifact {
	findings := make([]Finding, len(artifact.Findings))
	copy(findings, artifact.Findings)
	for index, finding := range findings {
		lines := commentable[finding.Path]
		if containsLine(lines, finding.Line) {
			findings[index].Anchored = true
			continue
		}
		line := nearestLine(lines, finding.Line)
		if line == 0 {
			findings[index].Anchored = false
			findings[index].NonBlocking = true
			continue
		}
		findings[index].Body = fmt.Sprintf("Reported at `%s:%d`, which is not a changed line of this pull request; anchored to the closest changed line.\n\n%s", finding.Path, finding.Line, finding.Body)
		findings[index].Line = line
		findings[index].Anchored = true
	}
	artifact.Findings = findings
	return artifact
}

func containsLine(lines []int, target int) bool {
	for _, line := range lines {
		if line == target {
			return true
		}
	}
	return false
}

// nearestLine returns the line closest to target, the lower one on a tie, or
// 0 when there is none.
func nearestLine(lines []int, target int) int {
	best := 0
	distance := func(line int) int {
		if line > target {
			return line - target
		}
		return target - line
	}
	for _, line := range lines {
		if line > 0 && (best == 0 || distance(line) < distance(best) || distance(line) == distance(best) && line < best) {
			best = line
		}
	}
	return best
}

// PublicationPlan is what the workflow does with a finished review.
type PublicationPlan struct {
	Decision Decision `json:"decision"`
	// Await holds an approval until the user confirms it.
	Await bool `json:"await"`
	// OwnPullRequest means the reviewing account authored the pull request, so
	// GitHub accepts only a comment.
	OwnPullRequest bool `json:"own_pull_request"`
	// NotesToAuthor hands the non-blocking findings to the author in a comment;
	// the author's renewed review request asks the user for approval.
	NotesToAuthor bool `json:"notes_to_author,omitempty"`
}

// blockingCount counts the new blocking findings and the prior findings the
// review left open.
func blockingCount(input ReviewInput, artifact Artifact) int {
	blocking, _ := input.splitFindings(artifact.Findings)
	return len(blocking) + len(artifact.openPrior())
}

// PlanPublication decides by the blocking findings only: a review whose
// findings are all notes, and that leaves no prior finding open, is clean and
// follows the rule's clean outcome. A clean review that the rule would approve
// without the user waits for them when the verifier demoted findings the
// policy wants them to see, or when the pull request matched a human review
// signal of the rule. A rule with on_notes comment hands a review of
// notes only to a human author first, unless the author already had them.
func PlanPublication(input ReviewInput, artifact Artifact, login string) PublicationPlan {
	plan := PublicationPlan{OwnPullRequest: input.PullRequest.Author == login}
	blocking := blockingCount(input, artifact)
	_, notes := input.splitFindings(artifact.Findings)
	switch {
	case blocking > 0 && input.Rule.OnFindings == policy.FindingsRequestChanges:
		plan.Decision = DecisionRequestChanges
	case blocking > 0:
		plan.Decision = DecisionComment
	case input.Rule.OnClean == policy.CleanApprove:
		plan.Decision, plan.Await = DecisionApprove, escalates(input, artifact) || len(artifact.HumanReview) > 0
	case input.Rule.OnNotes == policy.NotesComment && len(notes) > 0 && input.Handoff == nil && !plan.OwnPullRequest && !input.PullRequest.AuthorBot:
		plan.Decision, plan.NotesToAuthor = DecisionComment, true
	case input.Rule.OnClean == policy.CleanAwait:
		plan.Decision, plan.Await = DecisionApprove, true
	default:
		plan.Decision = DecisionComment
	}
	if plan.OwnPullRequest && plan.Decision != DecisionComment {
		plan.Decision, plan.Await = DecisionComment, false
	}
	return plan
}

// PublicationEvent names the notification of a published review; blocking is
// the number of its blocking findings.
func PublicationEvent(plan PublicationPlan, blocking int) policy.Event {
	switch {
	case plan.Decision == DecisionApprove:
		return policy.EventApproved
	case plan.Decision == DecisionRequestChanges:
		return policy.EventChangesRequested
	case plan.OwnPullRequest && blocking > 0:
		return policy.EventFindingsOnOwnPR
	}
	return policy.EventCommented
}

var commentDelimiters = strings.NewReplacer("<!--", "&lt;!--", "-->", "--&gt;")

// neutralize keeps model-written Markdown, code included, but outside code
// stops it from mentioning users, embedding HTML, or loading images, which
// GitHub fetches on render and could carry private content to a foreign host.
// HTML comment delimiters are defused everywhere, so model output can neither
// hide content nor imitate the review marker. Fences follow CommonMark, so a
// line GitHub renders as text is never mistaken for code.
func neutralize(value string) string {
	lines := strings.Split(value, "\n")
	fence := ""
	for index, line := range lines {
		indent := len(line) - len(strings.TrimLeft(line, " "))
		trimmed := line[indent:]
		if fence != "" {
			if indent <= 3 && closesFence(trimmed, fence) {
				fence = ""
			}
			continue
		}
		if marker := openingFence(trimmed); indent <= 3 && marker != "" {
			fence = marker
			continue
		}
		lines[index] = neutralizeInline(line)
	}
	return commentDelimiters.Replace(strings.Join(lines, "\n"))
}

// openingFence returns the run of at least three backticks or tildes that
// opens a fenced block; a backtick fence's info string has no backtick.
func openingFence(line string) string {
	for _, character := range []byte{'`', '~'} {
		run := 0
		for run < len(line) && line[run] == character {
			run++
		}
		if run >= 3 {
			if character == '`' && strings.Contains(line[run:], "`") {
				return ""
			}
			return line[:run]
		}
	}
	return ""
}

// closesFence reports a line of at least as many fence characters and nothing else.
func closesFence(line, fence string) bool {
	run := 0
	for run < len(line) && line[run] == fence[0] {
		run++
	}
	return run >= len(fence) && strings.TrimSpace(line[run:]) == ""
}

// neutralizeInline defuses mentions, HTML and images outside the inline code
// spans of a line.
func neutralizeInline(line string) string {
	var out strings.Builder
	for index := 0; index < len(line); {
		switch {
		case line[index] == '`':
			run := 1
			for index+run < len(line) && line[index+run] == '`' {
				run++
			}
			delimiter := line[index : index+run]
			if closing := strings.Index(line[index+run:], delimiter); closing >= 0 {
				end := index + run + closing + run
				out.WriteString(line[index:end])
				index = end
				continue
			}
			out.WriteString(delimiter)
			index += run
			continue
		case line[index] == '@' && index+1 < len(line) && isMentionStart(line[index+1]) && (index == 0 || !isWordByte(line[index-1])):
			out.WriteString("@\u200b")
		case line[index] == '<':
			out.WriteString("&lt;")
		case line[index] == '!' && index+1 < len(line) && line[index+1] == '[':
			out.WriteString("!\\")
		default:
			out.WriteByte(line[index])
		}
		index++
	}
	return out.String()
}

func isMentionStart(character byte) bool {
	return character >= 'a' && character <= 'z' || character >= 'A' && character <= 'Z' || character >= '0' && character <= '9'
}

func isWordByte(character byte) bool {
	return isMentionStart(character) || character == '_' || character == '.' || character == '-'
}

// reviewMarker identifies one publication. It carries the decision and the
// number of blocking findings, so a reader such as a merge gate tells a clean
// review from one with blocking findings without parsing the body, also when
// GitHub forces the decision to COMMENT on the reviewer's own pull request.
// Notes are not counted: they never hold the merge back. A prior finding the
// review left open is counted under its key.
func reviewMarker(pr PullRequest, decision Decision, findings []Finding, open []PriorResult) string {
	ids := make([]string, 0, len(findings)+len(open))
	for _, finding := range findings {
		ids = append(ids, finding.ID)
	}
	for _, result := range open {
		ids = append(ids, result.Finding.Key)
	}
	sort.Strings(ids)
	digest := sha256.Sum256(fmt.Appendf(nil, "%s\n%d\n%s\n%s\n%s", pr.Repository, pr.Number, pr.HeadSHA, decision, strings.Join(ids, ",")))
	return fmt.Sprintf("<!-- hermes-pr-review:v1:%s#%d@%s:decision=%s:findings=%d:digest=%s -->", pr.Repository, pr.Number, pr.HeadSHA, decision, len(ids), hex.EncodeToString(digest[:]))
}

func reviewerLabels(reviewers []policy.Reviewer) string {
	labels := make([]string, 0, len(reviewers))
	for _, reviewer := range reviewers {
		labels = append(labels, reviewer.Label)
	}
	return strings.Join(labels, " + ")
}

// RenderReview builds the GitHub review: anchored findings become inline
// comments, everything else is in the body. The AI signature is the last line
// of the body and of each comment, where agents look for it; every block of
// model text has its fences closed, so none can swallow the signature.
func RenderReview(cfg Config, input ReviewInput, artifact Artifact, plan PublicationPlan) (GitHubReview, error) {
	pr := input.PullRequest
	blocking, notes := input.splitFindings(artifact.Findings)
	open := artifact.openPrior()
	marker := reviewMarker(pr, plan.Decision, blocking, open)
	sections := []string{marker, "## Automated code review", fmt.Sprintf("Reviewed by %s, %s review of `%.12s`.", reviewerLabels(input.Reviewers), artifact.Scope.Mode, pr.HeadSHA)}
	if artifact.Scope.Incremental() {
		sections = append(sections, fmt.Sprintf("This re-review covers the changes since the previously reviewed commit `%.12s`.", artifact.Scope.PreviousHeadSHA))
	}
	for _, summary := range artifact.Summaries {
		sections = append(sections, fmt.Sprintf("**%s:** %s", neutralize(summary.Label), modelText(summary.Summary)))
	}
	if line := verificationLine(artifact.Verification); line != "" {
		sections = append(sections, neutralize(line))
	}
	if len(artifact.HumanReview) > 0 {
		sections = append(sections, neutralize("This review waited for a human approval because the pull request matched: "+humanReviewText(artifact.HumanReview, cfg.ExcerptCharacters)+"."))
	}
	comments := []InlineComment{}
	unanchored := []string{}
	for _, finding := range blocking {
		heading := findingHeading(finding)
		if finding.Anchored {
			comments = append(comments, InlineComment{Path: finding.Path, Line: finding.Line, Side: "RIGHT", Body: heading + "\n\n" + modelText(finding.Body) + "\n\n" + cfg.AISignature})
			continue
		}
		unanchored = append(unanchored, fmt.Sprintf("%s\n`%s:%d`\n\n%s", heading, neutralize(finding.Path), finding.Line, modelText(finding.Body)))
	}
	switch {
	case len(blocking) == 0 && len(open) > 0:
		sections = append(sections, "No new blocking findings were identified.")
	case len(blocking) == 0 && plan.NotesToAuthor:
		sections = append(sections, "No blocking findings were identified, only the non-blocking notes below. Whether to address them is up to the author. Request this review again once you have decided; the pull request then goes to the reviewer for approval.")
	case len(blocking) == 0:
		sections = append(sections, "No blocking findings were identified.")
	case len(unanchored) == 0:
		sections = append(sections, fmt.Sprintf("%d blocking finding(s) are posted as inline comments.", len(comments)))
	default:
		sections = append(sections, fmt.Sprintf("%d blocking finding(s) are posted as inline comments. These findings concern code outside the changed lines:", len(comments)))
		sections = append(sections, unanchored...)
	}
	if len(artifact.PriorResults) > 0 {
		sections = append(sections, renderPrior(artifact.PriorResults))
	}
	if len(notes) > 0 {
		sections = append(sections, renderNotes(notes))
	}
	if len(artifact.Tests) > 0 {
		lines := make([]string, 0, len(artifact.Tests))
		for _, test := range artifact.Tests {
			lines = append(lines, "- "+neutralize(test))
		}
		sections = append(sections, "### Validation performed\n"+strings.Join(lines, "\n"))
	}
	total := blockingCount(input, artifact)
	if plan.OwnPullRequest && (total > 0 && input.Rule.OnFindings == policy.FindingsRequestChanges || total == 0 && input.Rule.ApprovesClean()) {
		sections = append(sections, "GitHub does not let the reviewing account approve or request changes on its own pull request, so this review is a comment.")
	}
	sections = append(sections, cfg.AISignature)
	body := strings.Join(sections, "\n\n")
	// GitHub limits each text field on its own, not the whole submission.
	if len(body) > cfg.GitHubBodyMaxBytes {
		return GitHubReview{}, workflowError("github_review_too_large", true, nil)
	}
	for _, comment := range comments {
		if len(comment.Body) > cfg.GitHubBodyMaxBytes {
			return GitHubReview{}, workflowError("github_review_too_large", true, nil)
		}
	}
	return GitHubReview{Decision: plan.Decision, Body: body, Comments: comments, Marker: marker}, nil
}

func findingHeading(finding Finding) string {
	return fmt.Sprintf("**%s %s — %s** (confidence: %s, %s)", finding.ID, strings.ToUpper(finding.Severity), neutralize(finding.Title), finding.Confidence, neutralize(finding.ReviewerLabel))
}

// renderNotes lists the non-blocking findings in a collapsed section of the
// body. They get no inline comment, so they open no thread to resolve, and the
// summary line says outright that the pull request needs no change for them.
func renderNotes(notes []Finding) string {
	items := make([]string, 0, len(notes))
	for _, note := range notes {
		items = append(items, fmt.Sprintf("%s\n`%s:%d`\n\n%s", findingHeading(note), neutralize(note.Path), note.Line, modelText(note.Body)))
	}
	return fmt.Sprintf("<details>\n<summary>%d non-blocking note(s): no action required in this pull request</summary>\n\n%s\n\n</details>", len(notes), strings.Join(items, "\n\n"))
}

// renderPrior reports the verdict on every prior finding. An open one keeps
// its thread from the earlier review, so it gets no new inline comment.
func renderPrior(results []PriorResult) string {
	lines := []string{"### Findings from earlier reviews"}
	for _, result := range results {
		finding := result.Finding
		status := map[string]string{priorOpen: "**Still open, blocking**", priorFixed: "Fixed", priorAnswered: "Settled in the discussion"}[result.Status]
		if result.Reopened {
			status += ", reopened by the verification"
		}
		if result.Republished != "" {
			status += ", published again as " + result.Republished
		}
		if result.ReliesOn != "" {
			status += ", relying on a comment by " + neutralize(result.ReliesOn)
		}
		lines = append(lines, fmt.Sprintf("- %s: %s %s — %s `%s:%d` (review of `%.12s`)\n\n%s", status, finding.ID, strings.ToUpper(finding.Severity), neutralize(finding.Title), neutralize(finding.Path), finding.Line, finding.HeadSHA, modelText(result.Note)))
	}
	return strings.Join(lines, "\n\n")
}

// modelText renders multi-line model text as a self-contained block.
func modelText(value string) string { return closeOpenFence(neutralize(value)) }

// closeOpenFence appends the closing fence of a fenced block that model text
// leaves open, so the block cannot swallow the markup that follows it.
func closeOpenFence(value string) string {
	fence := ""
	for _, line := range strings.Split(value, "\n") {
		indent := len(line) - len(strings.TrimLeft(line, " "))
		if indent > 3 {
			continue
		}
		trimmed := line[indent:]
		if fence != "" {
			if closesFence(trimmed, fence) {
				fence = ""
			}
			continue
		}
		fence = openingFence(trimmed)
	}
	if fence == "" {
		return value
	}
	return value + "\n" + fence
}

// HumanReviewSignals lists the signals of a human_review that the pull request
// matches: each configured label it carries, and the deleted files that match
// a test glob.
func HumanReviewSignals(signals policy.HumanReview, labels, deleted []string) []string {
	matched := []string{}
	for _, label := range signals.Labels {
		for _, carried := range labels {
			if strings.EqualFold(carried, label) {
				matched = append(matched, fmt.Sprintf("label %s", label))
				break
			}
		}
	}
	tests := []string{}
	for _, file := range deleted {
		for _, glob := range signals.DeletedTests {
			if policy.MatchGlob(glob, file) {
				tests = append(tests, file)
				break
			}
		}
	}
	if len(tests) > 0 {
		sort.Strings(tests)
		matched = append(matched, fmt.Sprintf("deleted test files %s", strings.Join(tests, ", ")))
	}
	return matched
}

// humanReviewText joins the matched signals, each cut to limit characters.
func humanReviewText(signals []string, limit int) string {
	parts := make([]string, 0, len(signals))
	for _, signal := range signals {
		parts = append(parts, excerpt(signal, limit))
	}
	return strings.Join(parts, "; ")
}
