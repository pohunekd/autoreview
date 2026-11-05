// Package policy loads the review policy: which repositories are reviewed,
// by which reviewers, and how the outcome is published and announced.
package policy

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"go.yaml.in/yaml/v3"
)

// Protocol names the input and output contract of a reviewer agent profile.
type Protocol string

const (
	// ProtocolClaudeStructured sends a JSON request with an enforced schema and
	// reads the claude-otel-noninteractive structured result wrapper.
	ProtocolClaudeStructured Protocol = "claude-structured"
	// ProtocolCodexEnvelope sends a plain prompt and reads the base64 result
	// envelope printed by the pohunek Codex review wrapper.
	ProtocolCodexEnvelope Protocol = "codex-envelope"
)

// Checkout selects the commit the reviewer worktree starts from.
type Checkout string

const (
	// CheckoutBase starts at the base branch tip, so repository instructions
	// are read from trusted content before the reviewer fetches the head.
	CheckoutBase Checkout = "base"
	// CheckoutHead starts at refs/pull/<number>/head.
	CheckoutHead Checkout = "head"
)

type TriggerKind string

const (
	TriggerReviewRequested TriggerKind = "review-requested"
	TriggerLabel           TriggerKind = "label"
)

type Trigger struct {
	Kind  TriggerKind `json:"kind"`
	Label string      `json:"label,omitempty"`
}

func (trigger Trigger) String() string {
	if trigger.Kind == TriggerLabel {
		return "label:" + trigger.Label
	}
	return string(trigger.Kind)
}

type OnFindings string

const (
	FindingsRequestChanges OnFindings = "request_changes"
	FindingsComment        OnFindings = "comment"
)

type OnClean string

const (
	CleanApprove OnClean = "approve"
	CleanAwait   OnClean = "await"
	CleanComment OnClean = "comment"
)

// OnNotes decides a review whose findings are all non-blocking. Empty follows
// the rule's clean outcome.
type OnNotes string

// NotesComment hands the notes to the author in a comment first; once the
// author requests the review again, the clean outcome follows.
const NotesComment OnNotes = "comment"

// Level is the Matrix room a notification goes to. Only action messages
// mention the user.
type Level string

const (
	LevelAction Level = "action"
	LevelDigest Level = "digest"
	LevelLog    Level = "log"
)

type Event string

const (
	EventApprovalRequested Event = "approval_requested"
	EventFindingsOnOwnPR   Event = "findings_on_own_pr"
	EventReviewStopped     Event = "review_stopped"
	EventCleanupManual     Event = "cleanup_manual"
	EventCommandFailed     Event = "command_failed"
	EventApproved          Event = "approved"
	EventChangesRequested  Event = "changes_requested"
	EventCommented         Event = "commented"
	EventStarted           Event = "started"
	EventRetryScheduled    Event = "retry_scheduled"
	EventSuperseded        Event = "superseded"
	EventApprovalExpired   Event = "approval_expired"
	EventHandoffReminder   Event = "handoff_reminder"
)

// Events lists every notification event; the policy must assign each a level.
var Events = []Event{
	EventApprovalRequested, EventFindingsOnOwnPR, EventReviewStopped, EventCleanupManual, EventCommandFailed,
	EventApproved, EventChangesRequested, EventCommented, EventStarted, EventRetryScheduled, EventSuperseded,
	EventApprovalExpired, EventHandoffReminder,
}

// WorkflowEvents are the events a review workflow sends itself, so its input
// carries their levels. Discovery sends the others with the current policy.
var WorkflowEvents = []Event{
	EventApprovalRequested, EventFindingsOnOwnPR, EventReviewStopped, EventCleanupManual, EventCommandFailed,
	EventApproved, EventChangesRequested, EventCommented, EventStarted, EventRetryScheduled, EventSuperseded,
	EventApprovalExpired,
}

type Reviewer struct {
	ID            string   `json:"id"`
	Label         string   `json:"label"`
	Protocol      Protocol `json:"protocol"`
	AgentProfile  string   `json:"agent_profile"`
	FindingPrefix string   `json:"finding_prefix"`
}

