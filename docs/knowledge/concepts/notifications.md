---
type: Concept
id: concept/notifications
title: Notifications
description: The events autoreview emits, their three levels, how levels route to notifier backends, what happens when a delivery fails, the hourly review queue, the contents of a message and its Review ID, and how a new backend type would be added.
source_kind: manual
intents: [setup, help, debug]
---

# Notifications

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

autoreview tells people about reviews that need them, and it records the rest.
Each **event** has a **level**. Each level goes to one or more **backends**. The
policy sets the level of every event, and the configuration sets the routes from
levels to backends. The keys are in [policy: notifications](../../policy.md#notifications)
and [configuration: notify](../../configuration.md#notify).

## Levels

| Level | Meaning | Who is told |
|---|---|---|
| `action` | Someone has to do something. The message names the person to mention. | The backends that the `action` route lists. The configured person is mentioned. |
| `digest` | A quiet record of a result. | The backends of the `digest` route. Nobody is mentioned. |
| `log` | Muted. | Only the structured log. Backends of the `log` route receive it too, if listed. |

Routes map each level to a list of backend names. Several backends of the same
type are fine, for example one chat room per level:

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
    - name: log
      type: none
  routes:
    action: [matrix-action]
    digest: [matrix-digest]
    log: [log]
```

The backend types are `none`, `matrix` and `webhook`. See
[notifiers](../guides/notifiers.md) for setup of each one.

## Events

The policy must list a level for every event below. A policy that leaves one out
fails validation with `notifications: event X needs a level`.

| Event | Sent when | Example level |
|---|---|---|
| `approval_requested` | A clean review waits for a human approval | `action` |
| `handoff_reminder` | Notes were handed to an author, who did not ask for a review again within `notes_handoff.reminder_hours` | `action` |
| `findings_on_own_pr` | A review of the reviewing account's own pull request has blocking findings | `action` |
| `credential_invalid` | A GitHub token was rejected or belongs to another login, so its repositories are not reviewed until the token is replaced and the service restarted | `action` |
| `credential_expiring` | A GitHub token expires within `github.expiry_warning_days`; raised once a day | `action` |
| `review_stopped` | A review failed after its retries, or an approved review could not be published | `action` |
| `cleanup_manual` | A reviewer session could not be removed and needs `pohunek session rm` | `action` |
| `command_failed` | An operator command failed, such as `approve` or `retry` | `action` |
| `approved` | An approval was published | `digest` |
| `changes_requested` | A request for changes was published | `digest` |
| `commented` | A comment review was published | `digest` |
| `approval_expired` | A wait for approval passed `approval.timeout_hours` | `digest` |
| `started` | A review started | `log` |
| `retry_scheduled` | A failed review is retried later | `log` |
| `superseded` | A newer head or a changed trigger replaced a review | `log` |

The example levels come from the example policy. Each deployment chooses its own.

## Delivery failure

Delivery can fail when a backend is down or rejects a message. The rule depends
on the level, because a missed `action` message means that nobody learns that a
review waits:

| Level | Delivery fails | What happens |
|---|---|---|
| `digest` or `log` | The message cannot be delivered | Dropped with a warning in the log. The review continues. |
| `action` | The message cannot be delivered | The step fails. |

A failed `action` message fails the step that sent it. For `approval_requested`
the wait is cleared and the review fails, so the scheduler reports it instead,
and nobody is left waiting for an approval that no one knows about. The
scheduler's own retry rules then apply (see
[state and recovery](state-and-recovery.md#retry-layers)).

A digest or log message is never retried. A backend that is down during a
digest loses that digest, and the state of the review is still visible with
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
- When the credentials have different logins, the queue is grouped by account.
- At most `notify.review_queue.max_items` pull requests are listed. The message
  says when it was truncated.
- Nothing is sent when nothing waits. The command still prints an empty list.
- A pull request whose state cannot be read is left out of this message, and the
  next run includes it again.
- The message is cut between entries, never inside one, so formatted content stays
  valid.

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
The summary of each reviewer and the list of findings follow, with the finding id,
severity, confidence, location and title. The list of notes comes after the
approval instruction, so a cut message still shows what to do.

A Review ID is enough to find the wait. `autoreview resolve` takes the repository,
the pull request number and a head prefix of at least twelve characters, and it
prints the full head SHA. `autoreview approve` then publishes the approval for that
head. See [CLI: resolve](../../cli.md#autoreview-resolve).

The message length is bounded by `notify.max_characters`. A longer message is cut
with an ellipsis. Excerpts inside it are bounded by `limits.excerpt_characters`.

## Adding a backend (design)

Not in v0.1. The first release has the three types above. The design below keeps
any later channel out of the rest of the engine.

1. **One channel-neutral message.** Every event produces the same message object:
   level, event, title, text, Review ID and links. The backend decides how to
   show it. The text is plain. A backend that supports formatting adds it
   itself. The webhook `json` format already uses this object.
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
   variable in an `*_env` key, as the Matrix and webhook backends do.

Phase 6 of the project plan lists further backends as later decisions, each made
when a concrete need arrives.

## Related pages

- The guide for choosing backends: [notifiers](../guides/notifiers.md).
- The approval that a message asks for: [decisions: approval wait](decisions.md#approval-wait).
- The review that the message describes: [how it works](how-it-works.md#10-notifications).
- Event names in the policy: [policy: notifications](../../policy.md#notifications).
- Error codes of a failed delivery: [CLI error codes](../../cli.md#error-codes).
