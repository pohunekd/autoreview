package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"sync"
	"syscall"
	"time"

	_ "modernc.org/sqlite"

	"github.com/pohunekd/autoreview/schedule"
)

// Review statuses. A queued or running review is resumed by the engine after a
// restart; an awaiting review waits for an approval decision or its deadline.
const (
	reviewQueued    = "queued"
	reviewRunning   = "running"
	reviewAwaiting  = "awaiting"
	reviewCompleted = "completed"
)

// Review results of a completed review, as the scheduler reads them.
const (
	resultSuccess = "success"
	resultFailure = "failure"
)

const storeSchemaVersion = 1

const storeSchema = `
CREATE TABLE IF NOT EXISTS discovery (
	name  TEXT PRIMARY KEY,
	value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS reviews (
	workflow_id       TEXT PRIMARY KEY,
	review_key        TEXT NOT NULL,
	input             TEXT NOT NULL,
	status            TEXT NOT NULL CHECK (status IN ('queued', 'running', 'awaiting', 'completed')),
	result            TEXT CHECK (result IS NULL OR result IN ('success', 'failure')),
	failure           TEXT,
	released          INTEGER NOT NULL DEFAULT 0,
	cleanup_pending   INTEGER NOT NULL DEFAULT 0,
	approval          TEXT CHECK (approval IS NULL OR approval IN ('approve', 'expired', 'superseded')),
	approval_deadline TEXT,
	outcome           TEXT,
	created_at        TEXT NOT NULL,
	updated_at        TEXT NOT NULL,
	completed_at      TEXT
);
CREATE INDEX IF NOT EXISTS reviews_status ON reviews (status);
CREATE INDEX IF NOT EXISTS reviews_created_at ON reviews (created_at);
CREATE TABLE IF NOT EXISTS review_steps (
	workflow_id TEXT NOT NULL REFERENCES reviews (workflow_id) ON DELETE CASCADE,
	name        TEXT NOT NULL,
	output      TEXT NOT NULL,
	PRIMARY KEY (workflow_id, name)
);
CREATE TABLE IF NOT EXISTS review_state (
	workflow_id TEXT NOT NULL REFERENCES reviews (workflow_id) ON DELETE CASCADE,
	name        TEXT NOT NULL,
	value       TEXT NOT NULL,
	PRIMARY KEY (workflow_id, name)
);
`

var errDiscoveryLockTimeout = errors.New("discovery lock not acquired in time")

// Store keeps the discovery state and every review in one SQLite database.
// Writers serialize on SQLite itself; the discovery state additionally needs
// the discovery lock, because a poll reads it before slow GitHub calls and
// writes it afterwards.
type Store struct {
	db       *sql.DB
	lockPath string
	// discoveryMu serializes discovery critical sections inside one process;
	// the flock on lockPath serializes them across processes.
	discoveryMu sync.Mutex
	now         func() time.Time
}

func storeDSN(path string, readOnly bool) string {
	query := url.Values{}
	query.Add("_pragma", "busy_timeout(10000)")
	query.Add("_pragma", "foreign_keys(1)")
	if readOnly {
		query.Set("mode", "ro")
	} else {
		query.Add("_pragma", "journal_mode(WAL)")
		query.Set("_txlock", "immediate")
	}
	return "file:" + path + "?" + query.Encode()
}

// OpenStore opens the database for reading and writing and creates its schema.
func OpenStore(path string, now func() time.Time) (*Store, error) {
	if !filepath.IsAbs(path) {
		return nil, workflowError("store_path_invalid", true, errors.New("store path must be absolute"))
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, workflowError("store_unavailable", true, err)
	}
	db, err := sql.Open("sqlite", storeDSN(path, false))
	if err != nil {
		return nil, workflowError("store_unavailable", true, err)
	}
	// One connection keeps the per-connection pragmas in effect and makes every
	// transaction of this process strictly sequential.
	db.SetMaxOpenConns(1)
	store := &Store{db: db, lockPath: path + ".discovery.lock", now: now}
	if err := store.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	return store, nil
}

func (store *Store) Close() error { return store.db.Close() }

func (store *Store) migrate() error {
	var version int
	if err := store.db.QueryRow("PRAGMA user_version").Scan(&version); err != nil {
		return workflowError("store_unavailable", true, err)
	}
	if version > storeSchemaVersion {
		return workflowError("store_version_unsupported", true, fmt.Errorf("schema %d is newer than %d", version, storeSchemaVersion))
	}
	if _, err := store.db.Exec(storeSchema); err != nil {
		return workflowError("store_unavailable", true, err)
	}
	if _, err := store.db.Exec(fmt.Sprintf("PRAGMA user_version = %d", storeSchemaVersion)); err != nil {
		return workflowError("store_unavailable", true, err)
	}
	return nil
}

