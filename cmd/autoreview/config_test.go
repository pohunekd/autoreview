package main

import (
	"bufio"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/pohunekd/autoreview/policy"
)

// deployedEnvironment reads the synthetic service environment file and
// adds placeholder secrets, which a real deployment supplies out of band.
func deployedEnvironment(t *testing.T) map[string]string {
	t.Helper()
	file, err := os.Open(filepath.Join("..", "..", "testdata", "review.env"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	values := map[string]string{}
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		name, value, ok := strings.Cut(line, "=")
		if !ok {
			t.Fatalf("malformed env line %q", line)
		}
		values[name] = strings.Trim(value, `"'`)
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	values["GH_TOKEN"] = "placeholder-token"
	values["MATRIX_HOMESERVER"] = "https://matrix.example.org"
	values["MATRIX_ACCESS_TOKEN"] = "placeholder-token"
	return values
}

func loadDeployed(t *testing.T, values map[string]string) (Config, error) {
	t.Helper()
	repositoryPolicy := filepath.Join("..", "..", "testdata", "review-policy.yaml")
	return loadConfig(func(name string) (string, bool) {
		value, ok := values[name]
		return value, ok
	}, func(string) (*policy.Policy, error) { return policy.Load(repositoryPolicy) })
}

func TestDeployedConfigurationLoads(t *testing.T) {
	cfg, err := loadDeployed(t, deployedEnvironment(t))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.GitHubLogin != "example-user" || cfg.StateDBPath == "" {
		t.Fatalf("unexpected deployed configuration: %+v", cfg)
	}
	if cfg.SessionGroup != "autoreview" {
		t.Fatalf("session group = %q", cfg.SessionGroup)
	}
	if cfg.OutputPollInterval != 15*time.Second || cfg.Schedule.Debounce != 2*time.Minute {
		t.Fatalf("poll %s debounce %s", cfg.OutputPollInterval, cfg.Schedule.Debounce)
	}
	if limits := runLimits(cfg); !limits.valid() {
		t.Fatalf("deployed run limits are invalid: %+v", limits)
	}
}

// A published decision must outlive the notes hand-off reminder, or the
// reminder finds nothing to remind about.
func TestPublishedRetentionMustExceedTheNotesHandoffReminder(t *testing.T) {
	document, err := os.ReadFile(filepath.Join("..", "..", "testdata", "review-policy.yaml"))
	if err != nil {
		t.Fatal(err)
	}
	handoff := strings.Replace(string(document), "    on_clean: approve\n", "    on_clean: await\n    on_notes: comment\n", 1) + "notes_handoff:\n  reminder_hours: 24\n"
	loaded, err := policy.Parse([]byte(handoff), func(relative string) ([]byte, error) {
		return os.ReadFile(filepath.Join("..", "..", "testdata", relative))
	})
	if err != nil {
		t.Fatal(err)
	}
	values := deployedEnvironment(t)
	values["HERMES_PR_REVIEW_PUBLISHED_RETENTION_SECONDS"] = "3600"
	_, err = loadConfig(func(name string) (string, bool) {
		value, ok := values[name]
		return value, ok
	}, func(string) (*policy.Policy, error) { return loaded, nil })
	if err == nil || !strings.Contains(err.Error(), "notes hand-off reminder") {
		t.Fatalf("retention shorter than the hand-off reminder: error = %v", err)
	}
}

func TestMissingOrUnsafeConfigurationFailsFast(t *testing.T) {
	for _, name := range []string{"HERMES_PR_REVIEW_POLICY_FILE", "GH_TOKEN", "HERMES_PR_REVIEW_MATRIX_ACTION_ROOM", "HERMES_PR_REVIEW_PUBLISHED_RETENTION_SECONDS", "HERMES_PR_REVIEW_AI_SIGNATURE", "HERMES_PR_REVIEW_STATE_DB_PATH", "HERMES_PR_REVIEW_POLL_INTERVAL_SECONDS", "HERMES_PR_REVIEW_SESSION_GROUP"} {
		values := deployedEnvironment(t)
		delete(values, name)
		if _, err := loadDeployed(t, values); err == nil || !strings.Contains(err.Error(), name) {
			t.Fatalf("missing %s: error = %v", name, err)
		}
	}
	unsafe := map[string]string{
		"HERMES_PR_REVIEW_MAX_CONCURRENT_REVIEWS": "0",
		"HERMES_PR_REVIEW_DISCOVERY_LOCK_POLL_MS": "0",
		"HERMES_PR_REVIEW_OUTPUT_CHUNK_BYTES":     "900000",
		"HERMES_PR_REVIEW_POLICY_FILE":            "relative/policy.yaml",
		"HERMES_PR_REVIEW_GITHUB_LOGIN":           "Not A Login",
		"HERMES_PR_REVIEW_SESSION_GROUP":          "auto\u202ereview",
	}
	for name, value := range unsafe {
		values := deployedEnvironment(t)
		values[name] = value
		if _, err := loadDeployed(t, values); err == nil {
			t.Fatalf("%s=%s was accepted", name, value)
		}
	}
	for _, signature := range []string{"  ", "AI review\nby @example-user", "AI review by @example-user", "AI <!-- review -->", " AI review", strings.Repeat("a", 201), "**AI review**: machine-written", "🤖 AI review: example-agent"} {
		values := deployedEnvironment(t)
		values["HERMES_PR_REVIEW_AI_SIGNATURE"] = signature
		if _, err := loadDeployed(t, values); err == nil {
			t.Fatalf("AI signature %q was accepted", signature)
		}
	}
}
