---
type: Concept
id: concept/notifications
title: Notifications
description: The events autoreview emits, their three levels, how levels and single events route to notifier backends, what happens when a delivery fails, the hourly review queue, the contents of a message and its Review ID, and how a new backend type would be added.
source_kind: manual
intents: [setup, help, debug]
---

# Notifications

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

autoreview tells people about reviews that need them, and it records the rest.
Each **event** has a **level**. Each level goes to one or more **backends**. The
policy sets the level of every event, and the configuration sets the routes from
levels to backends. A single event can have its own route; every other event
takes the route of its level. The keys are in
[policy: notifications](../../policy.md#notifications) and
[configuration: notify](../../configuration.md#notify).

## Levels

| Level | Meaning | Who is told |
|---|---|---|
| `action` | Someone has to do something. The message names the person to mention. | The backends that the `action` route lists, or the event's own route. The configured person is mentioned. |
| `digest` | A quiet record of a result. | The backends of the `digest` route, or the event's own route. Nobody is mentioned. |
| `log` | Muted. | Only the structured log. Backends of the `log` route, or the event's own route, receive it too, if listed. |

Routes map each level to one backend name. Several backends of the same type
are fine, for example one chat room per level:

```yaml
notify:
  backends:
    - name: matrix-action
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_TOKEN
      room: "!actions:example.org"
      mention_user: "@reviewer:example.org"
    - name: matrix-digest
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_TOKEN
      room: "!digest:example.org"
      mention_user: "@reviewer:example.org"
  routes:
    action: [matrix-action]
    digest: [matrix-digest]
    log: [matrix-digest]
```

An event can have its own route next to the levels, for example
`approved: [matrix-approvals]` under `routes`. That event goes only to its own
route; its level still decides whether the person is mentioned and whether a
failed delivery is dropped. Every other event goes to the route of its level.

Routes reach `matrix` backends; `none` and `webhook` backends are planned in
#29. See [notifiers](../guides/notifiers.md) for the setup.

## Events

The policy must list a level for every event below. A policy that leaves one out
fails validation with `notifications: event X needs a level`.

| Event | Sent when | Example level |
|---|---|---|
| `approval_requested` | A clean review waits for a human approval | `action` |
| `handoff_reminder` | Notes were handed to an author, who did not ask for a review again within `notes_handoff.reminder_hours` | `action` |
| `findings_on_own_pr` | A review of the reviewing account's own pull request has blocking findings | `action` |
| `review_stopped` | A review failed after its retries, reviews wait for a review host, or an approved review could not be published | `action` |
| `cleanup_manual` | A reviewer session could not be removed and needs `pohunek session rm` | `action` |
| `command_failed` | An operator command failed, such as `approve` or `retry` | `action` |
| `approved` | An approval was published | `digest` |
| `changes_requested` | A request for changes was published | `digest` |
| `commented` | A comment review was published | `digest` |
| `approval_expired` | A wait for approval passed `approval.timeout_hours` | `digest` |
| `started` | A review started | `log` |
| `retry_scheduled` | A failed review is retried later | `log` |
| `superseded` | A newer head or a changed trigger replaced a review | `log` |

Events for a rejected or an expiring GitHub token are planned in #19.

The example levels come from the example policy. Each deployment chooses its own.

## Delivery failure

Delivery can fail when a backend is down or rejects a message. No review and no
discovery pass waits for a backend; only the warning differs by level:

| Level | Delivery fails | What happens |
|---|---|---|
| `digest` or `log` | The message cannot be delivered | Dropped with a warning in the log. The review continues. A message of a discovery pass is logged as described below. |
| `action` | The message cannot be delivered | Dropped with a warning in the log. The review continues, see below. |

A message of a review gets one attempt, bounded by the notifier's request
timeout (`notify.timeout_seconds`), and is never retried. A failed message is
logged as the warning `review notification dropped` with its event, review key,
level and error code. At the `digest` and `log` levels the notifier itself drops
a message the homeserver does not take and logs `matrix notification dropped`
instead. The review then goes on as if the message had
been sent, and a resumed review does not send it again. When the service shuts
down during the attempt of an `action` message, the warning carries the code
`shutdown`, and the resumed review makes one more attempt. At the `digest` and
`log` levels the notifier logs `matrix notification dropped` with the code
`matrix_unavailable` instead, and the resumed review also sends the message
again. A waiting review stays visible without
its `approval_requested` message: `autoreview awaiting` and the review queue
list the wait until it is approved or its deadline passes.

A discovery pass sends its messages after it has saved its state and released
the discovery lock: a rejected pull request (`review_stopped`), a review that
stopped or waits for a retry (`review_stopped`, `retry_scheduled`), reviews that
wait for a review host (`review_stopped`, once per outage), and a
`handoff_reminder`. Each message gets one attempt, bounded by the notifier's
request timeout. Once a message fails, at any level, the later messages of the
same route in the same pass get no attempt; the route is the event's own route
or, without one, its level. When the service shuts
down, the messages not yet sent get no attempt and carry the error code
`shutdown`. A message that is not delivered is logged as the warning
`discovery notification dropped` with its event, review key, level, error code
and `skipped_after_failure` (`true` when it got no attempt after a failure of
its route). A dropped message is never resent: the pass keeps its result, no
later pass repeats it, and the warning is the only record. A pass that fails
before it saves sends nothing, and the next pass that saves sends those
messages.

A message that is not delivered is lost. A backend that is down loses the
messages of that time, and the state of the review is still visible with
`autoreview review`.

## Review queue

The review queue is a list of every open pull request that asks for a review from the login
of a credential, in the repositories that credential serves, with the engine state of each one. It is not
an event. It is produced on demand and by a timer:

```bash
autoreview review-queue
```

The command sends the queue to the backends listed in
`notify.review_queue.backends`, and it prints the same content. Run it hourly from
a systemd timer or a cron job. The queue is sent as a quiet notice, so it does not
notify anyone.

Rules for the queue:

- Entries that need the user's action come first, then entries that only wait for
  the engine. Within a priority, entries are sorted by repository and number.
  The cut is made after sorting, so an entry that needs action is not dropped to
  make room for one that does not.
- Pull requests whose notes were handed to the author are listed with the state
  **Waiting on author**. These are listed apart from the other requests, because
  the author has to act first.
- At most `notify.review_queue.max_items` pull requests are listed. The message
  says when it was truncated.
- Nothing is sent when nothing waits. The command still prints an empty list.
- A pull request whose state cannot be read is left out of this message, and the
  next run includes it again.
- The message is cut between entries, never inside one, so formatted content stays
  valid.
- The message gets one attempt. When it fails, the warning
  `review queue digest dropped` names the code, and the command fails with it.

## Message contents

Every review message starts with a header. The header has the same fields in each
kind of message:

```text
GitHub review completed with no blocking findings

Review ID: owner/repo#PR42@3f2a9c1d0b7e
PR: https://github.com/owner/repo/pull/42
Title: Add retry to the sync worker
Reviewers: Opus, Sonnet
Scope: full
```

- **Review ID** has the form `owner/repo#PR<n>@<12-hex>`. The last part is the
  first twelve characters of the head SHA. It identifies the review in every
  message and in the commands.
- **Title** is shortened to one line.
- **Reviewers** lists the labels of the reviewers of the run.
- **Scope** is `full` or `incremental`.

The approval request adds the number of blocking findings, the number of notes,
how to approve, and the time when the wait expires. It says that nothing has been
posted to GitHub yet.
The summary of the first reviewer and the list of findings follow, with the finding id,
severity, confidence, location and title. The list of notes comes after the
approval instruction, so a cut message still shows what to do.

A Review ID is enough to find the wait. `autoreview resolve` takes the repository,
the pull request number and a head prefix of at least twelve characters, and it
prints the full head SHA. `autoreview approve` then publishes the approval for that
head. See [CLI: resolve](../../cli.md#autoreview-resolve).

The message length is bounded by `notify.max_characters`. A longer message is cut
with an ellipsis. Excerpts inside it are bounded by `limits.excerpt_characters`.

## Adding a backend (design)

Not in v0.1. The design below keeps any later channel out of the rest of the
engine.

1. **One channel-neutral message.** Every event produces the same message object:
   level, event, title, text, Review ID and links. The backend decides how to
   show it. The text is plain. A backend that supports formatting adds it
   itself.
2. **A registry of backend types.** Each type registers its name, its
   configuration keys and a send function. The engine looks a backend up by the
   `type` key, so a new type needs no change in the policy.
3. **Routes stay as they are.** A backend is only a name in `notify.backends` and
   in `notify.routes`. Several backends of one type may coexist.
4. **Failures are classified.** A send error says whether the backend was
   unavailable (retried or dropped, depending on level) or whether it rejected
   the message (never retried). The rules for `digest`, `log` and `action`
   above apply to every backend type.
5. **Secrets come from the environment.** A backend that needs a token names the
   variable in an `*_env` key, as the Matrix backend does.

Phase 6 of the project plan lists further backends as later decisions, each made
when a concrete need arrives.

## Related pages

- The guide for choosing backends: [notifiers](../guides/notifiers.md).
- The approval that a message asks for: [decisions: approval wait](decisions.md#approval-wait).
- The review that the message describes: [how it works](how-it-works.md#10-notifications).
- Event names in the policy: [policy: notifications](../../policy.md#notifications).
- Error codes of a failed delivery: [CLI error codes](../../cli.md#error-codes).
