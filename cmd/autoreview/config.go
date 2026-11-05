package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/pohunekd/autoreview/policy"
	"github.com/pohunekd/autoreview/schedule"
)

const envPrefix = "HERMES_PR_REVIEW"

// pohunekOutputChunkMaxBytes is the largest output chunk the pohunek protocol
// returns in one response.
const pohunekOutputChunkMaxBytes = 783240

type RouteOverride struct {
	Host      string `json:"host"`
	ProjectID string `json:"project_id"`
}

type ConfigError struct {
	Code  string
	Name  string
	Cause error
}

func (err *ConfigError) Error() string {
	if err.Cause == nil {
		return fmt.Sprintf("%s: %s", err.Code, err.Name)
	}
	return fmt.Sprintf("%s: %s: %v", err.Code, err.Name, err.Cause)
}

func (err *ConfigError) Unwrap() error { return err.Cause }

func configError(code, name string, cause error) error {
	return &ConfigError{Code: code, Name: name, Cause: cause}
}

// Config holds the technical settings of the service. What is reviewed and
// how it is published lives in the review policy file.
type Config struct {
	Policy         *policy.Policy
	Schedule       schedule.Policy
	GitHubHost     string
	GitHubLogin    string
	GitHubToken    string
	GHBin          string
	PohunekBin     string
	PohunekState   string
	PohunekRuntime string
	PohunekHosts   []string
	RouteOverrides map[string]RouteOverride

	MatrixHomeserverURL string
	MatrixAccessToken   string
	MatrixMentionUser   string
	MatrixRooms         map[policy.Level]string
	// QueueMatrixRoom receives the hourly review queue, which never mentions anyone.
	QueueMatrixRoom string
	// QueueMaxItems caps the requests listed in one queue message.
	QueueMaxItems int

	// StateDBPath is the SQLite database of discovery state and reviews.
	StateDBPath string

	GHTimeout           time.Duration
	PohunekTimeout      time.Duration
	MatrixTimeout       time.Duration
	SessionTimeout      time.Duration
	OutputPollInterval  time.Duration
	SessionStartRetry   time.Duration
	SessionCleanupRetry time.Duration
	ReviewerRunRetry    time.Duration
	// StepRetry bounds how long one failing step is retried; the delay starts
	// at StepRetryInitial and doubles up to StepRetryMaxInterval.
	StepRetry            time.Duration
	StepRetryInitial     time.Duration
	StepRetryMaxInterval time.Duration
	// PollInterval spaces discovery passes; WorkerTick spaces checks for
	// reviews to start, resume or expire.
	PollInterval time.Duration
	WorkerTick   time.Duration
	// DiscoveryLockTimeout bounds the wait for another process's discovery
	// pass; the lock is retried every DiscoveryLockPoll.
	DiscoveryLockTimeout time.Duration
	DiscoveryLockPoll    time.Duration
	// ReviewRetention bounds how long completed reviews stay in the database.
	ReviewRetention time.Duration

	SessionStartAttempts   int
	SessionCleanupAttempts int
	ReviewerRunAttempts    int
	MaxConcurrentReviews   int
	// ReviewListLimit caps the reviews one status command returns.
	ReviewListLimit int

	PollLimit            int
	SearchPageLimit      int
	StateEntryLimit      int
	ScheduleMaxBytes     int
	OutputMaxBytes       int
	OutputChunkBytes     int
	OutputWaitMS         int
	ResultMaxBytes       int
	MaxFindings          int
	MaxTests             int
	MinSummaryCharacters int
	MaxSummaryCharacters int
	MaxTitleCharacters   int
	MaxBodyCharacters    int
	// MaxDiscussionCharacters bounds the discussion handed to the reviewers of
	// a review with prior findings.
	MaxDiscussionCharacters int
	MaxPathCharacters       int
	MaxTestCharacters       int
	GitHubBodyMaxBytes      int
	// AISignature ends every published review body and inline comment, so a
	// reader or an agent knows the text is machine-written although it is
	// posted from a human account.
	AISignature         string
	MatrixMaxCharacters int
	// PublishedRetention bounds how long the last published decision of a
	// pull request is kept as the baseline of an incremental review.
	PublishedRetention time.Duration
	ExcerptCharacters  int
	TerminalCols       int
	TerminalRows       int
	// SessionGroup is the pohunek-companion group every review session is
	// placed in.
	SessionGroup string
}

