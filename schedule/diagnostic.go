package schedule

import (
	"errors"
	"strings"
)

// Only allowlisted diagnostic categories escape into logs. Raw CLI errors can
// contain tokens, prompt contents, and authenticated URLs.
func Diagnostic(err error) string {
	if err == nil {
		return "unavailable"
	}
	message := strings.ToLower(err.Error())
	for _, entry := range []struct{ match, code string }{
		{"no space left on device", "disk_full"},
		{"cannot lock ref", "ref_locked"},
		{"authentication failed", "authentication_failed"},
		{"permission denied (publickey)", "authentication_failed"},
		{"could not read username", "authentication_failed"},
		{"couldn't find remote ref", "remote_ref_missing"},
		{"could not resolve host", "dns_failed"},
		{"could not resolve hostname", "dns_failed"},
		{"permission denied", "permission_denied"},
		{"already exists", "resource_exists"},
		{"deadline exceeded", "timeout"},
		{"request_timeout", "timeout"},
		{"timed out", "timeout"},
		{"connection refused", "connection_refused"},
		{"host_unavailable", "host_unavailable"},
		{"agent_not_found", "agent_not_found"},
	} {
		if strings.Contains(message, entry.match) {
			return entry.code
		}
	}
	return "unclassified_cli_failure"
}

// FetchFailure classifies untrusted warning text before it enters durable state.
func FetchFailure(detail string) (string, bool) {
	switch Diagnostic(errors.New(detail)) {
	case "timeout":
		return "pohunek_base_fetch_timeout", false
	case "authentication_failed":
		return "pohunek_base_fetch_authentication_failed", true
	case "permission_denied":
		return "pohunek_base_fetch_permission_denied", true
	case "remote_ref_missing":
		return "pohunek_base_fetch_ref_missing", true
	case "disk_full":
		return "pohunek_base_fetch_disk_full", true
	case "dns_failed":
		return "pohunek_base_fetch_dns_failed", false
	case "ref_locked":
		return "pohunek_base_fetch_ref_locked", false
	default:
		return "pohunek_base_fetch_failed", false
	}
}

// FailureExplanation never interpolates raw CLI output or credentials.
func FailureExplanation(code string) string {
	switch code {
	case "pohunek_base_fetch_timeout":
		return "Git timed out while fetching the review base branch on the review host. Check GitHub connectivity and whether the Git credential helper is waiting for a locked keyring."
	case "pohunek_base_fetch_authentication_failed":
		return "Git could not authenticate to fetch the review base branch. Restore GitHub credentials or SSH access on the review host; retrying cannot repair credentials."
	case "pohunek_base_fetch_permission_denied":
		return "Git was denied access while fetching the review base branch. Correct repository file permissions on the review host before retrying."
	case "pohunek_base_fetch_ref_missing":
		return "The review base branch was not found on the Git remote. Verify the PR base branch and the review host repository remote before retrying."
	case "pohunek_base_fetch_disk_full":
		return "The review host ran out of disk space while fetching the review base branch. Free disk space before retrying."
	case "pohunek_base_fetch_dns_failed":
		return "The review host could not resolve the Git remote hostname. Check its DNS and network connection."
	case "pohunek_base_fetch_ref_locked":
		return "Another Git fetch in the same repository on the review host held a ref lock while the review base branch was fetched. The lock is released when that fetch ends, so the next attempt fetches again."
	case "pohunek_session_warning":
		return "The reviewer session reported a warning other than a failed base fetch, for example an existing branch checked out instead of the review base, or a failed setup script or hook. The session was removed; check the project's setup scripts and hooks and stale `autoreview/review/*` branches on the review host before retrying."
	case "pohunek_base_fetch_failed":
		return "Git could not fetch the review base branch on the review host. Check the Pohunek session fetch warning and GitHub access; the retained error does not identify a more specific cause."
	case "pohunek_session_start_failed":
		return "Pohunek could not start the reviewer process. Check the review host, agent configuration, and service logs."
	case "pohunek_request_failed":
		return "The request to the Pohunek review host failed. Check that the host and its daemon are reachable."
	case "pohunek_request_timeout":
		return "The Pohunek review host did not answer within the CLI request timeout. The host was reachable; check its network latency and the size of its session list."
	case "github_request_failed":
		return "A GitHub API request failed. Check GitHub availability, authentication, and rate limits."
	default:
		return "The review could not complete. Inspect the review service logs using the diagnostic code below."
	}
}