type Pipeline struct {
	Name                 string     `json:"name"`
	Protocol             Protocol   `json:"protocol"`
	Reviewers            []Reviewer `json:"reviewers"`
	IncrementalReviewers []Reviewer `json:"incremental_reviewers"`
	Prompt               string     `json:"prompt"`
	Checkout             Checkout   `json:"checkout"`
	ProjectSources       []string   `json:"project_sources"`
	// Verifier is absent for a pipeline that publishes its findings unchecked.
	Verifier *Verifier `json:"verifier,omitempty"`
}

// VerifierReviewerID names the verifier's session and state. A reviewer of the
// policy cannot take it, so the verifier never shares a session with one.
const VerifierReviewerID = "verifier"

// Verifier checks every blocking finding of a review before it is published.
type Verifier struct {
	Reviewer Reviewer `json:"reviewer"`
	Prompt   string   `json:"prompt"`
	// On a rule that approves clean reviews without the user, the user decides
	// instead once the verifier demoted a blocking finding of one of these
	// severities, or this many blocking findings.
	EscalateSeverities []string `json:"escalate_severities"`
	EscalateCount      int      `json:"escalate_count"`
}

// Rule is the resolved policy for one repository. It is copied into each
// review workflow input, so a policy change affects only reviews dispatched
// after it.
type Rule struct {
	Match      string     `json:"match"`
	Trigger    Trigger    `json:"trigger"`
	Pipeline   Pipeline   `json:"pipeline"`
	OnFindings OnFindings `json:"on_findings"`
	OnClean    OnClean    `json:"on_clean"`
	OnNotes    OnNotes    `json:"on_notes,omitempty"`
	// HumanReview is absent on a rule whose clean reviews need no signal
	// check before an automatic approval.
	HumanReview *HumanReview `json:"human_review,omitempty"`
}

// ApprovesClean reports whether a clean review of this rule can end in an
// approval, which decides whether a later review may be incremental.
func (rule Rule) ApprovesClean() bool {
	return rule.OnClean == CleanApprove || rule.OnClean == CleanAwait
}

type Policy struct {
	rules             []Rule
	readyDraftAuthors []string
	notifications     map[Event]Level
	approvalTimeout   time.Duration
	// handoffReminder is how long notes handed to an author wait before the
	// user is reminded; zero when no rule hands notes off.
	handoffReminder time.Duration
	digest          string
}

// Limits bound the policy file and its prompts.
const (
	maxPolicyBytes    = 256 << 10
	maxPromptBytes    = 64 << 10
	maxRules          = 200
	maxApprovalHours  = 720
	maxReviewersPerPR = 4
)

var (
	identifierPattern = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,31}$`)
	profilePattern    = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`)
	prefixPattern     = regexp.MustCompile(`^[A-Z]$`)
	ownerPattern      = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,38}$`)
	repoNamePattern   = regexp.MustCompile(`^[a-z0-9_.-]{1,100}$`)
	labelPattern      = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9:._-]{0,49}$`)
	sourcePattern     = regexp.MustCompile(`^[a-z][a-z0-9_-]{0,31}$`)
)

type fileReviewer struct {
	Protocol      Protocol `yaml:"protocol"`
	AgentProfile  string   `yaml:"agent_profile"`
	Label         string   `yaml:"label"`
	FindingPrefix string   `yaml:"finding_prefix"`
}

type filePipeline struct {
	Reviewers            []string      `yaml:"reviewers"`
	IncrementalReviewers []string      `yaml:"incremental_reviewers"`
	Prompt               string        `yaml:"prompt"`
	Checkout             Checkout      `yaml:"checkout"`
	ProjectSources       []string      `yaml:"project_sources"`
	Verifier             *fileVerifier `yaml:"verifier"`
}

type fileVerifier struct {
	Reviewer           string   `yaml:"reviewer"`
	Prompt             string   `yaml:"prompt"`
	EscalateSeverities []string `yaml:"escalate_severities"`
	EscalateCount      int      `yaml:"escalate_count"`
}

