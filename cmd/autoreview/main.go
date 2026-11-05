package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"
)

const usage = "usage: hermes-pr-review serve | validate | poll | retry|resolve|approve --repository OWNER/REPO NUMBER HEAD | review --repository OWNER/REPO NUMBER | reviews [LIMIT] | candidates | awaiting | review-queue | backup-db PATH"

func main() {
	logger := slog.New(slog.NewJSONHandler(os.Stderr, nil))
	slog.SetDefault(logger)
	if err := run(os.Args[1:], logger); err != nil {
		payload, _ := json.Marshal(commandErrorResponseFor(err))
		fmt.Fprintln(os.Stdout, string(payload))
		logger.Error("command failed", "error_code", errorCode(err), "error", err)
		os.Exit(1)
	}
}

func printJSON(value any) error { return json.NewEncoder(os.Stdout).Encode(value) }

func run(arguments []string, logger *slog.Logger) error {
	if len(arguments) == 0 {
		return workflowError("command_invalid", true, errors.New(usage))
	}
	cfg, err := LoadConfig()
	if err != nil {
		return err
	}
	external := NewExternal(cfg)
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	switch arguments[0] {
	case "validate":
		// LoadConfig above already rejected an invalid configuration or policy.
		return printJSON(ValidationResult{PolicyDigest: cfg.Policy.Digest(), AgentProfiles: cfg.Policy.AgentProfiles()})
	case "candidates":
		// Read-only: shows what the next poll would consider.
		list, err := external.ListCandidates(ctx)
		if err != nil {
			return err
		}
		return printJSON(list)
	}
	store, err := OpenStore(cfg.StateDBPath, time.Now)
	if err != nil {
		return err
	}
	defer store.Close()
	engine := NewEngine(cfg, store, external, logger, time.Now, sleepWithContext)
	switch arguments[0] {
	case "serve":
		return serve(ctx, cfg, engine, external)
	case "poll":
		result, err := engine.Poll(ctx)
		if err != nil {
			return err
		}
		return printJSON(result)
	case "retry":
		repository, number, head, err := parseReviewArguments(arguments[1:], true)
		if err != nil {
			return err
		}
		result, err := engine.Retry(ctx, RetryRequest{Repository: repository, Number: number, HeadSHA: head})
		if err != nil {
			return err
		}
		return printJSON(result)
	case "resolve":
		repository, number, prefix, err := parseReviewArguments(arguments[1:], true)
		if err != nil {
			return err
		}
		result, err := engine.Resolve(ctx, ResolveRequest{Repository: repository, Number: number, HeadPrefix: prefix})
		if err != nil {
			return err
		}
		return printJSON(result)
	case "approve":
		repository, number, head, err := parseReviewArguments(arguments[1:], false)
		if err != nil {
			return err
		}
		resolved, err := engine.Resolve(ctx, ResolveRequest{Repository: repository, Number: number, HeadPrefix: head})
		if err != nil {
			return err
		}
		if resolved.HeadSHA != head || !identifierPattern.MatchString(resolved.WorkflowID) {
			return workflowError("review_identity_invalid", true, nil)
		}
		result, err := engine.Approve(ctx, resolved.WorkflowID, ApproveRequest{Repository: repository, Number: number, HeadSHA: head})
		if err != nil {
			return err
		}
		return printJSON(result)
	case "awaiting":
		records, err := engine.ListAwaiting(ctx)
		if err != nil {
			return err
		}
		return printJSON(records)
	case "review-queue":
		result, err := engine.ReviewQueue(ctx)
		if err != nil {
			return err
		}
		return printJSON(result)
	case "reviews":
		limit := cfg.ReviewListLimit
		if len(arguments) > 2 {
			return workflowError("command_invalid", true, errors.New(usage))
		}
		if len(arguments) == 2 {
			if limit, err = strconv.Atoi(arguments[1]); err != nil || limit <= 0 || limit > cfg.ReviewListLimit {
				return workflowError("command_invalid", true, fmt.Errorf("limit must be between 1 and %d", cfg.ReviewListLimit))
			}
		}
		reviews, err := store.listReviews(ctx, "", limit)
		if err != nil {
			return err
		}
		return printJSON(reviews)
	case "review":
		repository, number, err := parsePullRequestArguments(arguments[1:])
		if err != nil {
			return err
		}
		status, err := engine.PullRequestStatus(ctx, repository, number)
		if err != nil {
			return err
		}
		return printJSON(status)
	case "backup-db":
		if len(arguments) != 2 {
			return workflowError("command_invalid", true, errors.New(usage))
		}
		if err := store.backupTo(ctx, arguments[1]); err != nil {
			return err
		}
		return printJSON(map[string]any{"ok": true, "path": arguments[1]})
	default:
		return workflowError("command_invalid", true, errors.New(usage))
	}
}

// ValidationResult tells the installer what the policy requires of the review
// hosts before the service is restarted.
type ValidationResult struct {
	PolicyDigest  string   `json:"policy_digest"`
	AgentProfiles []string `json:"agent_profiles"`
}

// serve refuses to start under a GitHub identity other than the configured
// login, because approvals and the own-pull-request check depend on it.
func serve(ctx context.Context, cfg Config, engine *Engine, external *External) error {
	checkCtx, cancel := context.WithTimeout(ctx, cfg.GHTimeout)
	login, err := external.ViewerLogin(checkCtx)
	cancel()
	if err != nil {
		return err
	}
	if login != cfg.GitHubLogin {
		return configError("configuration_invalid", envPrefix+"_GITHUB_LOGIN", fmt.Errorf("token belongs to %s", login))
	}
	return engine.Serve(ctx)
}

// parseReviewArguments reads `--repository OWNER/REPO NUMBER HEAD`. With
// prefix, HEAD may be an abbreviated head of at least 12 characters.
func parseReviewArguments(arguments []string, prefix bool) (string, int, string, error) {
	repository, positional := splitRepository(arguments)
	if !repositoryPattern.MatchString(repository) {
		return "", 0, "", workflowError("repository_not_allowed", true, nil)
	}
	if len(positional) != 2 {
		return "", 0, "", workflowError("command_invalid", true, errors.New(usage))
	}
	number, err := strconv.Atoi(positional[0])
	head := positional[1]
	if err != nil || number <= 0 || (prefix && !headPrefixValid(head)) || (!prefix && !shaPattern.MatchString(head)) {
		return "", 0, "", workflowError("review_identity_invalid", true, nil)
	}
	return repository, number, head, nil
}

// parsePullRequestArguments reads `--repository OWNER/REPO NUMBER`.
func parsePullRequestArguments(arguments []string) (string, int, error) {
	repository, positional := splitRepository(arguments)
	if !repositoryPattern.MatchString(repository) {
		return "", 0, workflowError("repository_not_allowed", true, nil)
	}
	if len(positional) != 1 {
		return "", 0, workflowError("command_invalid", true, errors.New(usage))
	}
	number, err := strconv.Atoi(positional[0])
	if err != nil || number <= 0 {
		return "", 0, workflowError("review_identity_invalid", true, nil)
	}
	return repository, number, nil
}

func splitRepository(arguments []string) (string, []string) {
	repository := ""
	positional := []string{}
	for index := 0; index < len(arguments); index++ {
		if arguments[index] == "--repository" && index+1 < len(arguments) {
			repository = arguments[index+1]
			index++
			continue
		}
		positional = append(positional, arguments[index])
	}
	return repository, positional
}