var (
	identifierPattern  = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]*$`)
	githubLoginPattern = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?$`)
	repositoryPattern  = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,38}/[a-z0-9_.-]{1,100}$`)
	shaPattern         = regexp.MustCompile(`^[0-9a-f]{40}$`)
	matrixRoomPattern  = regexp.MustCompile(`^![A-Za-z0-9._~=\-:]+$`)
	matrixUserPattern  = regexp.MustCompile(`^@[a-z0-9._=\-/+]+:[A-Za-z0-9.\-]+(:[0-9]+)?$`)
)

func LoadConfig() (Config, error) { return loadConfig(os.LookupEnv, policy.Load) }

func loadConfig(lookup func(string) (string, bool), loadPolicy func(string) (*policy.Policy, error)) (Config, error) {
	name := func(suffix string) string { return envPrefix + "_" + suffix }
	required := func(variable string) (string, error) {
		value, ok := lookup(variable)
		if !ok || strings.TrimSpace(value) == "" {
			return "", configError("configuration_missing", variable, nil)
		}
		return value, nil
	}
	positive := func(variable string) (int, error) {
		value, err := required(variable)
		if err != nil {
			return 0, err
		}
		parsed, err := strconv.Atoi(value)
		if err != nil || parsed <= 0 {
			return 0, configError("configuration_invalid", variable, errors.New("must be a positive integer"))
		}
		return parsed, nil
	}
	jsonValue := func(variable string, target any) error {
		value, err := required(variable)
		if err != nil {
			return err
		}
		if err := json.Unmarshal([]byte(value), target); err != nil {
			return configError("configuration_invalid", variable, err)
		}
		return nil
	}

	var cfg Config
	var err error
	policyFile, err := required(name("POLICY_FILE"))
	if err != nil {
		return Config{}, err
	}
	if !filepath.IsAbs(policyFile) {
		return Config{}, configError("configuration_invalid", name("POLICY_FILE"), errors.New("must be an absolute path"))
	}
	if cfg.Policy, err = loadPolicy(policyFile); err != nil {
		return Config{}, configError("configuration_invalid", name("POLICY_FILE"), err)
	}
	if cfg.Schedule, err = schedule.LoadPolicy(lookup, envPrefix); err != nil {
		return Config{}, configError("configuration_invalid", name("DEBOUNCE_SECONDS"), err)
	}

	for _, field := range []struct {
		variable string
		target   *string
	}{
		{name("GITHUB_HOST"), &cfg.GitHubHost}, {name("GITHUB_LOGIN"), &cfg.GitHubLogin}, {"GH_TOKEN", &cfg.GitHubToken},
		{name("GH_BIN"), &cfg.GHBin}, {name("POHUNEK_BIN"), &cfg.PohunekBin}, {name("POHUNEK_STATE_HOME"), &cfg.PohunekState},
		{name("POHUNEK_RUNTIME_DIR"), &cfg.PohunekRuntime},
		{"MATRIX_HOMESERVER", &cfg.MatrixHomeserverURL}, {"MATRIX_ACCESS_TOKEN", &cfg.MatrixAccessToken},
		{name("MATRIX_MENTION_USER"), &cfg.MatrixMentionUser},
		{name("STATE_DB_PATH"), &cfg.StateDBPath},
		{name("AI_SIGNATURE"), &cfg.AISignature},
		{name("QUEUE_MATRIX_ROOM"), &cfg.QueueMatrixRoom},
		{name("SESSION_GROUP"), &cfg.SessionGroup},
	} {
		if *field.target, err = required(field.variable); err != nil {
			return Config{}, err
		}
	}
	if err := jsonValue(name("POHUNEK_HOSTS_JSON"), &cfg.PohunekHosts); err != nil {
		return Config{}, err
	}
	if err := jsonValue(name("ROUTE_OVERRIDES_JSON"), &cfg.RouteOverrides); err != nil {
		return Config{}, err
	}
	cfg.MatrixRooms = map[policy.Level]string{}
	for level, suffix := range map[policy.Level]string{policy.LevelAction: "MATRIX_ACTION_ROOM", policy.LevelDigest: "MATRIX_DIGEST_ROOM", policy.LevelLog: "MATRIX_LOG_ROOM"} {
		if cfg.MatrixRooms[level], err = required(name(suffix)); err != nil {
			return Config{}, err
		}
	}

	for _, field := range []struct {
		suffix string
		target *time.Duration
	}{
		{"GH_TIMEOUT_SECONDS", &cfg.GHTimeout}, {"POHUNEK_TIMEOUT_SECONDS", &cfg.PohunekTimeout},
		{"MATRIX_TIMEOUT_SECONDS", &cfg.MatrixTimeout},
		{"SESSION_TIMEOUT_SECONDS", &cfg.SessionTimeout}, {"OUTPUT_POLL_SECONDS", &cfg.OutputPollInterval},
		{"SESSION_START_RETRY_SECONDS", &cfg.SessionStartRetry}, {"SESSION_CLEANUP_RETRY_SECONDS", &cfg.SessionCleanupRetry},
		{"REVIEWER_RUN_RETRY_SECONDS", &cfg.ReviewerRunRetry},
		{"PUBLISHED_RETENTION_SECONDS", &cfg.PublishedRetention}, {"STEP_RETRY_SECONDS", &cfg.StepRetry},
		{"STEP_RETRY_INITIAL_SECONDS", &cfg.StepRetryInitial}, {"STEP_RETRY_MAX_INTERVAL_SECONDS", &cfg.StepRetryMaxInterval},
		{"POLL_INTERVAL_SECONDS", &cfg.PollInterval}, {"WORKER_TICK_SECONDS", &cfg.WorkerTick},
		{"DISCOVERY_LOCK_TIMEOUT_SECONDS", &cfg.DiscoveryLockTimeout}, {"REVIEW_RETENTION_SECONDS", &cfg.ReviewRetention},
	} {
		seconds, err := positive(name(field.suffix))
		if err != nil {
			return Config{}, err
		}
		*field.target = time.Duration(seconds) * time.Second
	}
	for _, field := range []struct {
		suffix string
		target *int
	}{
		{"SESSION_START_MAX_ATTEMPTS", &cfg.SessionStartAttempts}, {"SESSION_CLEANUP_MAX_ATTEMPTS", &cfg.SessionCleanupAttempts},
		{"REVIEWER_RUN_MAX_ATTEMPTS", &cfg.ReviewerRunAttempts}, {"MAX_CONCURRENT_REVIEWS", &cfg.MaxConcurrentReviews},
		{"REVIEW_LIST_LIMIT", &cfg.ReviewListLimit}, {"QUEUE_MAX_ITEMS", &cfg.QueueMaxItems},
		{"POLL_LIMIT", &cfg.PollLimit}, {"SEARCH_PAGE_LIMIT", &cfg.SearchPageLimit}, {"STATE_ENTRY_LIMIT", &cfg.StateEntryLimit},
		{"SCHEDULER_MAX_BYTES", &cfg.ScheduleMaxBytes}, {"OUTPUT_MAX_BYTES", &cfg.OutputMaxBytes},
		{"OUTPUT_CHUNK_BYTES", &cfg.OutputChunkBytes}, {"OUTPUT_WAIT_MS", &cfg.OutputWaitMS}, {"RESULT_MAX_BYTES", &cfg.ResultMaxBytes},
		{"MAX_FINDINGS", &cfg.MaxFindings}, {"MAX_TESTS", &cfg.MaxTests},
		{"MIN_SUMMARY_CHARACTERS", &cfg.MinSummaryCharacters}, {"MAX_SUMMARY_CHARACTERS", &cfg.MaxSummaryCharacters},
		{"MAX_TITLE_CHARACTERS", &cfg.MaxTitleCharacters}, {"MAX_BODY_CHARACTERS", &cfg.MaxBodyCharacters},
		{"MAX_DISCUSSION_CHARACTERS", &cfg.MaxDiscussionCharacters},
		{"MAX_PATH_CHARACTERS", &cfg.MaxPathCharacters}, {"MAX_TEST_CHARACTERS", &cfg.MaxTestCharacters},
		{"GITHUB_BODY_MAX_BYTES", &cfg.GitHubBodyMaxBytes}, {"MATRIX_MAX_CHARACTERS", &cfg.MatrixMaxCharacters},
		{"EXCERPT_CHARACTERS", &cfg.ExcerptCharacters}, {"TERMINAL_COLS", &cfg.TerminalCols}, {"TERMINAL_ROWS", &cfg.TerminalRows},
	} {
		if *field.target, err = positive(name(field.suffix)); err != nil {
			return Config{}, err
		}
	}
	lockPollMS, err := positive(name("DISCOVERY_LOCK_POLL_MS"))
	if err != nil {
		return Config{}, err
	}
	cfg.DiscoveryLockPoll = time.Duration(lockPollMS) * time.Millisecond
	if err := cfg.Validate(); err != nil {
		return Config{}, configError("configuration_invalid", envPrefix, err)
	}
	return cfg, nil
}

func (cfg Config) Validate() error {
	if cfg.GitHubHost != "github.com" || !githubLoginPattern.MatchString(cfg.GitHubLogin) || strings.ContainsAny(cfg.GitHubToken, "\r\n ") {
		return errors.New("invalid GitHub configuration")
	}
	for label, value := range map[string]string{"GitHub CLI": cfg.GHBin, "Pohunek CLI": cfg.PohunekBin, "Pohunek state home": cfg.PohunekState, "Pohunek runtime directory": cfg.PohunekRuntime} {
		if !filepath.IsAbs(value) {
			return fmt.Errorf("%s path must be absolute", label)
		}
	}
	if len(cfg.PohunekHosts) == 0 {
		return errors.New("pohunek hosts must not be empty")
	}
	for _, host := range cfg.PohunekHosts {
		if !identifierPattern.MatchString(host) {
			return errors.New("invalid Pohunek host")
		}
	}
	for repository, route := range cfg.RouteOverrides {
		if !repositoryPattern.MatchString(repository) || !identifierPattern.MatchString(route.Host) || !identifierPattern.MatchString(route.ProjectID) {
			return errors.New("invalid route override")
		}
	}
	if err := validateSignature(cfg.AISignature); err != nil {
		return err
	}
	if !schedule.SessionGroupPattern.MatchString(cfg.SessionGroup) {
		return errors.New("invalid review session group")
	}
	if err := validateMatrix(cfg); err != nil {
		return err
	}
	if !filepath.IsAbs(cfg.StateDBPath) || filepath.Clean(cfg.StateDBPath) != cfg.StateDBPath || strings.ContainsAny(cfg.StateDBPath, "?#") {
		return errors.New("state database path must be a clean absolute path")
	}
	if cfg.OutputMaxBytes > 16<<20 || cfg.OutputChunkBytes > pohunekOutputChunkMaxBytes || cfg.OutputChunkBytes > cfg.OutputMaxBytes ||
		cfg.OutputWaitMS > 8000 || cfg.ScheduleMaxBytes > 64<<20 || cfg.ResultMaxBytes > cfg.OutputMaxBytes ||
		cfg.MinSummaryCharacters > 500 || cfg.MinSummaryCharacters > cfg.MaxSummaryCharacters ||
		cfg.MatrixMaxCharacters > 32000 || cfg.ExcerptCharacters > cfg.MatrixMaxCharacters/4 || cfg.GitHubBodyMaxBytes > 65536 ||
		cfg.TerminalCols < 120 || cfg.TerminalCols > 400 || cfg.TerminalRows < 100 || cfg.TerminalRows > 250 ||
		cfg.SessionStartAttempts > 5 || cfg.SessionStartRetry > 5*time.Minute || cfg.SessionCleanupAttempts > 10 || cfg.SessionCleanupRetry > 5*time.Minute ||
		cfg.ReviewerRunAttempts > 3 || cfg.ReviewerRunRetry > 5*time.Minute ||
		cfg.OutputPollInterval > cfg.SessionTimeout || cfg.StepRetry > time.Hour ||
		cfg.StepRetryMaxInterval < cfg.StepRetryInitial || cfg.StepRetryMaxInterval > cfg.StepRetry ||
		cfg.MaxConcurrentReviews > 32 || cfg.ReviewListLimit > cfg.StateEntryLimit || cfg.PollInterval > time.Hour || cfg.WorkerTick > cfg.PollInterval ||
		cfg.DiscoveryLockTimeout > time.Hour || cfg.DiscoveryLockPoll > cfg.DiscoveryLockTimeout || cfg.ReviewRetention < 24*time.Hour ||
		cfg.QueueMaxItems >= 1000 {
		return errors.New("configured limit is outside the safe range")
	}
	if cfg.Policy != nil && cfg.Policy.HandoffReminder() >= cfg.PublishedRetention {
		// The reminder reads the published record, so it must outlive the wait.
		return errors.New("published retention must exceed the notes hand-off reminder")
	}
	return nil
}

// validateSignature accepts one line of Markdown that cannot mention anyone,
// embed HTML, or imitate the review marker, and that starts with
// "Written by AI:" after an optional emoji.
func validateSignature(signature string) error {
	if strings.TrimSpace(signature) != signature || len(signature) > 200 || strings.ContainsAny(signature, "@<>") ||
		strings.ContainsFunc(signature, unicode.IsControl) || !strings.HasPrefix(strings.TrimLeftFunc(signature, isSignatureEmoji), "Written by AI:") {
		return errors.New("invalid AI signature")
	}
	return nil
}

// isSignatureEmoji accepts the leading emoji of a signature, including its
// variation selector and the space after it.
func isSignatureEmoji(r rune) bool {
	return r == ' ' || r == '\uFE0F' || unicode.Is(unicode.So, r)
}

func validateMatrix(cfg Config) error {
	parsed, err := url.Parse(cfg.MatrixHomeserverURL)
	if err != nil || parsed.User != nil || parsed.Hostname() == "" || parsed.RawQuery != "" || parsed.Fragment != "" {
		return errors.New("invalid Matrix URL")
	}
	if parsed.Scheme != "https" && (parsed.Scheme != "http" || parsed.Hostname() != "127.0.0.1") {
		return errors.New("matrix URL must use HTTPS or loopback HTTP")
	}
	if !matrixUserPattern.MatchString(cfg.MatrixMentionUser) || strings.ContainsAny(cfg.MatrixAccessToken, "\r\n ") {
		return errors.New("invalid Matrix identity")
	}
	for _, level := range []policy.Level{policy.LevelAction, policy.LevelDigest, policy.LevelLog} {
		if !matrixRoomPattern.MatchString(cfg.MatrixRooms[level]) {
			return fmt.Errorf("invalid Matrix %s room", level)
		}
	}
	if !matrixRoomPattern.MatchString(cfg.QueueMatrixRoom) {
		return errors.New("invalid Matrix queue room")
	}
	return nil
}