func (store *Store) timestamp() string { return store.now().UTC().Format(time.RFC3339Nano) }

// withDiscoveryLock runs fn while this process holds the discovery lock. It
// waits at most timeout for another process, such as a CLI retry, to finish.
func (store *Store) withDiscoveryLock(ctx context.Context, timeout time.Duration, poll time.Duration, fn func() error) error {
	store.discoveryMu.Lock()
	defer store.discoveryMu.Unlock()
	lock, err := os.OpenFile(store.lockPath, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return workflowError("store_unavailable", true, err)
	}
	defer lock.Close()
	deadline := store.now().Add(timeout)
	for {
		err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			break
		}
		if !errors.Is(err, syscall.EWOULDBLOCK) {
			return workflowError("store_unavailable", true, err)
		}
		if !store.now().Before(deadline) {
			return workflowError("discovery_busy", false, errDiscoveryLockTimeout)
		}
		if err := sleepWithContext(ctx, poll); err != nil {
			return err
		}
	}
	defer syscall.Flock(int(lock.Fd()), syscall.LOCK_UN)
	return fn()
}

// loadDiscovery reads the four discovery maps; missing maps start empty.
func (store *Store) loadDiscovery(ctx context.Context) (discoveryState, error) {
	state := discoveryState{schedule: map[string]schedule.Record{}, published: map[string]PublishedRecord{}, awaiting: map[string]AwaitRecord{}, rejected: map[string]string{}}
	rows, err := store.db.QueryContext(ctx, `SELECT name, value FROM discovery`)
	if err != nil {
		return state, workflowError("store_unavailable", false, err)
	}
	defer rows.Close()
	for rows.Next() {
		var name, value string
		if err := rows.Scan(&name, &value); err != nil {
			return state, workflowError("store_unavailable", false, err)
		}
		var target any
		switch name {
		case stateSchedule:
			target = &state.schedule
		case statePublished:
			target = &state.published
		case stateAwaiting:
			target = &state.awaiting
		case stateRejected:
			target = &state.rejected
		default:
			return state, workflowError("store_state_invalid", true, fmt.Errorf("unknown discovery entry %q", name))
		}
		if err := json.Unmarshal([]byte(value), target); err != nil {
			return state, workflowError("store_state_invalid", true, fmt.Errorf("discovery entry %s: %w", name, err))
		}
	}
	if err := rows.Err(); err != nil {
		return state, workflowError("store_unavailable", false, err)
	}
	if state.schedule == nil {
		state.schedule = map[string]schedule.Record{}
	}
	if state.published == nil {
		state.published = map[string]PublishedRecord{}
	}
	if state.awaiting == nil {
		state.awaiting = map[string]AwaitRecord{}
	}
	if state.rejected == nil {
		state.rejected = map[string]string{}
	}
	return state, nil
}

func saveDiscovery(ctx context.Context, tx *sql.Tx, state discoveryState) error {
	for name, value := range map[string]any{stateSchedule: state.schedule, statePublished: state.published, stateAwaiting: state.awaiting, stateRejected: state.rejected} {
		encoded, err := json.Marshal(value)
		if err != nil {
			return workflowError("store_state_invalid", true, err)
		}
		if _, err := tx.ExecContext(ctx, `INSERT INTO discovery (name, value) VALUES (?, ?) ON CONFLICT (name) DO UPDATE SET value = excluded.value`, name, string(encoded)); err != nil {
			return workflowError("store_unavailable", false, err)
		}
	}
	return nil
}

// updateDiscovery loads the discovery state, lets change modify it and writes
// it back in one transaction together with any extra writes of change. The
// caller holds the discovery lock.
func (store *Store) updateDiscovery(ctx context.Context, change func(state discoveryState, tx *sql.Tx) error) error {
	state, err := store.loadDiscovery(ctx)
	if err != nil {
		return err
	}
	return store.inTx(ctx, func(tx *sql.Tx) error {
		if err := change(state, tx); err != nil {
			return err
		}
		return saveDiscovery(ctx, tx, state)
	})
}