// blockingSeverities are the severities a blocking finding can have, so the
// only ones a verifier can demote.
var blockingSeverities = map[string]bool{"critical": true, "high": true}

type fileRule struct {
	Match       string           `yaml:"match"`
	Trigger     string           `yaml:"trigger"`
	Pipeline    string           `yaml:"pipeline"`
	OnFindings  OnFindings       `yaml:"on_findings"`
	OnClean     OnClean          `yaml:"on_clean"`
	OnNotes     OnNotes          `yaml:"on_notes"`
	HumanReview *fileHumanReview `yaml:"human_review"`
}

type fileApproval struct {
	TimeoutHours int `yaml:"timeout_hours"`
}

type fileNotesHandoff struct {
	ReminderHours int `yaml:"reminder_hours"`
}

type file struct {
	Version       int                     `yaml:"version"`
	Reviewers     map[string]fileReviewer `yaml:"reviewers"`
	Pipelines     map[string]filePipeline `yaml:"pipelines"`
	Repositories  []fileRule              `yaml:"repositories"`
	Unmatched     string                  `yaml:"unmatched"`
	Approval      fileApproval            `yaml:"approval"`
	NotesHandoff  *fileNotesHandoff       `yaml:"notes_handoff"`
	Notifications map[Event]Level         `yaml:"notifications"`
	// ReadyDraftAuthors are coding agents that open pull requests as drafts
	// and never mark them ready for review.
	ReadyDraftAuthors []string `yaml:"ready_draft_authors"`
}

// Load reads the policy file and the prompt files it references. Prompt paths
// are relative to the policy file and must stay inside its directory.
func Load(policyPath string) (*Policy, error) {
	data, err := readBounded(policyPath, maxPolicyBytes)
	if err != nil {
		return nil, fmt.Errorf("review policy %s: %w", policyPath, err)
	}
	directory := filepath.Dir(policyPath)
	return Parse(data, func(relative string) ([]byte, error) {
		return readBounded(filepath.Join(directory, filepath.FromSlash(relative)), maxPromptBytes)
	})
}

