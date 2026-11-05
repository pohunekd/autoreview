package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/url"
	"strings"

	"github.com/pohunekd/autoreview/policy"
)

// Notify posts a message to the Matrix room of its level; only action messages
// mention the user. A digest or log message the homeserver cannot take is
// dropped with a warning, so a muted notification never fails a review.
func (external *External) Notify(ctx context.Context, level policy.Level, message string) error {
	err := external.sendMatrix(ctx, level, message)
	if err != nil && level != policy.LevelAction {
		if code := errorCode(err); code == "matrix_unavailable" || code == "matrix_request_rejected" {
			slog.WarnContext(ctx, "matrix notification dropped", "level", string(level), "error_code", code)
			return nil
		}
	}
	return err
}

// matrixTransactionID is random per send. A deterministic ID would make the
// homeserver drop a legitimate repeat of an identical message.
func matrixTransactionID() (string, error) {
	random := make([]byte, 16)
	if _, err := rand.Read(random); err != nil {
		return "", err
	}
	return "notify-" + hex.EncodeToString(random), nil
}

func (external *External) sendMatrix(ctx context.Context, level policy.Level, message string) error {
	room, ok := external.cfg.MatrixRooms[level]
	if !ok {
		return workflowError("matrix_level_invalid", true, nil)
	}
	if strings.TrimSpace(message) == "" {
		return workflowError("matrix_message_invalid", true, nil)
	}
	if runes := []rune(message); len(runes) > external.cfg.MatrixMaxCharacters {
		message = string(runes[:external.cfg.MatrixMaxCharacters-1]) + "…"
	}
	mentions := map[string]any{}
	if level == policy.LevelAction {
		mentions["user_ids"] = []string{external.cfg.MatrixMentionUser}
	}
	transaction, err := matrixTransactionID()
	if err != nil {
		return workflowError("matrix_request_invalid", true, err)
	}
	return external.putMatrixMessage(ctx, room, transaction, map[string]any{"msgtype": "m.text", "body": message, "m.mentions": mentions})
}

// putMatrixMessage sends one m.room.message event. The homeserver answers a
// repeated transaction ID with the event of the first send, so a retry of the
// same ID cannot post the message twice.
func (external *External) putMatrixMessage(ctx context.Context, room, transaction string, content map[string]any) error {
	payload, err := json.Marshal(content)
	if err != nil {
		return workflowError("matrix_request_invalid", true, err)
	}
	ctx, cancel := context.WithTimeout(ctx, external.cfg.MatrixTimeout)
	defer cancel()
	endpoint := strings.TrimRight(external.cfg.MatrixHomeserverURL, "/") + "/_matrix/client/v3/rooms/" + url.PathEscape(room) + "/send/m.room.message/" + url.PathEscape(transaction)
	request, err := http.NewRequestWithContext(ctx, http.MethodPut, endpoint, bytes.NewReader(payload))
	if err != nil {
		return workflowError("matrix_request_invalid", true, err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Authorization", "Bearer "+external.cfg.MatrixAccessToken)
	response, err := external.httpClient.Do(request)
	if err != nil {
		return workflowError("matrix_unavailable", false, errors.New("request failed"))
	}
	defer response.Body.Close()
	if response.StatusCode/100 != 2 {
		if response.StatusCode == http.StatusTooManyRequests || response.StatusCode/100 == 5 {
			return workflowError("matrix_unavailable", false, nil)
		}
		return workflowError("matrix_request_rejected", true, nil)
	}
	return nil
}

// SendQueueDigest posts the review queue to its own room. A notice does not
// notify by default, and the empty mentions object stops mentions from
// notifying anyone. The caller chooses the transaction ID.
func (external *External) SendQueueDigest(ctx context.Context, transaction, plain, formatted string) error {
	content := map[string]any{
		"msgtype":        "m.notice",
		"body":           plain,
		"format":         "org.matrix.custom.html",
		"formatted_body": formatted,
		"m.mentions":     map[string]any{},
	}
	return external.putMatrixMessage(ctx, external.cfg.QueueMatrixRoom, transaction, content)
}
