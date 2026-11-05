package main

import (
	"context"
	"errors"
	"fmt"
	"time"
)

func sleepWithContext(ctx context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

// commandErrorResponse is the JSON a caller reads from a failed
// command. It carries only stable codes, never raw causes.
type commandErrorResponse struct {
	Error         string `json:"error"`
	Message       string `json:"message"`
	Configuration string `json:"configuration,omitempty"`
	Retryable     bool   `json:"retryable"`
	WakeAgent     bool   `json:"wakeAgent"`
}

func commandErrorResponseFor(err error) commandErrorResponse {
	code := errorCode(err)
	response := commandErrorResponse{Error: code, Message: fmt.Sprintf("PR review command failed: %s.", code)}
	switch code {
	case "command_invalid":
		response.Message = "PR review command arguments are invalid. " + usage
	case "discovery_busy":
		response.Message = "Another discovery pass holds the lock. Repeat the command shortly."
		response.Retryable = true
	}
	var config *ConfigError
	if errors.As(err, &config) {
		response.Error, response.Configuration = config.Code, config.Name
		response.Message = fmt.Sprintf("Configuration is invalid or missing: %s.", config.Name)
	}
	return response
}
