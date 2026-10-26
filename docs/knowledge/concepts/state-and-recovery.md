---
type: Concept
id: concept/state-and-recovery
title: State and recovery
description: What the SQLite store keeps, how the single-engine lock and the discovery lock work, how reviews resume after a restart, the four retry layers (scheduler, step, session start and reviewer run), retention, backup-db and cleanup_manual.
source_kind: manual
intents: [debug, help, update]
---

# State and recovery

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

autoreview keeps all of its state in one SQLite file, `store.path` in
[config.yaml](../../configuration.md#store). A restart does not lose a review in
progress: the engine reads the file and carries on from the last step it finished.
This page describes what is stored, how the engine avoids two writers, which
retries happen at which layer, and what is deleted over time.

## What the store contains

The file holds four kinds of data.

| Data | What it is for |
|---|---|
| Discovery state | The per-pull-request schedule (debounce, running, waiting for a retry, completed, failed), the last published decision of every pull request, the approval waits, and the candidates that were rejected, so that each rejection is reported once. |
| Reviews | One row per review: its identity, the frozen input (rule, pipeline, route, scope, prior findings, reviewers), its status (`queued`, `running`, `awaiting`, `completed`), its result, the approval decision and deadline, and its outcome. |
| Review steps | The output of each finished step of a review: the combined artifact, the discussion, the commentable lines, the publication plan, the rendered body, the GitHub publication, the publication time, and the verification. |
| Review state | The progress of each reviewer slot and of the verifier slot: the session identity, the run and start counters, the stored result, and the verification. |

The database holds the published text of reviews and the finding excerpts that
refer to the repository. Protect it accordingly. Retention (below) limits how
long it keeps completed reviews.

## Single writer

Two engine processes must never run against one database. `autoreview serve`
takes an exclusive lock on a file next to the database when it starts. A second
`serve` on the same database fails with `engine_already_running` and exits.

Discovery has its own lock, on another file next to the database. A discovery
pass runs under that lock. The engine's own loop and `autoreview poll` both take
it, so a manual poll waits for a running pass instead of racing it. The wait lasts
at most `schedule.discovery_lock_timeout_seconds`, polled every
`schedule.discovery_lock_poll_ms`. A pass that cannot take the lock in time fails
and is retried on the next tick.

Within one process, SQLite runs writes one at a time on a single handle, so each
transaction is strictly sequential. The discovery state is read and written in
one transaction, together with the new review rows it creates.

## Resume after restart

`autoreview serve` ends by stopping its running reviews without marking them as
failed. A review keeps its status and its stored steps. On the next start the
engine finds every review in `queued` or `running`, and it resumes it:

- A step that already has stored output is not repeated. The stored output is read
  instead, so a publication that happened before the restart is not posted twice.
- A review that waits for an approval keeps its first deadline. A restart does not
  extend the wait.
- A reviewer whose session was started before the restart is found by its name.
  The engine does not start a second session for it.
- A session started by an attempt that was given up is looked for and removed, so
  it cannot finish later and leave output behind.

Step names are part of the stored state. Renaming a step in a new release would
make a resumed review repeat that step, so the engine's step names are kept stable
across releases.

## Retry layers

There are four places where something is retried. Each has its own limit, and
they are not nested in a way that multiplies silently: each layer counts its own
attempts and stops when its limit is reached.

| Layer | What is retried | Limit | Result when the limit is reached |
|---|---|---|---|
| Step retry | A single GitHub read, a pohunek read or a similar step that failed for a transient reason | `schedule.step_retry_seconds` in total, with a backoff that starts at `schedule.step_retry_initial_seconds` and doubles up to `schedule.step_retry_max_interval_seconds` | The error is returned to the review. Permanent errors are never retried. |
| Scheduler | A whole review attempt that failed with a retryable failure, such as a session that could not start, a pohunek request that failed or timed out, a base fetch that failed, or a GitHub request that failed | One automatic retry for each entry in `schedule.retry_delays_seconds` | `review_stopped` is raised, and the review waits for a manual `autoreview retry`. |
| Session start | Starting one reviewer session | `session.start_max_attempts`, waiting `session.start_retry_seconds` between them | The review attempt fails and goes to the scheduler. |
| Reviewer run | A reviewer whose output was incomplete, unusable or invalid | `session.reviewer_run_max_attempts` fresh sessions | The reviewer has no result, and the review fails. |

Two more rules apply to these layers:

- **Each start attempt gets its own branch.** A session start that fails leaves a
  branch behind. The next start uses another branch name, so it begins from the
  base again. The names are described in [how it works](how-it-works.md#4-sessions-per-reviewer).
- **A superseded attempt is not a failure.** When the head changes, or the trigger
  stops asking, the attempt ends quietly. The pull request is reviewed again at the
  new head after the debounce time, with a new attempt number.

A failure that is not in the retryable list is not retried by the scheduler. The
policy's notifications tell the user that the review stopped, and
[retry and recovery](../runbooks/retry-and-recovery.md) explains how to start it
again.

### Scheduler states

The scheduler keeps one internal state per pull request. The names below are not
CLI output:

| State | Meaning |
|---|---|
| `waiting` | A head was seen. The review starts after `schedule.debounce_seconds` of quiet time. |
| `running` | A review of this head is in progress. |
| `retry_wait` | The last attempt failed with a retryable failure. The next attempt is due at a set time. |
| `completed` | A published review covers the head, or the review finished. |
| `failed` | The attempts are used up, or the failure is not retryable. |

## Cleanup of reviewer sessions

When a reviewer's output is stored, its session is removed. Removal is retried up
to `session.cleanup_max_attempts`, waiting `session.cleanup_retry_seconds` between
attempts. A session that cannot be removed in those attempts raises
`cleanup_manual`. The stored output is safe, so the review still completes.

To remove the session by hand, find it with the pohunek commands and remove it with
`pohunek session rm`. The notification names the session and its host. The runbook
[stale sessions and branches](../runbooks/stale-sessions-and-branches.md) gives the
steps. Leftover review branches are removed with `autoreview prune-branches`.

## Retention

Two retention periods bound the store, and each is applied during the discovery
passes:

| Key | Removes | Why |
|---|---|---|
| `store.review_retention_seconds` | Completed reviews whose last update is older than this | Their steps are no longer needed to resume anything |
| `store.published_retention_seconds` | Published decisions older than this | A published decision is the baseline of the next incremental review. Older baselines are not used. |

Two consequences follow from the retention periods:

- An incremental review needs the stored result of the baseline review. If that
  result has been removed, the review widens to a full review (see
  [incremental reviews](incremental-reviews.md#full-or-incremental)).
- A notes hand-off reminder reads the published decision, so the reminder period
  must be shorter than `store.published_retention_seconds`. The configuration
  validator also checks this, see [configuration](../../configuration.md#store).

The engine also refuses to grow the discovery state without limit. The state
holds at most `schedule.state_entry_limit` pull request entries and review
history entries. A pass that would go over the limit stops with an error rather
than dropping entries silently.

## Backup

`autoreview backup-db <path>` writes a consistent copy of the store while the
service runs. It uses SQLite's `VACUUM INTO`. The path must be absolute and must
not exist yet, so an old backup is never overwritten.

```bash
autoreview backup-db /var/backups/autoreview/autoreview-$(date +%F).db
```

A backup of the store is also a copy of the review bodies. Store it with the same
care as the database. The runbook [upgrade and backup](../runbooks/upgrade-and-backup.md)
describes restoring a copy and upgrading the engine.

## Related pages

- The keys of the store and the schedule: [configuration](../../configuration.md).
- The commands that act on a stopped review: [CLI](../../cli.md).
- The steps of one review: [how it works](how-it-works.md).
- Restart of a pending approval: [decisions: approval wait](decisions.md#approval-wait).
- Troubleshooting by error code: [troubleshooting](../runbooks/troubleshooting.md).
