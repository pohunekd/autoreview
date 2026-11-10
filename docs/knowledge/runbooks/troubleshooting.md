---
type: Runbook
id: runbook/troubleshooting
title: Troubleshooting
description: Symptoms and error codes of autoreview mapped to their causes and fixes, from a pull request that is never reviewed to an approval that is refused.
source_kind: manual
intents: [debug, help]
---

# Troubleshooting

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

Start with two commands. `autoreview candidates` shows what the next discovery
pass would consider, per rule and trigger. `autoreview review --repository
OWNER/REPO NUMBER` shows everything the engine knows about one pull request. Every
error is a JSON object with `"error": "<code>"` on stdout and exit status 1; the
codes below are the ones in the [CLI reference](../../cli.md#error-codes).

## Symptom table

| Symptom or code | Usual cause | Fix |
|---|---|---|
| Pull request never appears | Trigger not met, no rule matches, or it is a draft | [Nothing gets reviewed](#nothing-gets-reviewed) |
| `project_route_not_found` | Repository is not registered on an answering host in `pohunek.hosts` | [Route errors](#project_route_not_found) |
| `project_route_ambiguous` | Registered twice on the host that serves it, no `route_overrides` entry | [Route errors](#project_route_ambiguous) |
| `pohunek_session_start_failed` | Review host could not start the session | [Session start failures](#session-start-failures) |
| `pohunek_session_lost` | The session worker was lost while the session started | [Review host checks](#review-host-checks) |
| `pohunek_base_fetch_timeout`, `pohunek_base_fetch_authentication_failed` | Git on the review host could not fetch the base: no network, a hung SSH agent or a locked credential helper | [Review host checks](#review-host-checks) |
| `pohunek_base_fetch_ref_locked`, `pohunek_session_warning` | Base fetch on a locked ref, or a session warning | [Session start failures](#session-start-failures) |
| `pohunek_request_failed`, `pohunek_request_timeout` | Review host did not answer | [Session start failures](#session-start-failures) |
| `review_output_timeout` | Reviewer did not finish within `session.timeout_seconds` | [Reviewer output](#reviewer-output) |
| `review_output_invalid`, `review_result_invalid` | Reviewer output was unusable or broke a limit | [Reviewer output](#reviewer-output) |
| `github_search_limit_exceeded` | Discovery search returned more than `schedule.poll_limit` | [Search limit](#github_search_limit_exceeded) |
| `github_request_failed`, `github_request_rejected` | GitHub did not answer or refused the request | Check the token's permissions and the `github` section |
| `configuration_invalid` naming `github.credentials.login` | The token belongs to another account than the `login` of the credential | [Token of another account](#token-of-another-account) |
| `configuration_invalid` naming `github.credentials` | More than one credential, or a `match` other than `["*"]` | [Credential errors](#credential-errors) |
| A fine-grained token sees nothing | Organization approval pending, wrong resource owner, or repository not selected | [A fine-grained token sees nothing](#a-fine-grained-token-sees-nothing) |
| `engine_already_running` | Another `serve` uses the same database | Stop the other process |
| `matrix_unavailable`, `matrix_request_rejected` | The Matrix homeserver did not accept a message | [Notifier failures](#notifier-failures) |
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
3. **Author class.** An untrusted author with `untrusted: skip` is not reviewed;
   each pass logs `untrusted author skipped` with the `review_id` and `fork`. A
   fork pull request is untrusted unless its author is in `logins`. When the
   author's repository permission cannot be read, the pull request is not started
   and the `review_stopped` notice names the read to check with the engine's
   token, `gh api repos/<owner/repo>/collaborators/<login>/permission`; the next
   pass reads it again. See [untrusted authors](../safety/untrusted-authors.md).
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

A host that does not answer is skipped and logged as `review host skipped` with
the host and the code (`pohunek_request_failed`, `pohunek_request_timeout`). When
no later host has the repository, the review waits for a review host, not
`project_route_not_found`: it uses none of
[`schedule.retry_delays_seconds`](../../configuration.md#schedule), is tried again
on every discovery pass, and starts on the first pass after a host answers,
without `autoreview retry`. One `review_stopped` message announces the outage:
"Reviews wait for a review host", with the host error code after `Diagnostic:`.
Check the tunnel to the host, the pohunek versions of the CLI and the daemon, and
the `pohunek.timeout_seconds` value. See
[waiting for a review host](../concepts/state-and-recovery.md#waiting-for-a-review-host).

### project_route_ambiguous

The repository is registered as two or more projects on the host that serves
it, and no route pins it. A registration on two different hosts is not
ambiguous: the first host of `pohunek.hosts` runs the review. Either remove the
duplicate registration, or pin the repository:

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
| `pohunek_session_start_failed` | The host could not start a session. Check that the agent profile exists on that host and that the runtime package is installed on that host. Each start attempt uses its own branch, so the engine retries up to `session.start_max_attempts`. |
| `pohunek_base_fetch_ref_locked` | The base fetch hit a locked ref (pohunek [#1437](https://github.com/zajca/pohunek/issues/1437)). The engine retries on a fresh `-startN` branch. Repeated failures: see [stale sessions and branches](stale-sessions-and-branches.md). |
| `pohunek_session_warning` | The session reported a warning. The engine retries on a fresh branch. Read the warning on the review host before you change the policy. |
| `pohunek_request_failed`, `pohunek_request_timeout` | The pohunek call failed or passed `pohunek.timeout_seconds`. A session start includes the base fetch, so 45 seconds is the recommended minimum. |
| `pohunek_session_lost` | Pohunek reports the session's runtime as lost during the start. The log line `reviewer session start failed` names pohunek's loss cause as the `diagnostic`, for example `host_restarted`. The engine retries on a fresh branch. A leftover session that cannot be removed raises `cleanup_manual`, see [when a session cannot be removed](stale-sessions-and-branches.md#when-a-session-cannot-be-removed). Check the review host's resources and the pohunek journal, see [review host checks](#review-host-checks). |
| `pohunek_base_fetch_timeout` | Git on the review host did not finish the base fetch within pohunek's Git timeout. The engine retries. Check the network, then the SSH agent and the credential helper, see [review host checks](#review-host-checks). |
| `pohunek_base_fetch_authentication_failed` | Git on the review host could not authenticate to GitHub. It is not retried. Check the SSH agent and the credentials, see [review host checks](#review-host-checks). |

When all start attempts fail, the review reaches `review_stopped`. See
[retry and recovery](retry-and-recovery.md).

### Review host checks

Run these on the review host, as the user that runs the pohunek daemon.

**inotify watches.** systemd watches every new session worker. When another
process holds almost all of `fs.inotify.max_user_watches`, the worker is stopped
seconds after it starts, and the start fails with `pohunek_session_lost`,
`pohunek_session_cleanup_failed` or a fetch timeout. Compare the limit with the
watches in use, which the second command lists per process (count, name, PID),
then stop the process that holds them or raise the limit:

```sh
sysctl fs.inotify.max_user_watches
for p in /proc/[0-9]*; do
  n=$(cat "$p"/fdinfo/* 2>/dev/null | grep -c '^inotify')
  [ "$n" -gt 0 ] && echo "$n $(cat "$p/comm" 2>/dev/null) ${p#/proc/}"
done | sort -rn | head
```

Without root, the count covers only your own processes. Also check free memory
and the pohunek daemon journal for stopped workers.

**Git access without a prompt.** The daemon fetches the base branch with the
project's `origin` remote and nothing can answer a prompt. Run in the project's
repository:

```sh
GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND='ssh -o BatchMode=yes' \
  timeout 30 git ls-remote origin HEAD
```

It must print a commit within the 30 seconds that pohunek gives Git. A hang
means an SSH agent that waits for an unlock or a confirmation, or a credential
helper that waits for a locked keyring. `Permission denied (publickey)` means
the agent holds no key that GitHub accepts, or it refused to sign.

**HTTPS with a credential helper.** Reviews then do not depend on an unlocked
SSH agent. Give the daemon its own Git configuration file that includes the
user's configuration and rewrites the GitHub SSH URL, and point the daemon's
`GIT_CONFIG_GLOBAL` at it, for example in a systemd drop-in of its unit:

```ini
# <daemon-git-config>, the file GIT_CONFIG_GLOBAL names
[include]
    path = <home>/.gitconfig
[url "https://github.com/"]
    insteadOf = git@github.com:
[credential "https://github.com"]
    helper = <a helper that answers without a prompt>
```

Pohunek removes `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*` and `GIT_CONFIG_VALUE_*`
from the environment of its Git children, so a rewrite passed in those variables
has no effect; `GIT_CONFIG_GLOBAL` is kept. Run the `git ls-remote` check
again with `GIT_CONFIG_GLOBAL` set to the file.

## Reviewer output

A reviewer run ends in one of these codes:

- `review_output_timeout`: the session ran longer than `session.timeout_seconds`
  (default in the recommended config: 7200). Check the diff size and the model
  on the review host before you raise the timeout.
- `review_output_invalid`: the output could not be read as a reviewer result,
  or it broke an engine check: no findings list, another pull request or head,
  the summary length, the count of findings or tests (`max_findings`,
  `max_tests`), a finding's id, side, line, path, title, body, confidence or
  severity, a test's text, or a verdict on a prior finding. The recorded review
  failure carries only the code. The failed check is named in the `error` field
  of two log warnings: "retrying reviewer after unusable output" before a fresh
  run, and "rejecting reviewer output" when the run limit is used up. An
  unreadable output and a broken prior-finding verdict have no reason there.
  Common cause of an unreadable output: an output larger than
  `session.output_max_bytes`. Codex keeps its transcript in the output, so
  raise that bound for Codex. For a broken limit, tighten the prompt so the
  reviewer stays within it, or raise the bound in `limits` with care.
- `review_result_invalid`: a Codex result could not be read, is larger than
  `result_max_bytes`, or reports another head or a status other than
  `completed`.

Each unusable run gets a fresh session, up to `session.reviewer_run_max_attempts`.
If the same reviewer keeps failing, compare its agent profile and its prompt
with the protocol named in `policy.yaml`.

## github_search_limit_exceeded

A discovery search returned more than `schedule.poll_limit` results. The
engine stops rather than drop pull requests. Narrow the work: use a more
specific `match` or a label trigger, or split a large organization into
rules. Raise `schedule.poll_limit` only when the narrowing is not enough.

## Credential errors

The loader accepts exactly one credential, with `match: ["*"]`. Any other list
is refused with `configuration_invalid` naming `github.credentials`; several
credentials are planned in #13. An empty or unset token variable is refused at
load with `configuration_missing` naming `github.credentials.token_env`.

Check that a variable is set without printing its value:
`test -n "$AUTOREVIEW_GH_TOKEN" && echo set`. Use the variable name from the
credential's `token_env`.

### Token of another account

`serve` asks GitHub for the token's login before it starts. When the token
belongs to another account than the `login` of the credential, it refuses with
`configuration_invalid` naming `github.credentials.login`, and the logged error
names the token's login. Either correct `login`, or replace the token with one
of that account. A token that GitHub refuses stops `serve` with the error of
that call. A startup check of every token is planned in #15, and checks while
running in #19.

To see which login a token belongs to, load the environment file into a shell
without printing it, then ask GitHub with the token of that variable. Never print the
token itself. autoreview passes the token only to its `gh` children, as
`GH_TOKEN`, and ignores `GH_TOKEN` in its own environment:

```sh
set -a; . /etc/autoreview/autoreview.env; set +a
GH_TOKEN="$AUTOREVIEW_GH_TOKEN" gh api user --jq .login
```

### A fine-grained token sees nothing

`serve` can start, because the token itself is valid, while discovery finds no
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
| `matrix_unavailable` | The homeserver could not be reached. Check the URL and the network path from the autoreview host. Matrix URLs must use HTTPS (plain HTTP only on loopback). |
| `matrix_request_rejected` | The homeserver refused the message. Check the room and the token. |

Check that the `*_env` variable is set without printing it:
`test -n "$AUTOREVIEW_MATRIX_TOKEN" && echo set`. A `digest` or `log` message
of a review that cannot be delivered is dropped with the warning
`matrix notification dropped`. An `action` message of a review gets one
attempt and is then dropped with the warning `review notification dropped`,
which names the event, the review key, the level and the code (`shutdown` when
the service stopped during the attempt). The review goes on and does not resend
it; `autoreview awaiting` still lists a review that waits for approval. A message
of a discovery pass, at any level, gets at most one attempt (none after an
earlier failure of its route in the same pass, none after a shutdown, which
logs the code `shutdown`) and is then dropped with the warning
`discovery notification dropped`, which names the event, the review key, the
level, the code and `skipped_after_failure`. It is never resent; send its
content by hand if it matters. Every retry of a
step is logged as `step failed, retrying` with the step, the code and `delay_ms`. Test a backend
with `autoreview review-queue`, which sends the queue to `notify.review_queue.backends` once and
exits with the backend's error code, logged as `review queue digest dropped`, when it fails.

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