func (store *Store) inTx(ctx context.Context, fn func(*sql.Tx) error) error {
	tx, err := store.db.BeginTx(ctx, nil)
	if err != nil {
		return workflowError("store_unavailable", false, err)
	}
	if err := fn(tx); err != nil {
		tx.Rollback()
		return err
	}
	if err := tx.Commit(); err != nil {
		return workflowError("store_unavailable", false, err)
	}
	return nil
}

// enqueueReview records a dispatched review. A repeated dispatch of the same
// workflow ID keeps the existing review, as a deterministic ID promises.
func (store *Store) enqueueReview(ctx context.Context, tx *sql.Tx, workflowID string, input ReviewInput) error {
	encoded, err := json.Marshal(input)
	if err != nil {
		return workflowError("review_input_invalid", true, err)
	}
	now := store.timestamp()
	if _, err := tx.ExecContext(ctx, `INSERT INTO reviews (workflow_id, review_key, input, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (workflow_id) DO NOTHING`,
		workflowID, input.PullRequest.Key(), string(encoded), reviewQueued, now, now); err != nil {
		return workflowError("store_unavailable", false, err)
	}
	return nil
}

// StoredReview is one review row.
type StoredReview struct {
	WorkflowID       string      `json:"workflow_id"`
	Key              string      `json:"key"`
	Input            ReviewInput `json:"-"`
	Status           string      `json:"status"`
	Result           string      `json:"result,omitempty"`
	Failure          string      `json:"failure,omitempty"`
	Released         bool        `json:"released"`
	CleanupPending   bool        `json:"cleanup_pending"`
	Approval         string      `json:"approval,omitempty"`
	ApprovalDeadline string      `json:"approval_deadline,omitempty"`
	Outcome          string      `json:"outcome,omitempty"`
	CreatedAt        string      `json:"created_at"`
	UpdatedAt        string      `json:"updated_at"`
	CompletedAt      string      `json:"completed_at,omitempty"`
}

const reviewColumns = `workflow_id, review_key, input, status, result, failure, released, cleanup_pending, approval, approval_deadline, outcome, created_at, updated_at, completed_at`

type rowScanner interface{ Scan(...any) error }

func scanReview(row rowScanner) (StoredReview, error) {
	var review StoredReview
	var input string
	var result, failure, approval, deadline, outcome, completed sql.NullString
	if err := row.Scan(&review.WorkflowID, &review.Key, &input, &review.Status, &result, &failure, &review.Released, &review.CleanupPending, &approval, &deadline, &outcome, &review.CreatedAt, &review.UpdatedAt, &completed); err != nil {
		return StoredReview{}, err
	}
	if err := json.Unmarshal([]byte(input), &review.Input); err != nil {
		return StoredReview{}, workflowError("store_state_invalid", true, fmt.Errorf("review %s input: %w", review.WorkflowID, err))
	}
	review.Result, review.Failure, review.Approval = result.String, failure.String, approval.String
	review.ApprovalDeadline, review.Outcome, review.CompletedAt = deadline.String, outcome.String, completed.String
	return review, nil
}

func (store *Store) getReview(ctx context.Context, workflowID string) (StoredReview, bool, error) {
	review, err := scanReview(store.db.QueryRowContext(ctx, `SELECT `+reviewColumns+` FROM reviews WHERE workflow_id = ?`, workflowID))
	if errors.Is(err, sql.ErrNoRows) {
		return StoredReview{}, false, nil
	}
	if err != nil {
		return StoredReview{}, false, workflowError("store_unavailable", false, err)
	}
	return review, true, nil
}

// listReviews returns the newest reviews first; filter is an optional SQL
// condition over the review columns with its arguments.
func (store *Store) listReviews(ctx context.Context, filter string, limit int, args ...any) ([]StoredReview, error) {
	query := `SELECT ` + reviewColumns + ` FROM reviews`
	if filter != "" {
		query += ` WHERE ` + filter
	}
	query += ` ORDER BY created_at DESC, workflow_id LIMIT ?`
	rows, err := store.db.QueryContext(ctx, query, append(args, limit)...)
	if err != nil {
		return nil, workflowError("store_unavailable", false, err)
	}
	defer rows.Close()
	reviews := []StoredReview{}
	for rows.Next() {
		review, err := scanReview(rows)
		if err != nil {
			return nil, err
		}
		reviews = append(reviews, review)
	}
	if err := rows.Err(); err != nil {
		return nil, workflowError("store_unavailable", false, err)
	}
	return reviews, nil
}

