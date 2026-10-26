---
type: Runbook
id: runbook/troubleshooting
title: Troubleshooting
description: Symptoms and error codes of autoreview mapped to their causes and fixes, from a pull request that is never reviewed to an approval that is refused.
source_kind: manual
intents: [debug, help]
---

# Troubleshooting

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Start with two commands. `autoreview candidates` shows what the next discovery
pass would consider, per rule and trigger. `autoreview review --repository
OWNER/REPO NUMBER` shows everything the engine knows about one pull request. Every
error is a JSON object with `"error": "<code>"` on stdout and exit status 1; the
codes below are the ones in the [CLI reference](../../cli.md#error-codes).

## Symptom table

| Symptom or code | Usual cause | Fix |
|---|---|---|
| Pull request never appears | Trigger not met, no rule matches, or it is a draft | [Nothing gets reviewed](#nothing-gets-reviewed) |
| `project_route_not_found` | Repository is not registered on a host in `pohunek.hosts` | [Route errors](#project_route_not_found) |
| `project_route_ambiguous` | Registered on two hosts, no `route_overrides` entry | [Route errors](#project_route_ambiguous) |
| `pohunek_session_start_failed` | Review host could not start the session | [Session start failures](#session-start-failures) |
| `pohunek_base_fetch_ref_locked`, `pohunek_session_warning` | Base fetch on a locked ref, or a session warning | [Session start failures](#session-start-failures) |
| `pohunek_request_failed`, `pohunek_request_timeout` | Review host did not answer | [Session start failures](#session-start-failures) |
| `review_output_timeout` | Reviewer did not finish within `session.timeout_seconds` | [Reviewer output](#reviewer-output) |
| `review_output_invalid`, `review_result_invalid` | Reviewer output was unusable or broke a limit | [Reviewer output](#reviewer-output) |
| `github_search_limit_exceeded` | Discovery search returned more than `schedule.poll_limit` | [Search limit](#github_search_limit_exceeded) |
| `github_request_failed`, `github_request_rejected` | GitHub did not answer or refused the request | Check the token's permissions and the `github` section |
| `credential_invalid` | A token was rejected, has expired, or its variable is empty; its repositories are not reviewed | [credential_invalid](#credential_invalid) |
| `credential_login_mismatch` | A token belongs to another account than the `login` of its credential | [credential_login_mismatch](#credential_login_mismatch) |
| `credential_not_covered`, `credential_unused` | A policy rule has no credential, or a credential serves no rule | [credential_not_covered and credential_unused](#credential_not_covered-and-credential_unused) |
| Event `credential_expiring` | A token expires within `github.expiry_warning_days` | [credential_expiring](#credential_expiring) |
| A fine-grained token sees nothing | Organization approval pending, wrong resource owner, or repository not selected | [A fine-grained token sees nothing](#a-fine-grained-token-sees-nothing) |
| `engine_already_running` | Another `serve` uses the same database | Stop the other process |
| `matrix_unavailable`, `matrix_request_rejected`, `webhook_unavailable`, `webhook_request_rejected` | A notifier did not accept a message | [Notifier failures](#notifier-failures) |
| `review_not_awaiting_approval`, `approval_not_required` | The review does not wait for a human | [Approval errors](#approval-errors) |
| `review_superseded`, `approval_expired` | The wait ended and nothing was posted | [Approval errors](#approval-errors) |
| `head_changed` | The head SHA given is no longer the pull request head | [Approval errors](#approval-errors) |
| `review_already_running`, `review_retry_not_dispatched` | A retry is not needed or not possible now | [Retry and recovery](retry-and-recovery.md) |
| `configuration_missing`, `configuration_invalid` | A key is missing or invalid; the message names it | Fix the key, then run `validate` |
| `pohunek_session_cleanup_failed` | A reviewer session could not be removed | [Stale sessions](stale-sessions-and-branches.md) |

Policy validation errors (for example `unreachable, owner/* matches first`) are
not error codes; they are listed in the [policy reference](../../policy.md#validation-errors).

## Nothing gets reviewed

Work through the list in this order. `autoreview candidates` answers the first
three questions directly: a pull request that is not in its output is not
considered.

1. **Trigger.** A `review-requested` rule needs a review requested from the `login`
   of the credential that serves the repository. A `label:<name>` rule needs the
   label with that exact name.
   The next discovery pass (every `schedule.poll_interval_seconds`) picks it up.
2. **Rule.** The first rule whose `match` fits wins. `match` is lowercase
   `owner/repository` or `owner/*`; a repository in another case does not match.
   A pull request no rule matches is never reviewed (`unmatched: skip`).
3. **Author class.** An untrusted author with `untrusted: skip` is not reviewed.
   See [untrusted authors](../safety/untrusted-authors.md).
4. **Search limit.** A truncated search fails with `github_search_limit_exceeded`
   and drops nothing silently. See [the next section](#github_search_limit_exceeded).
5. **Draft.** A draft is not reviewed. A draft by a login in `ready_draft_authors`
   is marked ready only when it requests a review from the credential's `login`.
6. **Debounce.** A pull request pushed to in the last `schedule.debounce_seconds`
   waits before the review starts.

If the pull request is a candidate but no review appears, run
`autoreview review --repository <owner/repo> <number>` and read its state.

## Route errors

### project_route_not_found

The repository is not registered as a pohunek project on any host in
`pohunek.hosts` that the pipeline's `project_sources` allows. Check these in order:

- The repository is registered on a review host: `pohunek project add` was run
  there (see the [install page](../../install.md#prerequisites)).
- The origin URL of that pohunek project is the repository's GitHub URL. A fork
  or a renamed remote does not match.
- `project_sources` includes `manual` (for projects added with `pohunek project add`)
  or `auto` (discovered projects) for the pipeline.
- The host is listed in `pohunek.hosts`.

When a host does not answer, the error is `pohunek_request_failed` or
`pohunek_request_timeout`. The reference does not say whether an unreachable
host is reported as `project_route_not_found`. Check the tunnel to the host and
the `pohunek.timeout_seconds` value.

### project_route_ambiguous

The repository is registered on two or more hosts and no route pins it. Either
remove the duplicate registration, or pin the repository:

```yaml
pohunek:
  route_overrides:
    zajca/pohunek: {host: <review-host>, project_id: <project-id>}
```

A route override pins one repository to one host. See
[multiple review hosts](../guides/multihost.md).

## Session start failures

| Code | Meaning and fix |
|---|---|
| `pohunek_session_start_failed` | The host could not start a session. Check that the agent profile exists (`autoreview profiles install`) and that the runtime package is installed on that host. Each start attempt uses its own branch, so the engine retries up to `session.start_max_attempts`. |
| `pohunek_base_fetch_ref_locked` | The base fetch hit a locked ref (pohunek [#1437](https://github.com/zajca/pohunek/issues/1437)). The engine retries on a fresh `-startN` branch. Repeated failures: see [stale sessions and branches](stale-sessions-and-branches.md). |
| `pohunek_session_warning` | The session reported a warning. The engine retries on a fresh branch. Read the warning on the review host before you change the policy. |
| `pohunek_request_failed`, `pohunek_request_timeout` | The pohunek call failed or passed `pohunek.timeout_seconds`. A session start includes the base fetch, so 45 seconds is the recommended minimum. |

When all start attempts fail, the review reaches `review_stopped`. See
[retry and recovery](retry-and-recovery.md).

## Reviewer output

A reviewer run ends in one of these codes:

- `review_output_timeout`: the session ran longer than `session.timeout_seconds`
  (default in the recommended config: 7200). Check the diff size and the model
  on the review host before you raise the timeout.
- `review_output_invalid`: the output could not be read as a reviewer result.
  Common cause: an output larger than `session.output_max_bytes`. Codex keeps
  its transcript in the output, so raise that bound for Codex.
- `review_result_invalid`: the output parsed but broke a bound in `limits`
  (`result_max_bytes`, `max_findings`, summary or title length). Tighten the
  prompt so the reviewer stays within the bound, or raise the bound with care.

Each unusable run gets a fresh session, up to `session.reviewer_run_max_attempts`.
If the same reviewer keeps failing, compare its agent profile and its prompt
with the protocol named in `policy.yaml`.

## github_search_limit_exceeded

A discovery search returned more than `schedule.poll_limit` results. The
engine stops rather than drop pull requests. Narrow the work: use a more
specific `match` or a label trigger, or split a large organization into
rules. Raise `schedule.poll_limit` only when the narrowing is not enough.

## Credential errors

`serve` and `validate` check every token of `github.credentials` before they do
anything else. A token that fails a check stops startup with its error code. While
the service runs, a token that GitHub rejects only suspends its own credential; the
other credentials keep working. Replacing the token and restarting resumes them.

Check that a variable is set without printing its value:
`test -n "$AUTOREVIEW_GH_TOKEN" && echo set`. Use the variable name from the
credential's `token_env`.

### credential_invalid

GitHub rejected the token, its expiry has passed, or the variable is empty. At
startup, `serve` and `validate` refuse with `credential_invalid`. While running, the
event `credential_invalid` (level `action` in the examples) is raised, and the
repositories of that credential are not reviewed until the token is replaced. Reviews
in flight wait. Replace the value of the variable named by `token_env`, then restart
the service.

### credential_login_mismatch

The token belongs to another account than the `login` of its credential. Startup
refuses, and the message names both logins. Either correct `login` in the
credential, or replace the token with one of that account. Several credentials may
share a login, but each token must belong to the login of its own credential.

To see which login a token belongs to, load the environment file into a shell
without printing it, then ask GitHub with the token of that variable. Never print the
token itself. autoreview passes each token only to the `gh` children of its own
credential, as `GH_TOKEN`, and ignores `GH_TOKEN` in its own environment:

```sh
set -a; . /etc/autoreview/autoreview.env; set +a
GH_TOKEN="$AUTOREVIEW_GH_TOKEN" gh api user --jq .login
```

### credential_not_covered and credential_unused

Both are reported by `validate`, so `serve` refuses to start on them too.
`credential_not_covered` means a repository that a policy rule can match has no
credential. `credential_unused` means a credential serves no rule. Fix a
not-covered rule by adding a credential whose `match` covers its repository, placed
above any broader credential. For an unused credential, remove it or correct its
`match`. A credential that an earlier credential fully covers is also an error; its
message names the credential.

### credential_expiring

GitHub reports a token's expiry date in the `GitHub-Authentication-Token-Expiration`
header. `validate` prints it as `expires_at` for every credential, and `null` means
the token has no expiry date. Within `github.expiry_warning_days` of that date the
event `credential_expiring` (level `action` in the examples) is raised once a day.
Nothing stops before the expiry. Create a new token for the same `login`, replace the
variable named by `token_env`, and restart the service. An expired token then gives
`credential_invalid`.

### A fine-grained token sees nothing

`validate` can pass, because the token itself is valid, while discovery finds no
pull requests in a repository. A fine-grained token covers one resource owner, and an
organization may require an admin to approve the token before it sees anything. Check:

- The token's resource owner is the owner of the repositories of its `match`. A
  `pohunekd/*` credential needs a token of `pohunekd`.
- The organization has approved the token. A pending approval shows as an empty
  result, not as an error.
- The repository is selected in the token's repository access.

After a fix, run `autoreview poll`, then `autoreview candidates` to check that the
pull request appears.

## Notifier failures

| Code | Meaning and fix |
|---|---|
| `matrix_unavailable`, `webhook_unavailable` | The backend could not be reached. Check the URL and the network path from the autoreview host. Matrix URLs must use HTTPS (plain HTTP only on loopback). |
| `matrix_request_rejected`, `webhook_request_rejected` | The backend refused the message. Check the room, the token or the webhook URL. |

Check that the `*_env` variable is set without printing it:
`test -n "$AUTOREVIEW_WEBHOOK_URL" && echo set`. A `digest` or `log` message
that cannot be delivered is dropped with a warning. An `action` message that
cannot be delivered fails the step, and the step is retried. Test a backend
with `autoreview review-queue`, which sends the queue to `notify.review_queue.backends`.

## Approval errors

Approve with the full head SHA. If you have only a prefix from a notification,
resolve it first: `autoreview resolve --repository <owner/repo> <number> <prefix>`
(at least 12 characters) prints the full `head_sha`.

| Code | Meaning and fix |
|---|---|
| `review_not_awaiting_approval` | The review is not waiting for a human. It was already published, or its rule does not use `on_clean: await`. |
| `approval_not_required` | The rule approves without a human. Nothing to approve. |
| `review_superseded` | A newer commit replaced the review, or the pull request stopped asking for one (review request or label removed, closed, back to draft). Nothing was posted. The new head is reviewed. |
| `approval_expired` | `approval.timeout_hours` passed. Nothing was posted. See [retry and recovery](retry-and-recovery.md#expired-waits) for the open question about a fresh review. |
| `head_changed` | The HEAD_SHA given is not the pull request head any more. Resolve the current head and check the diff before you approve it. |

Approving twice is safe. Check the outcome with `autoreview review` and look for
`publication_status: published` with a `github_review_id`.

## Related pages

- [Retry and recovery](retry-and-recovery.md)
- [Stale sessions and branches](stale-sessions-and-branches.md)
- [Upgrade and backup](upgrade-and-backup.md)
- [CLI reference](../../cli.md)
- [Configuration reference](../../configuration.md)