func readBounded(name string, limit int64) ([]byte, error) {
	handle, err := os.Open(name)
	if err != nil {
		return nil, err
	}
	defer handle.Close()
	data, err := io.ReadAll(io.LimitReader(handle, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("file exceeds %d bytes", limit)
	}
	return data, nil
}

// Parse validates a policy document. readPrompt returns the content of a
// prompt path as written in the document.
func Parse(data []byte, readPrompt func(string) ([]byte, error)) (*Policy, error) {
	decoder := yaml.NewDecoder(bytes.NewReader(data))
	decoder.KnownFields(true)
	var document file
	if err := decoder.Decode(&document); err != nil {
		return nil, fmt.Errorf("review policy is not valid YAML: %w", err)
	}
	var extra any
	if err := decoder.Decode(&extra); !errors.Is(err, io.EOF) {
		return nil, errors.New("review policy must contain exactly one YAML document")
	}
	if document.Version != 1 {
		return nil, errors.New("review policy version must be 1")
	}
	if document.Unmatched != "skip" {
		return nil, errors.New("review policy unmatched must be skip")
	}
	if document.Approval.TimeoutHours < 1 || document.Approval.TimeoutHours > maxApprovalHours {
		return nil, fmt.Errorf("review policy approval.timeout_hours must be between 1 and %d", maxApprovalHours)
	}

	reviewers, err := parseReviewers(document.Reviewers)
	if err != nil {
		return nil, err
	}
	pipelines, err := parsePipelines(document.Pipelines, reviewers, readPrompt)
	if err != nil {
		return nil, err
	}
	rules, err := parseRules(document.Repositories, pipelines)
	if err != nil {
		return nil, err
	}
	notifications, err := parseNotifications(document.Notifications)
	if err != nil {
		return nil, err
	}
	reminderHours, err := parseNotesHandoff(document.NotesHandoff, rules)
	if err != nil {
		return nil, err
	}
	authors := []string{}
	for _, author := range document.ReadyDraftAuthors {
		if !ownerPattern.MatchString(author) || slices.Contains(authors, author) {
			return nil, fmt.Errorf("ready_draft_authors: %q must be a lowercase login listed once", author)
		}
		authors = append(authors, author)
	}
	sort.Strings(authors)
	return &Policy{
		rules:             rules,
		readyDraftAuthors: authors,
		notifications:     notifications,
		approvalTimeout:   time.Duration(document.Approval.TimeoutHours) * time.Hour,
		handoffReminder:   time.Duration(reminderHours) * time.Hour,
		digest:            digestOf(rules, notifications, document.Approval.TimeoutHours, reminderHours, authors),
	}, nil
}

func parseReviewers(raw map[string]fileReviewer) (map[string]Reviewer, error) {
	if len(raw) == 0 {
		return nil, errors.New("review policy defines no reviewers")
	}
	reviewers := make(map[string]Reviewer, len(raw))
	prefixes := map[string]string{}
	for id, value := range raw {
		if !identifierPattern.MatchString(id) || id == VerifierReviewerID {
			return nil, fmt.Errorf("reviewer id %q is invalid or reserved", id)
		}
		if value.Protocol != ProtocolClaudeStructured && value.Protocol != ProtocolCodexEnvelope {
			return nil, fmt.Errorf("reviewer %s: protocol must be %s or %s", id, ProtocolClaudeStructured, ProtocolCodexEnvelope)
		}
		if !profilePattern.MatchString(value.AgentProfile) {
			return nil, fmt.Errorf("reviewer %s: agent_profile is invalid", id)
		}
		if !validText(value.Label, 64) {
			return nil, fmt.Errorf("reviewer %s: label must be a single line of at most 64 characters", id)
		}
		if !prefixPattern.MatchString(value.FindingPrefix) {
			return nil, fmt.Errorf("reviewer %s: finding_prefix must be one uppercase letter", id)
		}
		if other, taken := prefixes[value.FindingPrefix]; taken {
			return nil, fmt.Errorf("reviewers %s and %s share finding_prefix %s", other, id, value.FindingPrefix)
		}
		prefixes[value.FindingPrefix] = id
		reviewers[id] = Reviewer{ID: id, Label: value.Label, Protocol: value.Protocol, AgentProfile: value.AgentProfile, FindingPrefix: value.FindingPrefix}
	}
	return reviewers, nil
}

func parsePipelines(raw map[string]filePipeline, reviewers map[string]Reviewer, readPrompt func(string) ([]byte, error)) (map[string]Pipeline, error) {
	if len(raw) == 0 {
		return nil, errors.New("review policy defines no pipelines")
	}
	pipelines := make(map[string]Pipeline, len(raw))
	for name, value := range raw {
		if !identifierPattern.MatchString(name) {
			return nil, fmt.Errorf("pipeline name %q is invalid", name)
		}
		selected, err := selectReviewers(name, "reviewers", value.Reviewers, reviewers)
		if err != nil {
			return nil, err
		}
		incremental, err := selectReviewers(name, "incremental_reviewers", value.IncrementalReviewers, reviewers)
		if err != nil {
			return nil, err
		}
		members := map[string]bool{}
		for _, reviewer := range selected {
			members[reviewer.ID] = true
		}
		for _, reviewer := range incremental {
			if !members[reviewer.ID] {
				return nil, fmt.Errorf("pipeline %s: incremental reviewer %s is not one of its reviewers", name, reviewer.ID)
			}
		}
		protocol := selected[0].Protocol
		for _, reviewer := range selected {
			if reviewer.Protocol != protocol {
				// The prompt is written for one request and result contract.
				return nil, fmt.Errorf("pipeline %s: all reviewers must use the same protocol", name)
			}
		}
		if value.Checkout != CheckoutBase && value.Checkout != CheckoutHead {
			return nil, fmt.Errorf("pipeline %s: checkout must be %s or %s", name, CheckoutBase, CheckoutHead)
		}
		if len(value.ProjectSources) == 0 {
			return nil, fmt.Errorf("pipeline %s: project_sources must not be empty", name)
		}
		sources := map[string]bool{}
		for _, source := range value.ProjectSources {
			if !sourcePattern.MatchString(source) || sources[source] {
				return nil, fmt.Errorf("pipeline %s: project source %q is invalid or repeated", name, source)
			}
			sources[source] = true
		}
		prompt, err := loadPrompt(name, value.Prompt, readPrompt)
		if err != nil {
			return nil, err
		}
		verifier, err := parseVerifier(name, value.Verifier, reviewers, readPrompt)
		if err != nil {
			return nil, err
		}
		pipelines[name] = Pipeline{
			Name: name, Protocol: protocol, Reviewers: selected, IncrementalReviewers: incremental,
			Prompt: prompt, Checkout: value.Checkout, ProjectSources: append([]string(nil), value.ProjectSources...), Verifier: verifier,
		}
	}
	return pipelines, nil
}

func parseVerifier(pipeline string, raw *fileVerifier, reviewers map[string]Reviewer, readPrompt func(string) ([]byte, error)) (*Verifier, error) {
	if raw == nil {
		return nil, nil
	}
	reviewer, ok := reviewers[raw.Reviewer]
	if !ok {
		return nil, fmt.Errorf("pipeline %s: verifier names unknown reviewer %q", pipeline, raw.Reviewer)
	}
	if reviewer.Protocol != ProtocolClaudeStructured {
		// Only the structured protocol enforces the verdict schema.
		return nil, fmt.Errorf("pipeline %s: verifier must use protocol %s", pipeline, ProtocolClaudeStructured)
	}
	prompt, err := loadPrompt(pipeline, raw.Prompt, readPrompt)
	if err != nil {
		return nil, err
	}
	if len(raw.EscalateSeverities) == 0 || raw.EscalateCount < 1 {
		return nil, fmt.Errorf("pipeline %s: verifier needs escalate_severities and an escalate_count of at least 1", pipeline)
	}
	seen := map[string]bool{}
	for _, severity := range raw.EscalateSeverities {
		if !blockingSeverities[severity] || seen[severity] {
			return nil, fmt.Errorf("pipeline %s: verifier escalate_severities must list critical or high once each", pipeline)
		}
		seen[severity] = true
	}
	return &Verifier{Reviewer: reviewer, Prompt: prompt, EscalateSeverities: append([]string(nil), raw.EscalateSeverities...), EscalateCount: raw.EscalateCount}, nil
}

func selectReviewers(pipeline, field string, ids []string, reviewers map[string]Reviewer) ([]Reviewer, error) {
	if len(ids) == 0 || len(ids) > maxReviewersPerPR {
		return nil, fmt.Errorf("pipeline %s: %s must list between 1 and %d reviewers", pipeline, field, maxReviewersPerPR)
	}
	selected := make([]Reviewer, 0, len(ids))
	seen := map[string]bool{}
	for _, id := range ids {
		reviewer, ok := reviewers[id]
		if !ok || seen[id] {
			return nil, fmt.Errorf("pipeline %s: %s names unknown or repeated reviewer %q", pipeline, field, id)
		}
		seen[id] = true
		selected = append(selected, reviewer)
	}
	return selected, nil
}

func loadPrompt(pipeline, relative string, readPrompt func(string) ([]byte, error)) (string, error) {
	clean := path.Clean(relative)
	if relative == "" || clean != relative || path.IsAbs(clean) || clean == ".." || strings.HasPrefix(clean, "../") || strings.Contains(relative, "\\") {
		return "", fmt.Errorf("pipeline %s: prompt must be a relative path inside the policy directory", pipeline)
	}
	content, err := readPrompt(relative)
	if err != nil {
		return "", fmt.Errorf("pipeline %s: prompt %s: %w", pipeline, relative, err)
	}
	if len(content) > maxPromptBytes || !utf8.Valid(content) || bytes.ContainsRune(content, 0) || strings.TrimSpace(string(content)) == "" {
		return "", fmt.Errorf("pipeline %s: prompt %s must be non-empty UTF-8 text of at most %d bytes", pipeline, relative, maxPromptBytes)
	}
	return strings.TrimSpace(string(content)), nil
}

func parseTrigger(raw string) (Trigger, error) {
	if raw == string(TriggerReviewRequested) {
		return Trigger{Kind: TriggerReviewRequested}, nil
	}
	if label, ok := strings.CutPrefix(raw, "label:"); ok && labelPattern.MatchString(label) {
		return Trigger{Kind: TriggerLabel, Label: label}, nil
	}
	return Trigger{}, fmt.Errorf("trigger %q must be %s or label:<name>", raw, TriggerReviewRequested)
}

func parseRules(raw []fileRule, pipelines map[string]Pipeline) ([]Rule, error) {
	if len(raw) == 0 || len(raw) > maxRules {
		return nil, fmt.Errorf("review policy must list between 1 and %d repository rules", maxRules)
	}
	rules := make([]Rule, 0, len(raw))
	for index, value := range raw {
		position := fmt.Sprintf("repositories[%d] (%s)", index, value.Match)
		if !validMatch(value.Match) {
			return nil, fmt.Errorf("%s: match must be owner/repository or owner/* in lowercase", position)
		}
		for _, earlier := range rules {
			if matches(earlier.Match, value.Match) {
				// First match wins, so this rule could never apply.
				return nil, fmt.Errorf("%s: unreachable, %s matches first", position, earlier.Match)
			}
		}
		trigger, err := parseTrigger(value.Trigger)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", position, err)
		}
		pipeline, ok := pipelines[value.Pipeline]
		if !ok {
			return nil, fmt.Errorf("%s: unknown pipeline %q", position, value.Pipeline)
		}
		if value.OnFindings != FindingsRequestChanges && value.OnFindings != FindingsComment {
			return nil, fmt.Errorf("%s: on_findings must be %s or %s", position, FindingsRequestChanges, FindingsComment)
		}
		if value.OnClean != CleanApprove && value.OnClean != CleanAwait && value.OnClean != CleanComment {
			return nil, fmt.Errorf("%s: on_clean must be %s, %s or %s", position, CleanApprove, CleanAwait, CleanComment)
		}
		if value.OnClean == CleanApprove && !sameReviewers(pipeline.Reviewers, pipeline.IncrementalReviewers) {
			// Nobody confirms an automatic approval, so a re-review must not
			// rest on fewer reviewers than the full review.
			return nil, fmt.Errorf("%s: on_clean approve needs incremental_reviewers equal to reviewers", position)
		}
		if value.OnNotes != "" && value.OnNotes != NotesComment {
			return nil, fmt.Errorf("%s: on_notes must be %s when set", position, NotesComment)
		}
		if value.OnNotes == NotesComment && (value.OnClean != CleanAwait || trigger.Kind != TriggerReviewRequested) {
			// The author's renewed review request ends the hand-off, and only
			// the user's approval may follow it.
			return nil, fmt.Errorf("%s: on_notes %s needs on_clean %s and trigger %s", position, NotesComment, CleanAwait, TriggerReviewRequested)
		}
		humanReview, err := parseHumanReview(value.HumanReview, value.OnClean)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", position, err)
		}
		rules = append(rules, Rule{Match: value.Match, Trigger: trigger, Pipeline: pipeline, OnFindings: value.OnFindings, OnClean: value.OnClean, OnNotes: value.OnNotes, HumanReview: humanReview})
	}
	return rules, nil
}