// runHistory returns the newest reviews in the shape the scheduler plans with.
// A history larger than the limit fails closed, so the scheduler never
// dispatches over a review it cannot see.
func (store *Store) runHistory(ctx context.Context, limit int) ([]schedule.Run, error) {
	reviews, err := store.listReviews(ctx, "", limit+1)
	if err != nil {
		return nil, err
	}
	if len(reviews) > limit {
		return nil, workflowError("review_history_limit_exceeded", true, nil)
	}
	runs := make([]schedule.Run, 0, len(reviews))
	for _, review := range reviews {
		status := "running"
		if review.Status == reviewCompleted {
			status = "completed"
		}
		runs = append(runs, schedule.Run{
			Target: review.Input.PullRequest.Target(), WorkflowID: review.WorkflowID, Attempt: review.Input.Attempt,
			Status: status, Result: review.Result, Failure: review.Failure, Released: review.Released, CleanupPending: review.CleanupPending,
		})
	}
	return runs, nil
}

// readyReviews returns the reviews that can make progress, oldest first:
// queued and running ones, and waits with an approval decision.
func (store *Store) readyReviews(ctx context.Context) ([]StoredReview, error) {
	rows, err := store.db.QueryContext(ctx, `SELECT `+reviewColumns+` FROM reviews WHERE status IN (?, ?) OR (status = ? AND approval IS NOT NULL) ORDER BY created_at, workflow_id`,
		reviewQueued, reviewRunning, reviewAwaiting)
	if err != nil {
		return nil, workflowError("store_unavailable", false, err)
	}
	defer rows.Close()
	reviews := []StoredReview{}
	for rows.Next() {
		review, err := scanReview(rows)
		if err != nil {
			return nil, err
		}
		reviews = append(reviews, review)
	}
	if err := rows.Err(); err != nil {
		return nil, workflowError("store_unavailable", false, err)
	}
	return reviews, nil
}

// setReviewStatus moves a review between non-final statuses.
func (store *Store) setReviewStatus(ctx context.Context, workflowID, from, to string) (bool, error) {
	outcome, err := store.db.ExecContext(ctx, `UPDATE reviews SET status = ?, updated_at = ? WHERE workflow_id = ? AND status = ?`, to, store.timestamp(), workflowID, from)
	if err != nil {
		return false, workflowError("store_unavailable", false, err)
	}
	rows, err := outcome.RowsAffected()
	return rows == 1, err
}

func (store *Store) setReviewFlags(ctx context.Context, workflowID string, released, cleanupPending *bool) error {
	if released != nil {
		if _, err := store.db.ExecContext(ctx, `UPDATE reviews SET released = ?, updated_at = ? WHERE workflow_id = ?`, *released, store.timestamp(), workflowID); err != nil {
			return workflowError("store_unavailable", false, err)
		}
	}
	if cleanupPending != nil {
		if _, err := store.db.ExecContext(ctx, `UPDATE reviews SET cleanup_pending = ?, updated_at = ? WHERE workflow_id = ?`, *cleanupPending, store.timestamp(), workflowID); err != nil {
			return workflowError("store_unavailable", false, err)
		}
	}
	return nil
}

// startAwaiting records the approval deadline once and marks the review as
// waiting; a resumed review keeps its first deadline.
func (store *Store) startAwaiting(ctx context.Context, workflowID string, deadline time.Time) error {
	if _, err := store.db.ExecContext(ctx, `UPDATE reviews SET status = ?, released = 1, approval_deadline = COALESCE(approval_deadline, ?), updated_at = ? WHERE workflow_id = ?`,
		reviewAwaiting, deadline.UTC().Format(time.RFC3339Nano), store.timestamp(), workflowID); err != nil {
		return workflowError("store_unavailable", false, err)
	}
	return nil
}

// decideApproval sets the approval decision unless one is already set, so an
// approval, a supersede and the deadline resolve the wait exactly once. It
// returns the decision in effect afterwards.
func (store *Store) decideApproval(ctx context.Context, workflowID, decision string) (string, error) {
	var current string
	err := store.inTx(ctx, func(tx *sql.Tx) error {
		var err error
		current, err = store.decideApprovalTx(ctx, tx, workflowID, decision)
		return err
	})
	return current, err
}

