---
type: Guide
id: guide/notifiers
title: Notifiers
description: Configure where autoreview messages go - no notifier, Matrix rooms per level, webhooks for Slack, Discord or plain JSON, several backends per level, the review queue and delivery-failure semantics.
source_kind: manual
intents: [setup, help, debug]
---

# Notifiers

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Every event in the policy has one of three levels: `action`, `digest` or
`log`. The notification section of `config.yaml` says where each level goes.
A level is routed to one or more named backends, and a backend is one
endpoint of one type. The policy decides *what* is worth a message; this guide
covers *where* the message goes.

The event list and the meaning of each level are in
[policy notifications](../../policy.md#notifications). The configuration keys
are in [configuration notify](../../configuration.md#notify).

## How routing works

```yaml
notify:
  timeout_seconds: 30          # one delivery
  max_characters: 8000         # longer messages are cut
  backends:
    - name: log
      type: none
  routes:
    action: [log]              # each level: a list of backend names
    digest: [log]
    log: [log]
  review_queue:
    backends: [log]
    max_items: 50
```

- `backends` defines named endpoints. Names are used in the routes, and the
  same type may appear several times under different names.
- `routes` maps each level to a list of backend names. Every backend in the
  list receives the message of that level.
- `review_queue.backends` lists the backends of the review queue, which is a
  separate message and not an event.

A message goes only to the backends of its own level. Keep levels in separate
backends when they must not share a room.

## No notifier

```yaml
backends:
  - name: log
    type: none
routes:
  action: [log]
  digest: [log]
  log: [log]
review_queue:
  backends: [log]
  max_items: 50
```

Nothing leaves the host, and the structured log is the only record. Use this
while you evaluate autoreview. A clean review that waits for a human is then
visible only through `autoreview awaiting`, so pair `human-approval` setups
with a real backend.

## Matrix, one room per level

A common layout uses two rooms: one that notifies a person, and one quiet room
for results. The log stays on the `log` backend.

```yaml
notify:
  timeout_seconds: 30
  max_characters: 8000
  backends:
    - name: matrix-action
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_ACTION_TOKEN
      room: "!actionroom:example.org"
      mention_user: "@you:example.org"
    - name: matrix-digest
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_DIGEST_TOKEN
      room: "!digestroom:example.org"
      mention_user: "@you:example.org"
    - name: log
      type: none
  routes:
    action: [matrix-action]
    digest: [matrix-digest]
    log: [log]
  review_queue:
    backends: [matrix-digest]
    max_items: 50
```

- `mention_user` is used only in `action` messages. Setting it on the digest
  backend changes nothing, because digests never mention anyone.
- The homeserver URL must use HTTPS. Plain HTTP is accepted only on loopback.
- Messages are `m.text`. The review queue is an `m.notice` with empty
  mentions, so it never notifies anyone, even in the action room.
- The token is read from the variable named in `access_token_env`. The name
  must start with `AUTOREVIEW_`; the value never goes into the file.

Create a bot user for autoreview, invite it to the rooms and give it the
token. Do not reuse a personal access token for it.

## Webhook for Slack, Discord or JSON

```yaml
notify:
  timeout_seconds: 30
  max_characters: 8000
  backends:
    - name: team-chat
      type: webhook
      url_env: AUTOREVIEW_WEBHOOK_URL
      format: slack            # json, slack or discord
    - name: log
      type: none
  routes:
    action: [team-chat]
    digest: [team-chat]
    log: [log]
  review_queue:
    backends: [team-chat]
    max_items: 50
```

One JSON body is POSTed per message:

- `json` is autoreview's own message object: level, event, title, text,
  review id and links. Use it for your own receiver.
- `slack` and `discord` wrap the text in the incoming-webhook format of that
  service.

The webhook URL is a secret for most services. It comes from the variable
named in `url_env` and is never written into `config.yaml`. Check that it is
set without printing it:

```sh
test -n "$AUTOREVIEW_WEBHOOK_URL" && echo set
```

## Several backends for one level

A level may list two backends. For example, `action` can reach a Matrix room
and a Slack channel, so one unavailable service does not hide a required
action:

```yaml
routes:
  action: [matrix-action, team-chat]
  digest: [matrix-digest]
  log: [log]
```

Every backend in the list gets the message. Each one has its own `timeout_seconds`
budget, so a slow backend delays only its own delivery.

## Review queue

`autoreview review-queue` lists every open pull request that requests a review
from the login of one of its credentials and whose engine state is known, including
those that wait on the author. The list is grouped by account when the credentials
have different logins. It sends the list to `review_queue.backends` and prints it. The
list holds at most `review_queue.max_items` pull requests.

The queue is a quiet notice, not an event: when no request waits, nothing is
sent. Run it from a timer, for example an hourly systemd timer, and keep its
backends quiet unless you want the reminder to notify someone every hour.

```sh
autoreview review-queue
```

## Delivery failures

Failure handling depends on the level of the event:

| Level | Delivery fails | Effect |
|---|---|---|
| `action` | The backend does not accept the message | The step fails and is retried. Otherwise nobody would learn that an action is needed |
| `digest` | Dropped with a warning in the log | The review and its publication are unaffected |
| `log` | Dropped with a warning in the log | Nothing else changes |

Because a failed `action` delivery fails the step, a broken action backend
blocks reviews that need a human. Watch for `matrix_unavailable`,
`matrix_request_rejected`, `webhook_unavailable` and `webhook_request_rejected`
in the logs; the [CLI error codes](../../cli.md#error-codes) list them.

`autoreview validate` checks the notification configuration, not whether a
backend is reachable. After you change a backend, confirm delivery with a real
event, for example a review that reaches `approval_requested` on a
`human-approval` rule.

## Choosing a layout

| Need | Layout |
|---|---|
| Try it out | `none` for every level |
| Team chat with a mention | Matrix or Slack for `action`, `digest` in a quiet room |
| Own dashboard | `webhook` with `format: json` for every level |
| An action must not be missed | Two backends for `action` |
| No notifications | `none`, and check `autoreview awaiting` yourself |

For how the level of each event is chosen, see
[policy notifications](../../policy.md#notifications). The model behind
backends is in [notifications](../concepts/notifications.md).