// parseNotesHandoff returns the reminder hours of notes handed to an author.
// The section is required exactly when a rule hands notes off.
func parseNotesHandoff(raw *fileNotesHandoff, rules []Rule) (int, error) {
	used := slices.ContainsFunc(rules, func(rule Rule) bool { return rule.OnNotes == NotesComment })
	switch {
	case raw == nil && !used:
		return 0, nil
	case raw == nil:
		return 0, fmt.Errorf("review policy needs notes_handoff.reminder_hours because a rule sets on_notes %s", NotesComment)
	case !used:
		return 0, fmt.Errorf("review policy notes_handoff is set but no rule sets on_notes %s", NotesComment)
	case raw.ReminderHours < 1 || raw.ReminderHours > maxApprovalHours:
		return 0, fmt.Errorf("review policy notes_handoff.reminder_hours must be between 1 and %d", maxApprovalHours)
	}
	return raw.ReminderHours, nil
}

func parseNotifications(raw map[Event]Level) (map[Event]Level, error) {
	known := map[Event]bool{}
	for _, event := range Events {
		known[event] = true
	}
	for event := range raw {
		if !known[event] {
			return nil, fmt.Errorf("notifications: unknown event %q", event)
		}
	}
	notifications := make(map[Event]Level, len(Events))
	for _, event := range Events {
		level, ok := raw[event]
		if !ok {
			return nil, fmt.Errorf("notifications: event %s needs a level", event)
		}
		if level != LevelAction && level != LevelDigest && level != LevelLog {
			return nil, fmt.Errorf("notifications: event %s level must be action, digest or log", event)
		}
		notifications[event] = level
	}
	return notifications, nil
}