// decideApprovalTx is decideApproval inside a caller's transaction. Callers
// holding a transaction must not use store.db, which has one connection.
func (store *Store) decideApprovalTx(ctx context.Context, tx *sql.Tx, workflowID, decision string) (string, error) {
	if _, err := tx.ExecContext(ctx, `UPDATE reviews SET approval = ?, updated_at = ? WHERE workflow_id = ? AND approval IS NULL`, decision, store.timestamp(), workflowID); err != nil {
		return "", workflowError("store_unavailable", false, err)
	}
	var current sql.NullString
	err := tx.QueryRowContext(ctx, `SELECT approval FROM reviews WHERE workflow_id = ?`, workflowID).Scan(&current)
	if errors.Is(err, sql.ErrNoRows) {
		return "", nil
	}
	if err != nil {
		return "", workflowError("store_unavailable", false, err)
	}
	return current.String, nil
}

// completeReview records the final result of a review.
func (store *Store) completeReview(ctx context.Context, workflowID, result, failure string, outcome *ReviewOutcome) error {
	var encoded sql.NullString
	if outcome != nil {
		value, err := json.Marshal(outcome)
		if err != nil {
			return workflowError("store_state_invalid", true, err)
		}
		encoded = sql.NullString{String: string(value), Valid: true}
	}
	var failureValue sql.NullString
	if failure != "" {
		failureValue = sql.NullString{String: failure, Valid: true}
	}
	now := store.timestamp()
	if _, err := store.db.ExecContext(ctx, `UPDATE reviews SET status = ?, result = ?, failure = ?, outcome = ?, completed_at = ?, updated_at = ? WHERE workflow_id = ?`,
		reviewCompleted, result, failureValue, encoded, now, now, workflowID); err != nil {
		return workflowError("store_unavailable", false, err)
	}
	return nil
}

// pruneReviews removes completed reviews not updated since the cutoff.
func (store *Store) pruneReviews(ctx context.Context, cutoff time.Time) (int64, error) {
	outcome, err := store.db.ExecContext(ctx, `DELETE FROM reviews WHERE status = ? AND updated_at < ?`, reviewCompleted, cutoff.UTC().Format(time.RFC3339Nano))
	if err != nil {
		return 0, workflowError("store_unavailable", false, err)
	}
	return outcome.RowsAffected()
}

func (store *Store) loadStep(ctx context.Context, workflowID, name string, target any) (bool, error) {
	var output string
	err := store.db.QueryRowContext(ctx, `SELECT output FROM review_steps WHERE workflow_id = ? AND name = ?`, workflowID, name).Scan(&output)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, workflowError("store_unavailable", false, err)
	}
	if err := json.Unmarshal([]byte(output), target); err != nil {
		return false, workflowError("store_state_invalid", true, fmt.Errorf("step %s: %w", name, err))
	}
	return true, nil
}

func (store *Store) saveStep(ctx context.Context, workflowID, name string, value any) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return workflowError("store_state_invalid", true, err)
	}
	// A step output is deterministic for its review, so writing it again after
	// an interrupted resume replaces it with the same value.
	if _, err := store.db.ExecContext(ctx, `INSERT INTO review_steps (workflow_id, name, output) VALUES (?, ?, ?) ON CONFLICT (workflow_id, name) DO UPDATE SET output = excluded.output`, workflowID, name, string(encoded)); err != nil {
		return workflowError("store_unavailable", false, err)
	}
	return nil
}

func (store *Store) getState(ctx context.Context, workflowID, name string, target any) (bool, error) {
	var value string
	err := store.db.QueryRowContext(ctx, `SELECT value FROM review_state WHERE workflow_id = ? AND name = ?`, workflowID, name).Scan(&value)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, workflowError("store_unavailable", false, err)
	}
	if err := json.Unmarshal([]byte(value), target); err != nil {
		return false, workflowError("store_state_invalid", true, fmt.Errorf("state %s: %w", name, err))
	}
	return true, nil
}

func (store *Store) setState(ctx context.Context, workflowID, name string, value any) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return workflowError("store_state_invalid", true, err)
	}
	if _, err := store.db.ExecContext(ctx, `INSERT INTO review_state (workflow_id, name, value) VALUES (?, ?, ?) ON CONFLICT (workflow_id, name) DO UPDATE SET value = excluded.value`, workflowID, name, string(encoded)); err != nil {
		return workflowError("store_unavailable", false, err)
	}
	return nil
}

// backupTo writes a consistent copy of the database to path.
func (store *Store) backupTo(ctx context.Context, path string) error {
	if !filepath.IsAbs(path) {
		return workflowError("backup_path_invalid", true, nil)
	}
	if _, err := os.Stat(path); err == nil {
		return workflowError("backup_path_exists", true, nil)
	}
	if _, err := store.db.ExecContext(ctx, `VACUUM INTO ?`, path); err != nil {
		return workflowError("backup_failed", true, err)
	}
	return nil
}
