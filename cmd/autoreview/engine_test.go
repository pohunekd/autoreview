package main

import (
	"context"
	"io"
	"log/slog"
	"testing"
	"time"
)

// fakeClock advances only when the fake sleep is called.
type fakeClock struct{ now time.Time }

func (c *fakeClock) Now() time.Time { return c.now }

func (c *fakeClock) Sleep(_ context.Context, delay time.Duration) error {
	c.now = c.now.Add(delay)
	return nil
}

func TestRetryStepStopsOnPermanentErrors(t *testing.T) {
	clock := &fakeClock{now: time.Unix(0, 0)}
	retry := stepRetryPolicy{For: time.Second, Initial: 100 * time.Millisecond, Max: 400 * time.Millisecond}
	calls := 0
	_, err := retryStep(context.Background(), retry, clock.Now, clock.Sleep, func(context.Context) (int, error) {
		calls++
		return 0, workflowError("review_identity_invalid", true, nil)
	})
	if errorCode(err) != "review_identity_invalid" || calls != 1 || clock.now != time.Unix(0, 0) {
		t.Fatalf("calls = %d, elapsed %s, err = %v", calls, clock.now.Sub(time.Unix(0, 0)), err)
	}
}

// Transient failures are retried with doubling delays until the next delay
// would end past the total duration: 100ms, 200ms and 400ms fit in one second,
// the following 400ms does not.
func TestRetryStepRespectsItsTotalDuration(t *testing.T) {
	start := time.Unix(0, 0)
	clock := &fakeClock{now: start}
	retry := stepRetryPolicy{For: time.Second, Initial: 100 * time.Millisecond, Max: 400 * time.Millisecond}
	calls := 0
	_, err := retryStep(context.Background(), retry, clock.Now, clock.Sleep, func(context.Context) (int, error) {
		calls++
		return 0, workflowError("github_request_failed", false, nil)
	})
	if errorCode(err) != "github_request_failed" || calls != 4 || clock.now.Sub(start) != 700*time.Millisecond {
		t.Fatalf("calls = %d, elapsed %s, err = %v", calls, clock.now.Sub(start), err)
	}
}

func TestRetryStepReturnsTheFirstSuccess(t *testing.T) {
	clock := &fakeClock{now: time.Unix(0, 0)}
	retry := stepRetryPolicy{For: time.Second, Initial: 100 * time.Millisecond, Max: 400 * time.Millisecond}
	calls := 0
	value, err := retryStep(context.Background(), retry, clock.Now, clock.Sleep, func(context.Context) (string, error) {
		calls++
		if calls < 3 {
			return "", workflowError("github_request_failed", false, nil)
		}
		return "found", nil
	})
	if err != nil || value != "found" || calls != 3 {
		t.Fatalf("value = %q, calls = %d, err = %v", value, calls, err)
	}
}

// A second engine on the same store is refused while the first one serves.
func TestSecondEngineOnTheSameStoreIsRefused(t *testing.T) {
	h := startHarness(t, nil)
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	second := NewEngine(h.cfg, h.store, h.fake, logger, time.Now, sleepWithContext)
	if err := second.Serve(context.Background()); errorCode(err) != "engine_already_running" {
		t.Fatalf("second engine error = %v", err)
	}
}