func sameReviewers(left, right []Reviewer) bool {
	if len(left) != len(right) {
		return false
	}
	for index := range left {
		if left[index].ID != right[index].ID {
			return false
		}
	}
	return true
}

func validMatch(pattern string) bool {
	owner, name, ok := strings.Cut(pattern, "/")
	return ok && ownerPattern.MatchString(owner) && (name == "*" || repoNamePattern.MatchString(name))
}

// matches reports whether pattern covers target, where target is a repository
// or another pattern.
func matches(pattern, target string) bool {
	if pattern == target {
		return true
	}
	owner, name, _ := strings.Cut(pattern, "/")
	targetOwner, _, _ := strings.Cut(target, "/")
	return name == "*" && owner == targetOwner
}

func validText(value string, maximum int) bool {
	return strings.TrimSpace(value) != "" && utf8.ValidString(value) && utf8.RuneCountInString(value) <= maximum && !strings.ContainsAny(value, "\x00\r\n")
}

func digestOf(rules []Rule, notifications map[Event]Level, timeoutHours, reminderHours int, readyDraftAuthors []string) string {
	events := make([]string, 0, len(notifications))
	for event, level := range notifications {
		events = append(events, string(event)+"="+string(level))
	}
	sort.Strings(events)
	hash := sha256.New()
	for _, rule := range rules {
		fmt.Fprintf(hash, "rule\x00%s\x00%s\x00%s\x00%s\x00%s\x00%s\x00%s\x00%s\x00", rule.Match, rule.Trigger, rule.Pipeline.Name, rule.Pipeline.Checkout, rule.OnFindings, rule.OnClean, rule.OnNotes, strings.Join(rule.Pipeline.ProjectSources, ","))
		for _, group := range [][]Reviewer{rule.Pipeline.Reviewers, rule.Pipeline.IncrementalReviewers} {
			for _, reviewer := range group {
				fmt.Fprintf(hash, "reviewer\x00%s\x00%s\x00%s\x00%s\x00%s\x00", reviewer.ID, reviewer.Label, reviewer.Protocol, reviewer.AgentProfile, reviewer.FindingPrefix)
			}
			hash.Write([]byte{0})
		}
		fmt.Fprintf(hash, "prompt\x00%d\x00%s", len(rule.Pipeline.Prompt), rule.Pipeline.Prompt)
		if verifier := rule.Pipeline.Verifier; verifier != nil {
			reviewer := verifier.Reviewer
			fmt.Fprintf(hash, "verifier\x00%s\x00%s\x00%s\x00%s\x00%s\x00%d\x00prompt\x00%d\x00%s", reviewer.ID, reviewer.Label, reviewer.Protocol, reviewer.AgentProfile,
				strings.Join(verifier.EscalateSeverities, ","), verifier.EscalateCount, len(verifier.Prompt), verifier.Prompt)
		}
		if human := rule.HumanReview; human != nil {
			fmt.Fprintf(hash, "human_review\x00%s\x00%s\x00", strings.Join(human.Labels, ","), strings.Join(human.DeletedTests, ","))
		}
		hash.Write([]byte{0})
	}
	fmt.Fprintf(hash, "notifications\x00%s\x00approval\x00%d\x00notes_handoff\x00%d\x00ready_draft_authors\x00%s", strings.Join(events, ","), timeoutHours, reminderHours, strings.Join(readyDraftAuthors, ","))
	return hex.EncodeToString(hash.Sum(nil))
}

