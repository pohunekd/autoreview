package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/url"
	"regexp"
	"strconv"
	"strings"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

var pohunekErrorCodePattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,63}$`)

// pohunekFailureCause keeps only the allowlisted error code of the CLI
// envelope; the message may echo prompts or authenticated URLs.
func pohunekFailureCause(output []byte, err error) (string, error) {
	var envelope struct {
		Err struct {
			Code string `json:"code"`
		} `json:"err"`
	}
	if json.Unmarshal(output, &envelope) != nil || !pohunekErrorCodePattern.MatchString(envelope.Err.Code) {
		return "", err
	}
	return envelope.Err.Code, &PohunekCodeError{Code: envelope.Err.Code, Cause: err}
}

func (external *External) pohunekEnvironment() []string {
	return cleanEnvironment("XDG_STATE_HOME="+external.cfg.PohunekState, "XDG_RUNTIME_DIR="+external.cfg.PohunekRuntime, "PAGER=cat")
}

func (external *External) pohunek(ctx context.Context, args []string) (json.RawMessage, error) {
	ctx, cancel := context.WithTimeout(ctx, external.cfg.PohunekTimeout)
	defer cancel()
	output, err := external.runner.Run(ctx, external.cfg.PohunekBin, args, nil, external.pohunekEnvironment())
	if err != nil {
		code, cause := pohunekFailureCause(output, err)
		switch code {
		case "session_not_found":
			return nil, workflowError("pohunek_session_not_found", true, cause)
		case "request_timeout":
			return nil, workflowError("pohunek_request_timeout", false, cause)
		default:
			return nil, workflowError("pohunek_request_failed", false, cause)
		}
	}
	var envelope struct {
		OK json.RawMessage `json:"ok"`
	}
	if json.Unmarshal(output, &envelope) != nil || envelope.OK == nil {
		return nil, workflowError("pohunek_response_invalid", true, nil)
	}
	return envelope.OK, nil
}

func canonicalRepository(host, raw string) string {
	if strings.Contains(raw, "@") && !strings.Contains(raw, "://") {
		parts := strings.SplitN(raw, ":", 2)
		if len(parts) != 2 || !strings.HasSuffix(parts[0], "@"+host) {
			return ""
		}
		raw = "https://" + host + "/" + parts[1]
	}
	parsed, err := url.Parse(raw)
	if err != nil || strings.ToLower(parsed.Hostname()) != host || parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" {
		return ""
	}
	value := strings.ToLower(strings.TrimSuffix(strings.TrimPrefix(parsed.Path, "/"), ".git"))
	if !repositoryPattern.MatchString(value) {
		return ""
	}
	return value
}

// ResolveProject finds the single pohunek project of the repository among the
// configured hosts and the pipeline's project sources.
func (external *External) ResolveProject(ctx context.Context, repository string, sources []string) (ProjectRoute, error) {
	allowed := map[string]bool{}
	for _, source := range sources {
		allowed[source] = true
	}
	candidates := []ProjectRoute{}
	for _, host := range external.cfg.PohunekHosts {
		output, err := external.pohunek(ctx, []string{"--host", host, "project", "list", "--json"})
		if err != nil {
			return ProjectRoute{}, err
		}
		var projects []struct {
			ID        string `json:"id"`
			Source    string `json:"source"`
			RepoRoot  string `json:"repo_root"`
			OriginURL string `json:"origin_url"`
		}
		if json.Unmarshal(output, &projects) != nil || projects == nil {
			return ProjectRoute{}, workflowError("pohunek_response_invalid", true, nil)
		}
		for _, project := range projects {
			if allowed[project.Source] && canonicalRepository(external.cfg.GitHubHost, project.OriginURL) == repository && identifierPattern.MatchString(project.ID) && strings.HasPrefix(project.RepoRoot, "/") {
				candidates = append(candidates, ProjectRoute{Host: host, ProjectID: project.ID})
			}
		}
	}
	if override, ok := external.cfg.RouteOverrides[repository]; ok {
		filtered := candidates[:0]
		for _, candidate := range candidates {
			if candidate == ProjectRoute(override) {
				filtered = append(filtered, candidate)
			}
		}
		candidates = filtered
	}
	switch len(candidates) {
	case 0:
		return ProjectRoute{}, workflowError("project_route_not_found", true, nil)
	case 1:
		return candidates[0], nil
	default:
		return ProjectRoute{}, workflowError("project_route_ambiguous", true, nil)
	}
}

type pohunekSession struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Agent     string `json:"agent"`
	ProjectID string `json:"project_id"`
	Branch    string `json:"branch"`
	State     string `json:"state"`
	Linked    bool   `json:"is_linked_worktree"`
	Warnings  []struct {
		Kind    string `json:"kind"`
		Message string `json:"message"`
		Detail  string `json:"detail"`
	} `json:"warnings"`
	Runtime struct {
		State      string `json:"state"`
		ID         string `json:"worker_instance_id"`
		Generation string `json:"runtime_generation"`
	} `json:"runtime"`
}

// FindSession returns the session this request owns, if any. An owned but
// unhealthy session is returned together with its error so the caller can
// remove it; a foreign session with the same name is never touched.
func (external *External) FindSession(ctx context.Context, request SessionRequest) (*SessionIdentity, error) {
	output, err := external.pohunek(ctx, []string{"--host", request.Route.Host, "session", "list", "--json"})
	if err != nil {
		return nil, err
	}
	var sessions []pohunekSession
	if json.Unmarshal(output, &sessions) != nil || sessions == nil {
		return nil, workflowError("pohunek_response_invalid", true, nil)
	}
	var match *pohunekSession
	for index := range sessions {
		if sessions[index].Name == request.Name() {
			if match != nil {
				return nil, workflowError("session_identity_ambiguous", true, nil)
			}
			match = &sessions[index]
		}
	}
	if match == nil {
		return nil, nil
	}
	generation, generationErr := strconv.Atoi(match.Runtime.Generation)
	if match.Agent != request.Reviewer.AgentProfile || match.ProjectID != request.Route.ProjectID || match.Branch != request.Branch() ||
		!identifierPattern.MatchString(match.ID) || !identifierPattern.MatchString(match.Runtime.ID) || generationErr != nil || generation <= 0 {
		return nil, workflowError("session_identity_invalid", true, generationErr)
	}
	identity := &SessionIdentity{Host: request.Route.Host, SessionID: match.ID, WorkerInstanceID: match.Runtime.ID, RuntimeGeneration: match.Runtime.Generation, ReviewerID: request.Reviewer.ID}
	if match.State != "running" || !match.Linked || match.Runtime.State != "live" {
		return identity, workflowError("session_identity_invalid", true, nil)
	}
	if len(match.Warnings) == 0 {
		return identity, nil
	}
	for _, warning := range match.Warnings {
		if warning.Kind != "fetch" || strings.TrimSpace(warning.Message) == "" || strings.TrimSpace(warning.Detail) == "" {
			return identity, workflowError("pohunek_session_warning", true, nil)
		}
	}
	code, permanent := schedule.FetchFailure(match.Warnings[0].Detail)
	// Prefer an actionable permanent cause if several fetches failed.
	for _, warning := range match.Warnings[1:] {
		nextCode, nextPermanent := schedule.FetchFailure(warning.Detail)
		if nextPermanent || code == "pohunek_base_fetch_failed" {
			code, permanent = nextCode, nextPermanent
		}
	}
	return identity, workflowError(code, permanent, nil)
}

// EnsureSession returns the request's session, starting it when absent. The
// daemon may accept a start after the client timed out, so the session list
// is always consulted after a start attempt.
func (external *External) EnsureSession(ctx context.Context, request SessionRequest) (SessionIdentity, error) {
	reject := func(session *SessionIdentity, cause error) (SessionIdentity, error) {
		if session == nil {
			return SessionIdentity{}, cause
		}
		if cleanupErr := external.RemoveSessionConfirmed(context.WithoutCancel(ctx), *session); cleanupErr != nil {
			return SessionIdentity{}, workflowError("pohunek_session_cleanup_failed", false, errors.Join(cause, cleanupErr))
		}
		return SessionIdentity{}, cause
	}
	existing, err := external.FindSession(ctx, request)
	if err != nil {
		return reject(existing, err)
	}
	if existing != nil {
		return *existing, nil
	}
	pr := request.PullRequest
	metadata, err := schedule.MetadataArgs(schedule.WorkSession{Repository: pr.Repository, Number: pr.Number, URL: pr.URL, HeadBranch: pr.HeadBranch, HeadSHA: pr.HeadSHA}, external.cfg.SessionGroup)
	if err != nil {
		return SessionIdentity{}, workflowError("review_identity_invalid", true, err)
	}
	args := []string{
		"--host", request.Route.Host, "session", "new", "--agent", request.Reviewer.AgentProfile,
		"--request-timeout-ms", strconv.FormatInt(external.cfg.PohunekTimeout.Milliseconds(), 10), "--json", "--yes",
		"--name", request.Name(), "--project", request.Route.ProjectID, "--branch", request.Branch(), "--base-branch", request.BaseRef(),
		"--cols", strconv.Itoa(external.cfg.TerminalCols), "--rows", strconv.Itoa(external.cfg.TerminalRows), "--input-stdin",
	}
	args = append(args, metadata...)
	startCtx, cancel := context.WithTimeout(ctx, external.cfg.PohunekTimeout)
	defer cancel()
	startOutput, startErr := external.runner.Run(startCtx, external.cfg.PohunekBin, args, []byte(request.Prompt), external.pohunekEnvironment())
	created, recoverErr := external.FindSession(context.WithoutCancel(ctx), request)
	if created != nil {
		if recoverErr != nil {
			return reject(created, recoverErr)
		}
		return *created, nil
	}
	if recoverErr != nil {
		return SessionIdentity{}, recoverErr
	}
	if startErr != nil {
		_, cause := pohunekFailureCause(startOutput, startErr)
		return SessionIdentity{}, workflowError("pohunek_session_start_failed", false, cause)
	}
	return SessionIdentity{}, workflowError("pohunek_response_invalid", true, nil)
}

// outputComplete reports whether a reviewer has written its final frame.
func outputComplete(protocol policy.Protocol, output []byte) bool {
	switch protocol {
	case policy.ProtocolClaudeStructured:
		return json.Valid(bytes.TrimSpace(output)) || bytes.HasSuffix(output, []byte("\n"))
	case policy.ProtocolCodexEnvelope:
		return bytes.Contains(output, []byte(codexExitPrefix))
	}
	return false
}

// SessionOutput returns the complete output of a reviewer, or nil while it is
// still running.
func (external *External) SessionOutput(ctx context.Context, session SessionIdentity, protocol policy.Protocol) ([]byte, error) {
	collected := []byte{}
	after := 0
	for {
		remaining := external.cfg.OutputMaxBytes - len(collected)
		if remaining <= 0 {
			return nil, workflowError("review_output_too_large", true, nil)
		}
		output, err := external.pohunek(ctx, []string{
			"session", "output", session.SessionID, "--host", session.Host, "--worker-instance-id", session.WorkerInstanceID,
			"--runtime-generation", session.RuntimeGeneration, "--after-offset", strconv.Itoa(after),
			"--max-bytes", strconv.Itoa(min(external.cfg.OutputChunkBytes, remaining)), "--wait-ms", strconv.Itoa(external.cfg.OutputWaitMS), "--json",
		})
		if err != nil {
			return nil, err
		}
		var chunk struct {
			SessionID        string `json:"session_id"`
			WorkerInstanceID string `json:"worker_instance_id"`
			Generation       string `json:"runtime_generation"`
			HistoryStart     string `json:"history_start_offset"`
			Start            string `json:"start_offset"`
			Next             string `json:"next_offset"`
			End              string `json:"runtime_end_offset"`
			Data             string `json:"data_base64"`
			HasMore          bool   `json:"has_more"`
			TimedOut         bool   `json:"timed_out"`
		}
		if json.Unmarshal(output, &chunk) != nil || chunk.SessionID != session.SessionID || chunk.WorkerInstanceID != session.WorkerInstanceID ||
			chunk.Generation != session.RuntimeGeneration || chunk.HistoryStart != "0" {
			return nil, workflowError("review_output_invalid", true, nil)
		}
		start, startErr := strconv.Atoi(chunk.Start)
		next, nextErr := strconv.Atoi(chunk.Next)
		end, endErr := strconv.Atoi(chunk.End)
		data, dataErr := base64.StdEncoding.Strict().DecodeString(chunk.Data)
		if startErr != nil || nextErr != nil || endErr != nil || dataErr != nil || start != after || next < start || end < next || len(data) != next-start || chunk.HasMore != (next < end) {
			return nil, workflowError("review_output_invalid", true, nil)
		}
		if end > external.cfg.OutputMaxBytes {
			return nil, workflowError("review_output_too_large", true, nil)
		}
		collected = append(collected, data...)
		if outputComplete(protocol, collected) {
			return collected, nil
		}
		if chunk.TimedOut {
			return nil, nil
		}
		after = next
	}
}

func (external *External) removeSession(ctx context.Context, session SessionIdentity) error {
	_, err := external.pohunek(ctx, []string{"session", "rm", session.SessionID, "--host", session.Host, "--json"})
	if errorCode(err) == "pohunek_session_not_found" {
		return nil
	}
	return err
}

// RemoveSessionConfirmed succeeds only after the session is gone from the
// host, so finished reviewer sessions never accumulate on it.
func (external *External) RemoveSessionConfirmed(ctx context.Context, session SessionIdentity) error {
	if err := external.removeSession(ctx, session); err != nil {
		return err
	}
	output, err := external.pohunek(ctx, []string{"--host", session.Host, "session", "list", "--json", "--filter", "id=" + session.SessionID})
	if err != nil {
		return err
	}
	var sessions []struct {
		ID string `json:"id"`
	}
	if json.Unmarshal(output, &sessions) != nil || sessions == nil {
		return workflowError("pohunek_response_invalid", true, nil)
	}
	for _, existing := range sessions {
		if existing.ID == session.SessionID {
			return workflowError("pohunek_session_cleanup_pending", false, nil)
		}
	}
	return nil
}
