package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"sync"
	"syscall"
	"time"
)

// Engine discovers pull requests, runs their reviews and resumes interrupted
// ones. All durable state lives in the Store.
type Engine struct {
	cfg         Config
	store       *Store
	integration Integrations
	logger      *slog.Logger
	now         func() time.Time
	sleep       func(context.Context, time.Duration) error

	mu     sync.Mutex
	active map[string]bool
	wg     sync.WaitGroup
}

func NewEngine(cfg Config, store *Store, integration Integrations, logger *slog.Logger, now func() time.Time, sleep func(context.Context, time.Duration) error) *Engine {
	return &Engine{cfg: cfg, store: store, integration: integration, logger: logger, now: now, sleep: sleep, active: map[string]bool{}}
}

type stepRetryPolicy struct {
	For     time.Duration
	Initial time.Duration
	Max     time.Duration
}

// retryStep runs fn until it succeeds, fails permanently, the context ends or
// retrying would exceed the policy's total duration. The delay doubles up to
// the policy maximum.
func retryStep[T any](ctx context.Context, policy stepRetryPolicy, now func() time.Time, sleep func(context.Context, time.Duration) error, fn func(context.Context) (T, error)) (T, error) {
	start := now()
	delay := policy.Initial
	for {
		value, err := fn(ctx)
		if err == nil || ctx.Err() != nil {
			return value, err
		}
		var typed *WorkflowError
		if errors.As(err, &typed) && typed.Permanent {
			return value, err
		}
		if now().Sub(start)+delay > policy.For {
			return value, err
		}
		if sleepErr := sleep(ctx, delay); sleepErr != nil {
			return value, errors.Join(err, sleepErr)
		}
		delay = min(delay*2, policy.Max)
	}
}

func (engine *Engine) stepPolicy() stepRetryPolicy {
	return stepRetryPolicy{For: engine.cfg.StepRetry, Initial: engine.cfg.StepRetryInitial, Max: engine.cfg.StepRetryMaxInterval}
}

// Serve runs discovery and reviews until ctx ends. A second engine on the
// same database is refused. Reviews still running at shutdown keep their
// status and resume on the next start.
func (engine *Engine) Serve(ctx context.Context) error {
	lock, err := os.OpenFile(engine.store.lockPath+".engine", os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return workflowError("store_unavailable", true, err)
	}
	defer lock.Close()
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		return workflowError("engine_already_running", true, err)
	}
	reviewCtx, cancelReviews := context.WithCancel(ctx)
	defer func() {
		cancelReviews()
		engine.wg.Wait()
	}()
	pollTicker := time.NewTicker(engine.cfg.PollInterval)
	defer pollTicker.Stop()
	workTicker := time.NewTicker(engine.cfg.WorkerTick)
	defer workTicker.Stop()
	engine.logger.Info("pull request review engine starting", "policy_digest", engine.cfg.Policy.Digest(), "max_concurrent_reviews", engine.cfg.MaxConcurrentReviews)
	engine.pollAndLog(ctx)
	engine.work(reviewCtx)
	for {
		select {
		case <-ctx.Done():
			engine.logger.Info("pull request review engine stopping")
			return nil
		case <-pollTicker.C:
			engine.pollAndLog(ctx)
			engine.prune(ctx)
		case <-workTicker.C:
			engine.work(reviewCtx)
		}
	}
}

func (engine *Engine) pollAndLog(ctx context.Context) {
	result, err := engine.Poll(ctx)
	if err != nil {
		if ctx.Err() == nil {
			engine.logger.Error("discovery poll failed", "error_code", errorCode(err), "error", err)
		}
		return
	}
	if result.Dispatched > 0 || result.Superseded > 0 {
		engine.logger.Info("discovery poll", "open", result.Open, "dispatched", result.Dispatched, "superseded", result.Superseded)
	}
}

func (engine *Engine) prune(ctx context.Context) {
	if removed, err := engine.store.pruneReviews(ctx, engine.now().Add(-engine.cfg.ReviewRetention)); err != nil {
		engine.logger.Error("review pruning failed", "error_code", errorCode(err))
	} else if removed > 0 {
		engine.logger.Info("completed reviews pruned", "reviews", removed)
	}
}

// work expires approval waits past their deadline and starts every review
// that can make progress, up to the concurrency limit.
func (engine *Engine) work(ctx context.Context) {
	if ctx.Err() != nil {
		return
	}
	if err := engine.expireApprovals(ctx); err != nil {
		engine.logger.Error("approval expiry failed", "error_code", errorCode(err))
	}
	ready, err := engine.store.readyReviews(ctx)
	if err != nil {
		engine.logger.Error("review queue unreadable", "error_code", errorCode(err))
		return
	}
	for _, stored := range ready {
		if !engine.claim(stored.WorkflowID) {
			continue
		}
		if stored.Status != reviewRunning {
			moved, err := engine.store.setReviewStatus(ctx, stored.WorkflowID, stored.Status, reviewRunning)
			if err != nil || !moved {
				engine.release(stored.WorkflowID)
				continue
			}
			stored.Status = reviewRunning
		}
		engine.wg.Add(1)
		go func(stored StoredReview) {
			defer engine.wg.Done()
			defer engine.release(stored.WorkflowID)
			err := engine.runReview(ctx, stored)
			switch {
			case err == nil, errors.Is(err, errShutdown), ctx.Err() != nil:
			default:
				// The review keeps its status, so the next tick resumes it.
				engine.logger.Error("review interrupted", "review_id", stored.Input.PullRequest.ReviewID(), "workflow_id", stored.WorkflowID, "error_code", errorCode(err), "error", err)
			}
		}(stored)
	}
}

// claim marks a review as handled by this engine, unless it already is or
// the concurrency limit is reached.
func (engine *Engine) claim(workflowID string) bool {
	engine.mu.Lock()
	defer engine.mu.Unlock()
	if engine.active[workflowID] || len(engine.active) >= engine.cfg.MaxConcurrentReviews {
		return false
	}
	engine.active[workflowID] = true
	return true
}

func (engine *Engine) release(workflowID string) {
	engine.mu.Lock()
	defer engine.mu.Unlock()
	delete(engine.active, workflowID)
}

func (engine *Engine) expireApprovals(ctx context.Context) error {
	waiting, err := engine.store.listReviews(ctx, "status = ? AND approval IS NULL", engine.cfg.StateEntryLimit, reviewAwaiting)
	if err != nil {
		return err
	}
	now := engine.now()
	for _, review := range waiting {
		deadline, err := time.Parse(time.RFC3339Nano, review.ApprovalDeadline)
		if err != nil {
			return workflowError("store_state_invalid", true, fmt.Errorf("review %s deadline: %w", review.WorkflowID, err))
		}
		if now.Before(deadline) {
			continue
		}
		if _, err := engine.store.decideApproval(ctx, review.WorkflowID, approvalKindExpired); err != nil {
			return err
		}
	}
	return nil
}