// Resolve returns the first rule that matches the lowercase repository.
func (policy *Policy) Resolve(repository string) (Rule, bool) {
	for _, rule := range policy.rules {
		if matches(rule.Match, repository) {
			return rule, true
		}
	}
	return Rule{}, false
}

// Labels returns the distinct labels used by label triggers, sorted.
func (policy *Policy) Labels() []string {
	seen := map[string]bool{}
	labels := []string{}
	for _, rule := range policy.rules {
		if rule.Trigger.Kind == TriggerLabel && !seen[rule.Trigger.Label] {
			seen[rule.Trigger.Label] = true
			labels = append(labels, rule.Trigger.Label)
		}
	}
	sort.Strings(labels)
	return labels
}

// LabelScopes returns the search qualifiers of the rules triggered by label:
// user:<owner> for an owner rule and repo:<owner>/<name> for a repository rule.
// GitHub combines them with OR, and user: matches organizations as well.
func (policy *Policy) LabelScopes(label string) []string {
	seen := map[string]bool{}
	scopes := []string{}
	for _, rule := range policy.rules {
		if rule.Trigger.Kind != TriggerLabel || rule.Trigger.Label != label {
			continue
		}
		owner, name, _ := strings.Cut(rule.Match, "/")
		scope := "repo:" + rule.Match
		if name == "*" {
			scope = "user:" + owner
		}
		if !seen[scope] {
			seen[scope] = true
			scopes = append(scopes, scope)
		}
	}
	sort.Strings(scopes)
	return scopes
}

