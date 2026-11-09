---
type: Guide
id: guide/notifiers
title: Notifiers
description: Configure where autoreview messages go - Matrix rooms per level, the review queue and delivery-failure semantics; no notifier, webhooks and several backends per level are planned.
source_kind: manual
intents: [setup, help, debug]
---

# Notifiers

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Every event in the policy has one of three levels: `action`, `digest` or
`log`. The notification section of `config.yaml` says where each level goes.
A level is routed to one named backend, and a backend is one
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
    - name: matrix
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_TOKEN
      room: "!roomid:example.org"
      mention_user: "@you:example.org"
  routes:
    action: [matrix]           # each level: a list of one backend name
    digest: [matrix]
    log: [matrix]
  review_queue:
    backends: [matrix]
    max_items: 50
```

- `backends` defines named endpoints. Names are used in the routes, and the
  same type may appear several times under different names.
- `routes` maps each level to a list of exactly one backend name. Several
  backends per level are planned in #27.
- `review_queue.backends` lists the backends of the review queue, which is a
  separate message and not an event.

A message goes only to the backends of its own level. Keep levels in separate
backends when they must not share a room.

## No notifier

Planned in #29. Until then every route and the review queue need a `matrix`
backend. A route to a `none` backend is refused at load with
`configuration_invalid: config: invalid Matrix <level> room`. A
`review_queue.backends` list of `none` backends only is refused too, see
[configuration](../../configuration.md#backend-none).

## Matrix, one room per level

A common layout uses two rooms: one that notifies a person, and one quiet room
for results. The `log` level shares the quiet room.

```yaml
notify:
  timeout_seconds: 30
  max_characters: 8000
  backends:
    - name: matrix-action
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_TOKEN
      room: "!actionroom:example.org"
      mention_user: "@you:example.org"
    - name: matrix-digest
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_TOKEN
      room: "!digestroom:example.org"
      mention_user: "@you:example.org"
  routes:
    action: [matrix-action]
    digest: [matrix-digest]
    log: [matrix-digest]
  review_queue:
    backends: [matrix-digest]
    max_items: 50
```

- Until M3, every Matrix backend that a level or the review queue reaches must
  share `homeserver_url`, `access_token_env` and `mention_user`; only `room`
  differs. Otherwise `config.yaml` is refused with `configuration_invalid` and
  the first route key whose backend differs from an earlier one.
- `mention_user` is used only in `action` messages, because digests never
  mention anyone.
- The homeserver URL must use HTTPS. Plain HTTP is accepted only on loopback.
- Messages are `m.text`. The review queue is an `m.notice` with empty
  mentions, so it never notifies anyone, even in the action room.
- The token is read from the variable named in `access_token_env`. The name
  must start with `AUTOREVIEW_`; the value never goes into the file.

Create a bot user for autoreview, invite it to the rooms and give it the
token. Do not reuse a personal access token for it.

## Webhook for Slack, Discord or JSON

Planned in #29.

## Several backends for one level

Planned in #27.

## Review queue

`autoreview review-queue` lists every open pull request that requests a review
from the login of the credential and whose engine state is known, including
those that wait on the author. It sends the list to `review_queue.backends` and prints it. The
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
| `action` | The backend does not accept the message | The step fails and is retried. Otherwise nobody would learn that an action is needed. A message of a discovery pass is the exception, see below |
| `digest` | Dropped with a warning in the log | The review and its publication are unaffected. A message of a discovery pass follows the rule below |
| `log` | Dropped with a warning in the log | Nothing else changes. A message of a discovery pass follows the rule below |

The `approval_requested` message is the exception: the review is already
waiting when it is sent, so a failed delivery is logged as
`approval request failed`. `autoreview awaiting` lists the wait, and the review
queue shows it while the pull request requests the review, until it is
approved or its deadline passes.

A discovery pass sends its messages (a rejected pull request, a stopped or
retried review, a hand-off reminder) once, after it has saved its state. After
one fails, at any level, the rest of that level in the pass get no attempt, and
after a shutdown no message gets an attempt (code `shutdown`). Each message that
is not delivered is logged as `discovery notification dropped` with the event,
the review key, the level, the code and `skipped_after_failure`. It is never
resent, so the warning is the only record; discovery goes on.

Because a failed `action` delivery fails the step of a review, a broken action
backend blocks reviews that need a human. Watch for `matrix_unavailable` and
`matrix_request_rejected` in the logs; the [CLI error codes](../../cli.md#error-codes) list them.

`autoreview validate` checks the notification configuration, not whether a
backend is reachable. After you change a backend, confirm delivery with a real
event, for example a review that reaches `approval_requested` on a
`human-approval` rule.

## Choosing a layout

| Need | Layout |
|---|---|
| Team chat with a mention | Matrix for `action`, `digest` in a quiet room |

For how the level of each event is chosen, see
[policy notifications](../../policy.md#notifications). The model behind
backends is in [notifications](../concepts/notifications.md).
