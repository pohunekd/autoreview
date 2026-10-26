---
type: Reference
id: reference/cli
title: CLI reference
description: Every autoreview command - serve, validate, init, profiles, poll, candidates, reviews, review, awaiting, approve, resolve, retry, review-queue, prune-branches and backup-db - with arguments and JSON output.
source_kind: manual
intents: [setup, debug, help]
---

# CLI reference

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

```
autoreview [--config <path>] [--env-file <path>] <command> [arguments]
```

`--env-file` reads `AUTOREVIEW_*` variables from a file instead of the
process environment: one `NAME=value` per line, the file owned by the current
user with mode `0600` (otherwise the command refuses with
`configuration_invalid`). Values from the file win over the environment. The
workflow plugin uses it, because pohunek starts plugins with an empty
environment.

Every command reads the configuration from `--config` or `AUTOREVIEW_CONFIG`
and validates it together with the policy before doing anything. Output is
one JSON object (or array) on stdout; logs are JSON lines on stderr. A failed
command prints `{"error": "<code>", ...}` with a stable error code and exits
with status 1, so scripts and agents can branch on the code.

## Setup

### `autoreview init`

```
autoreview init --dir <directory> --login <github-login> --pohunek-host <host> [--example <name>]
```

`--login` becomes the login of the single credential `init` writes
(`match: ["*"]`, token in `AUTOREVIEW_GH_TOKEN`); add credentials by hand.
Writes `config.yaml` with every key, `policy.yaml` from one of the
[examples](knowledge/guides/index.md) (default `single-repo-comment`), and the
prompts `prompts/generic.md` and `prompts/verifier.md` plus any prompt the
example adds (such as `org-wildcard`'s per-repository prompts). It refuses to overwrite
existing files. Prints the agent profiles the policy needs.

### `autoreview validate`

Loads config and policy, reads every prompt, checks that every policy rule is
covered by a credential and every token is valid and belongs to its login,
and checks that the pohunek CLI answers and its version is supported. Prints:

```json
{
  "policy_digest": "...",
  "agent_profiles": ["autoreview-codex"],
  "pohunek_version": "0.37.0",
  "credentials": [
    {"name": "default", "login": "review-bot", "match": ["*"], "expires_at": "2027-06-30T00:00:00Z"}
  ]
}
```

`expires_at` is `null` for a token without an expiry date. Token values are
never printed.

### `autoreview profiles install`

```
autoreview profiles install --host <host> --agent claude|codex --model <model> --name <profile> [--replace]
```

Creates the pohunek agent profile and adapter wrapper a reviewer runs on one
review host. The base runtime (`claude` or `codex`) must already be installed
on that host as its official pohunek runtime package. Running it again with the
same arguments changes nothing; it refuses to overwrite a profile it did not
create unless `--replace` is given. See
[reviewers and adapters](knowledge/concepts/reviewers-and-adapters.md).

## Service

### `autoreview serve`

Runs discovery and reviews until stopped. Checks first every credential as
`validate` does and refuses to start when one fails
(`credential_invalid`, `credential_login_mismatch`). While running, a failing
credential suspends only its own repositories, see
[credentials](configuration.md#credentials). A second `serve` on the same database is refused. Reviews
running at shutdown resume from their stored steps on the next start.

### `autoreview poll`

Runs one discovery pass now and prints what it queued. Safe while `serve`
runs: passes take turns on a lock.

### `autoreview candidates`

Read-only: prints the pull requests the next discovery pass would consider,
per rule and trigger.

## Inspect

| Command | Prints |
|---|---|
| `autoreview reviews [LIMIT]` | The newest reviews, at most `cli.review_list_limit` |
| `autoreview review --repository OWNER/REPO NUMBER` | Everything the engine knows about one pull request: current head, reviews, published decisions, waits, hand-offs |
| `autoreview awaiting` | Reviews that wait for a human approval |
| `autoreview review-queue` | Sends the review queue to `notify.review_queue.backends` and prints it, grouped by account when credentials have different logins. Run it from a timer |

## Act

### `autoreview approve`

```
autoreview approve --repository OWNER/REPO NUMBER HEAD_SHA
```

Approves a clean review that waits for a human (`on_clean: await`). `HEAD_SHA`
is the full head commit; the approval applies only if it is still the pull
request head. The engine publishes on its next tick:

```json
{"publication_status": "pending", "review_id": "zajca/pohunek#PR1450@3f2a9c1d0b7e"}
```

Run it again to see `published` with `github_review_id`. Repeating an approval is
safe. `review_superseded` means a newer commit or a changed review request
replaced the review; `approval_expired` means `approval.timeout_hours` passed.
In both cases nothing is posted.

### `autoreview resolve`

```
autoreview resolve --repository OWNER/REPO NUMBER HEAD_PREFIX
```

Finds the waiting review for a head prefix of at least 12 characters (the form
notifications show in `Review ID`) and prints its full `head_sha`. Read-only.
Chat integrations use it before `approve`.

### `autoreview retry`

```
autoreview retry --repository OWNER/REPO NUMBER HEAD_PREFIX
```

Starts a new attempt of a review that stopped after its retries
(`review_stopped`). The head must still be the pull request head.

## Maintenance

### `autoreview prune-branches`

```
autoreview prune-branches [--host <host>] [--older-than-days N] [--yes]
```

Lists review branches under `session.branch_prefix` on the review hosts that
belong to no running review; with `--yes` deletes them and prints a list of
`branch` and `commit` for recovery. pohunek keeps a session's branch after the
session is removed, so these accumulate. See
[stale sessions and branches](knowledge/runbooks/stale-sessions-and-branches.md).

### `autoreview backup-db`

```
autoreview backup-db <path>
```

Writes a consistent copy of the SQLite store while the service runs. `path`
must be absolute and must not exist yet (`backup_path_invalid`,
`backup_path_exists`).

## Error codes

| Code | Meaning |
|---|---|
| `configuration_missing`, `configuration_invalid` | A key is missing or invalid; the message names it |
| `command_invalid` | Wrong arguments |
| `repository_not_allowed`, `review_identity_invalid` | Malformed repository, number or head |
| `review_not_awaiting_approval`, `approval_not_required` | The review does not wait for an approval |
| `review_superseded`, `approval_expired` | The wait ended without a publication |
| `head_changed` | The pull request has a newer head than the one given |
| `review_already_running`, `review_retry_not_dispatched` | A retry is not needed or not possible right now |
| `engine_already_running` | Another `serve` uses the same database |
| `credential_not_covered`, `credential_unused` | A policy rule has no credential, or a credential serves no rule |
| `credential_invalid`, `credential_login_mismatch` | A token was rejected, expired, or belongs to another login than its credential |
| `discovery_busy` | Another discovery pass held the lock past `schedule.discovery_lock_timeout_seconds` |
| `pull_request_not_open`, `pull_request_draft`, `review_label_removed`, `review_request_not_active` | The pull request no longer asks for a review: closed, back in draft, label removed or review request withdrawn |
| `pohunek_base_fetch_failed`, `pohunek_base_fetch_timeout`, `pohunek_base_fetch_dns_failed` | The review host could not fetch the base branch from GitHub |
| `state_entry_limit_exceeded` | The discovery state hit `schedule.state_entry_limit` |
| `store_unavailable` | The SQLite store could not be opened or written |
| `backup_path_invalid`, `backup_path_exists`, `backup_failed` | `backup-db` target problems |
| `project_route_not_found`, `project_route_ambiguous` | The repository is registered on no review host, or on several without a route override |
| `pohunek_request_failed`, `pohunek_request_timeout`, `pohunek_session_start_failed` | The review host did not answer or could not start a session |
| `pohunek_base_fetch_ref_locked`, `pohunek_session_warning` | The session start hit a locked base fetch or reported a warning; retried on a fresh branch |
| `pohunek_session_cleanup_failed` | A session could not be removed, see `cleanup_manual` |
| `review_output_timeout`, `review_output_invalid`, `review_result_invalid` | A reviewer did not return a usable result in time |
| `github_search_limit_exceeded` | A search returned more than `schedule.poll_limit`; narrow the rules or raise the limit |
| `github_request_failed`, `github_request_rejected` | GitHub did not answer or refused the request |
| `matrix_unavailable`, `matrix_request_rejected`, `webhook_unavailable`, `webhook_request_rejected` | A notifier did not accept a message |

The [troubleshooting runbook](knowledge/runbooks/troubleshooting.md) maps codes
to fixes.
