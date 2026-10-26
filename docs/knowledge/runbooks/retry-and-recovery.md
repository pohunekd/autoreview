---
type: Runbook
id: runbook/retry-and-recovery
title: Retry and recovery
description: What to do when a review stopped, how restarts resume reviews, and how superseded, expired and half-published reviews are handled.
source_kind: manual
intents: [debug, help]
---

# Retry and recovery

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A review moves through steps. Each step is retried by the scheduler, and a
review that still fails after its retries stops with the event
`review_stopped`. The commands here inspect and restart such a review. Error
codes not listed here are in the [troubleshooting runbook](troubleshooting.md).

## Review stopped

`review_stopped` is an action-level event: the notifier asks someone to act.
The review does not restart by itself after it stopped.

1. Find the pull request and its state:

   ```sh
   autoreview --config /etc/autoreview/config.yaml reviews
   autoreview --config /etc/autoreview/config.yaml review --repository <owner/repo> <number>
   ```

   `reviews` lists the newest reviews, up to `cli.review_list_limit`. `review`
   shows the current head, the reviews, the published decisions, the waits and
   the hand-offs. Read the last failure before you retry.

2. Decide whether a retry can work. A retry needs the head to be the pull
   request head. If the author pushed, the old review is superseded and there is
   nothing to retry: the new head is reviewed.

3. Retry with the head prefix that `review` shows (at least 12 characters):

   ```sh
   autoreview --config /etc/autoreview/config.yaml retry --repository <owner/repo> <number> <head-prefix>
   ```

   A retry is refused with `review_already_running` when the review still runs,
   and with `review_retry_not_dispatched` when a retry is not possible right now.
   `head_changed` means the head moved; see the step above.

If the same failure stops the review again, fix its cause first. The usual
causes are in [troubleshooting](troubleshooting.md): route errors, session start
failures and reviewer output errors.

## Restarts

Reviews are durable. All state is in the SQLite store at `store.path`.

- A review running when the service stops resumes from its stored steps on the
  next `serve`.
- A second `serve` on the same database is refused with `engine_already_running`.
  Stop the old process first, and do not delete the database to get past the error.
- `autoreview poll` may run while `serve` runs. Passes take turns on a lock.
- Each published decision carries its own `publication_status` (`pending` or
  `published`), which `autoreview review` shows.

Before a planned restart, run `validate` (see [upgrade and backup](upgrade-and-backup.md)).

## Superseded reviews

A review is `superseded` (event `superseded`, log level by default) when a newer
commit arrives or the review request changes before the review was published.
Nothing is published for the old head. The newer head is reviewed under its own
review. `approve` for the old head returns `review_superseded`, and that is the
expected answer. Do not retry a superseded review.

## Expired waits

A clean review under `on_clean: await` waits for a human approval for
`approval.timeout_hours` (1 to 720). A wait that passes that time ends as
`approval_expired`: the event is raised and `approve` returns `approval_expired`.
Nothing is posted.

The reference does not say whether a new review request alone starts a fresh
review of an expired wait. The documented way to get a new decision is a new
commit on the pull request. Check the behaviour on a test pull request before
you rely on the request alone.

## A review published, but a notification failed

Publishing the review on GitHub and sending the notice are two operations. A
failed `action` notice fails the step, so the step is retried. A failed `digest`
or `log` notice is dropped with a warning and nothing is retried.

When you see a notification failure after a publication:

1. Check GitHub. Open the pull request and look for the review. Its body ends
   with the AI signature, and its hidden marker names the repository, the number
   and the head SHA (`<!-- autoreview:v1:<repo>#<n>@<head>:... -->`).
2. Check the store with `autoreview review --repository <owner/repo> <number>`.
   A published decision shows `publication_status: published` and a
   `github_review_id`.
3. Do not retry or re-run a review only to resend a notice. The reference does
   not say how a notification failure after publication is retried, so a
   blind retry could publish a second decision on a changed head. If the
   publication is missing on GitHub, check the `review_stopped` event first.
4. Fix the notifier (see [notifier failures](troubleshooting.md#notifier-failures)).
   Then tell the people concerned about the decision by hand, if they need it.

## Related pages

- [Troubleshooting](troubleshooting.md)
- [Stale sessions and branches](stale-sessions-and-branches.md)
- [Upgrade and backup](upgrade-and-backup.md)
- [CLI reference](../../cli.md)
- [State and recovery](../concepts/state-and-recovery.md)