// AgentProfiles returns the distinct agent profiles the rules use, sorted.
func (policy *Policy) AgentProfiles() []string {
	seen := map[string]bool{}
	profiles := []string{}
	for _, rule := range policy.rules {
		reviewers := rule.Pipeline.Reviewers
		if rule.Pipeline.Verifier != nil {
			reviewers = append(append([]Reviewer(nil), reviewers...), rule.Pipeline.Verifier.Reviewer)
		}
		for _, reviewer := range reviewers {
			if !seen[reviewer.AgentProfile] {
				seen[reviewer.AgentProfile] = true
				profiles = append(profiles, reviewer.AgentProfile)
			}
		}
	}
	sort.Strings(profiles)
	return profiles
}

// ReviewRequestScopes returns the match patterns of the rules triggered by a
// review request, sorted, so the search covers only their repositories.
func (policy *Policy) ReviewRequestScopes() []string {
	seen := map[string]bool{}
	scopes := []string{}
	for _, rule := range policy.rules {
		if rule.Trigger.Kind == TriggerReviewRequested && !seen[rule.Match] {
			seen[rule.Match] = true
			scopes = append(scopes, rule.Match)
		}
	}
	sort.Strings(scopes)
	return scopes
}

// ReadyDraftAuthors returns the sorted logins whose drafts that ask for a
// review are marked ready for review, so they can be reviewed.
func (policy *Policy) ReadyDraftAuthors() []string {
	return append([]string(nil), policy.readyDraftAuthors...)
}

func (policy *Policy) Level(event Event) Level { return policy.notifications[event] }

// Notifications returns a copy of the event levels.
func (policy *Policy) Notifications() map[Event]Level {
	levels := make(map[Event]Level, len(policy.notifications))
	for event, level := range policy.notifications {
		levels[event] = level
	}
	return levels
}

func (policy *Policy) ApprovalTimeout() time.Duration { return policy.approvalTimeout }

// HandoffReminder is how long notes handed to an author wait before the user
// is reminded once; zero when no rule hands notes off.
func (policy *Policy) HandoffReminder() time.Duration { return policy.handoffReminder }

// Digest identifies the effective policy in logs and workflow inputs.
func (policy *Policy) Digest() string { return policy.digest }
