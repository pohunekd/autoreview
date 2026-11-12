---
type: Runbook
id: runbook/troubleshooting
title: Troubleshooting
description: Symptoms and error codes of autoreview mapped to their causes and fixes, from a pull request that is never reviewed to an approval that is refused.
source_kind: manual
intents: [debug, help]
---

# Troubleshooting

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0.

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
| `review_output_invalid`, `review_output_too_large`, `review_result_invalid` | Reviewer output was unusable, too large or broke a limit | [Reviewer output](#reviewer-output) |
| `review_adapter_outdated` | A Codex profile runs an adapter that sends no blocking flag or verdicts, or a Claude profile one that does not name the checkout it checked | [Reviewer output](#reviewer-output) |
| `codex-adapter: ...` in the session output, exit status 64, `review_result_invalid` | The Codex adapter refused the worktree it was started in | [Codex checkout refused](#codex-checkout-refused) |
| `review_result_invalid: Codex exited without a result: ...` in the log | The Codex program itself failed, for example on a model the account does not support, or the adapter refused what a Codex run left | [Codex program failed](#codex-program-failed) |
| `review_result_invalid: Claude adapter refused the run: claude-adapter: ...` in the log | The Claude adapter refused the worktree, before or after Claude Code ran | [Codex checkout refused](#codex-checkout-refused) |
| `review_wrapper_invalid`, `review_wrapper_invalid: Claude Code exited with status N: ...` in the log | A Claude result line was unusable, or Claude Code itself failed | [Claude Code failed](#claude-code-failed) |
| `claude_result_invalid`, `claude_result_mismatch` | Claude Code's result message was unusable, or its prose disagreed with its structured output; not retried | [Reviewer output](#reviewer-output) |
| `review_head_unavailable` | A reviewer on a base checkout stopped: the pull request head commit is not in the review host's repository | [Head commit unavailable](#head-commit-unavailable) |
| `github_search_limit_exceeded` | Discovery search returned more than `schedule.poll_limit` | [Search limit](#github_search_limit_exceeded) |
| `github_request_failed`, `github_request_rejected` | GitHub did not answer or refused the request | Check the token's permissions and the `github` section |
| `configuration_invalid` naming `github.credentials.login` | The token belongs to another account than the `login` of the credential | [Token of another account](#token-of-another-account) |
| `configuration_invalid` naming `github.credentials` | More than one credential, or a `match` other than `["*"]` | [Credential errors](#credential-errors) |
| A fine-grained token sees nothing | Organization approval pending, wrong resource owner, or repository not selected | [A fine-grained token sees nothing](#a-fine-grained-token-sees-nothing) |
| `engine_already_running` | Another `serve` uses the same database | Stop the other process |
| `metrics_listen_failed` | A second `serve` or another process holds `metrics.listen` | Stop it |
| `matrix_unavailable`, `matrix_request_rejected` | The Matrix homeserver did not accept a message | [Notifier failures](#notifier-failures) |
| `review_not_awaiting_approval`, `approval_not_required` | The review does not wait for a human | [Approval errors](#approval-errors) |
| `review_superseded`, `approval_expired` | The wait ended and nothing was posted | [Approval errors](#approval-errors) |
| `head_changed` | The head SHA given is no longer the pull request head | [Approval errors](#approval-errors) |
| `head_prefix_invalid`, `head_sha_invalid` | The head argument of `retry`, `resolve` or `approve` has the wrong form | [Approval errors](#approval-errors) |
| `stats_range_invalid`, `stats_range_too_long` | The range of `stats` or `review_stats` is not `YYYY-MM-DD` days in order up to today (UTC), or covers more than 366 days | [Review statistics](../../stats.md#the-range) |
| `review_already_running`, `review_retry_not_dispatched` | A retry is not needed or not possible now | [Retry and recovery](retry-and-recovery.md) |
| `review_already_completed` | `retry` found a completed review of this head (it succeeded or went on to wait for an approval) against the current base branch and no later failed review of it, or one whose base branch is no longer known | Nothing to do when that review is current; approve it if it still waits ([Approval errors](#approval-errors)); push a new commit for a fresh review |
| `pull_request_not_open`, `pull_request_draft`, `review_label_removed`, `review_request_not_active` | The pull request no longer asks for a review: closed, back in draft, label removed or review request withdrawn. A running review ends as superseded with the code, except one that resumes after a restart and looks for its own review posted before the stop: it completes as published when it finds it on the reviewed head, and fails with the error code of that lookup when the lookup fails ([resume after restart](../concepts/state-and-recovery.md#resume-after-restart)). `retry` refuses with `review_request_not_active` because the pull request is not among the candidates of its discovery pass | Check it with `autoreview candidates` ([Nothing gets reviewed](#nothing-gets-reviewed)); once it asks again, a review stopped as superseded starts anew, and a refused `retry` works once the pull request is in that list |
| `configuration_missing`, `configuration_invalid` | A key is missing or invalid; the message names it | Fix the key, then run `validate` |
| `pohunek_session_cleanup_failed` | A reviewer session could not be removed | [Stale sessions](stale-sessions-and-branches.md) |
| `workflow_failed` | A failure without a more specific code, for example a stored review that misses a part it needs; the scheduler does not retry it | Read the `review failed` log line of the review and the lines before it, run `autoreview retry`, and report it as a defect when it repeats |

Policy validation errors (for example `unreachable, owner/* matches first`) are
not error codes; they are listed in the [policy reference](../../policy.md#validation-errors).

## Nothing gets reviewed

Work through the list in this order. `autoreview candidates` answers the first
three questions directly: a pull request that is not in its `candidates` list is
not reviewed, and its `skipped` list names the ones that the author class leaves
out.

1. **Trigger.** A `review-requested` rule needs a review requested from the `login`
   of the credential that serves the repository. A `label:<name>` rule needs the
   label with that exact name.
   The next discovery pass (every `schedule.poll_interval_seconds`) picks it up.
2. **Rule.** The first rule whose `match` fits wins. `match` is lowercase
   `owner/repository` or `owner/*`; a repository in another case does not match.
   A pull request no rule matches is never reviewed (`unmatched: skip`).
3. **Author class.** An untrusted author with `untrusted: skip` is not reviewed;
   each pass logs `untrusted author skipped` with the `review_id` and `fork`, and
   `autoreview review` and the review queue report the pull request as skipped. A
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
without `autoreview retry`. One `review_stopped` message announces each host
outage: "Reviews wait for a review host", with the failed hosts and the host
error code after `Diagnostic:`. The review queue lists the review as
`AI review waits for a review host: CODE`.
Check the tunnel to the host, the pohunek versions of the CLI and the daemon, and
the `pohunek.timeout_seconds` value. See
[waiting for a review host](../concepts/state-and-recovery.md#waiting-for-a-review-host).

With `pohunek.host_state`, a host that a rule refuses is logged as
`review host skipped` with `host_state_ineligible` and the `rule`. It is asked
for its projects only when no allowed host has the repository; when it has the
repository, the review waits with `host_state_ineligible` and the wait is not
announced. A `review host state unknown` warning means that the host's daemon
could not run the `moc.host-state` command, or that its state did not read or
was too far from now, either way. A `review host state not read` warning means
that the read failed as a pohunek call: the host counts as failed and its outage
is announced as above. See
[choose a host by its state](../guides/multihost.md#choose-a-host-by-its-state).

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
| `pohunek_session_start_failed` | The host could not start a session. Check that the agent profile exists on that host and that the runtime package is installed on that host. Each start attempt uses its own branch, so the engine retries up to `session.start_max_attempts`. For a Codex reviewer whose every start fails, see [Codex input not ready](#codex-input-not-ready). |
| `pohunek_base_fetch_ref_locked` | The base fetch hit a locked ref (pohunek [#1437](https://github.com/zajca/pohunek/issues/1437)). The engine retries on a fresh `-startN` branch. Repeated failures: see [stale sessions and branches](stale-sessions-and-branches.md). |
| `pohunek_session_warning` | The session reported a warning. The engine retries on a fresh branch. Read the warning on the review host before you change the policy. |
| `pohunek_request_failed`, `pohunek_request_timeout` | The pohunek call failed or passed `pohunek.timeout_seconds`. A session start includes the base fetch, so 45 seconds is the recommended minimum. |
| `pohunek_session_lost` | Pohunek reports the session's runtime as lost during the start. The log line `reviewer session start failed` names pohunek's loss cause as the `diagnostic`, for example `host_restarted`. The engine retries on a fresh branch. A leftover session that cannot be removed raises `cleanup_manual`, see [when a session cannot be removed](stale-sessions-and-branches.md#when-a-session-cannot-be-removed). Check the review host's resources and the pohunek journal, see [review host checks](#review-host-checks). |
| `pohunek_base_fetch_timeout` | Git on the review host did not finish the base fetch within pohunek's Git timeout. The engine retries. Check the network, then the SSH agent and the credential helper, see [review host checks](#review-host-checks). |
| `pohunek_base_fetch_authentication_failed` | Git on the review host could not authenticate to GitHub. It is not retried. Check the SSH agent and the credentials, see [review host checks](#review-host-checks). |

The log line `reviewer session start failed` carries the code above as
`error_code` and the cause as `diagnostic`: the error code of pohunek's JSON
error when it matches `^[a-z][a-z0-9_]{0,63}$`, the loss cause for
`pohunek_session_lost`, otherwise an allowlisted class such as
`unclassified_cli_failure`. Nothing else from pohunek's output is logged.

| Diagnostic | Cause and fix |
|---|---|
| `session_input_not_ready` | The reviewer program gave pohunek no evidence that it is ready for input within pohunek's input grace (`input.initial_startup_grace_ms`), so pohunek did not write the request and refused the start. Every attempt fails the same way. The profile's program must show the input readiness that its pohunek runtime declares; for a Codex reviewer, see [Codex input not ready](#codex-input-not-ready). |

When all start attempts fail, the review reaches `review_stopped`. See
[retry and recovery](retry-and-recovery.md).

### Codex input not ready

Every start of a Codex reviewer fails with `pohunek_session_start_failed`, and
the log line `reviewer session start failed` carries
`diagnostic=session_input_not_ready`, pohunek's error code. Each attempt waits
for pohunek's whole input grace (`input.initial_startup_grace_ms`), then the
review reaches `review_stopped` after `session.start_max_attempts`. A hand-run
`pohunek session new --agent <profile> --input-stdin` on the review host, or the
pohunek daemon log, shows the same code with pohunek's message, for example
`agent did not show an editable prompt within 5000 ms`.

The Codex runtime declares `manifest_idle` input readiness, so pohunek writes
the request only after the session shows an idle terminal title or screen. The
adapter in `adapters/codex/` sets a fixed title before it reads its request; a
profile whose program sets no title, such as an adapter written by an older
autoreview or a wrapper of your own, never becomes ready. Neither does a program
that exits before its title because it refuses the `--no-daemon` that pohunek
appends to every Codex launch. Run
`autoreview profiles install` with the same flags on the review host (an upgrade
from 0.3.0 or earlier adds `--pohunek-timeout-seconds`; with
`--replace` when the command did not write the profile; it prints `updated`
when it rewrote only the adapter), then `autoreview retry` for the pull
request. See [the Codex adapter](../concepts/reviewers-and-adapters.md#the-codex-adapter).

### Codex checkout refused

A Codex review keeps failing with `review_result_invalid`, and the session
output holds a `codex-adapter: ...` line and ends with
`[pohunek-codex-review exited with status 64]` without a result frame. A
refusal after the Codex run also puts that message on the reason line right
before the exit line (see [Codex program failed](#codex-program-failed)), so
the engine logs it. The engine reads that output at once, starts a fresh
session up to `session.reviewer_run_max_attempts`, then records the failure.
The adapter checks the worktree against the trusted lines of the request before
and after the Codex run. The Claude adapter runs the same `base` checks with
the same messages, and on a `head` checkout only the `HEAD` check; its refusal
is one result line whose `stderr_base64` holds the message. Every refusal but a
missing head is `review_result_invalid`:

| Message | Cause | Fix |
|---|---|---|
| `review checkout HEAD does not match the trusted pull request head` | A `head` request in a worktree at another commit. A request without the `- Trusted checkout:` line counts as `head`, and the Go engine writes none, so on Go a `checkout: base` pipeline fails here. | Run the Rust engine, or give Codex `checkout: head` while Go runs |
| `trusted pull request head is unavailable in the review checkout` | A `base` request whose head commit is not in the repository; the review stops with `review_head_unavailable` | See [Head commit unavailable](#head-commit-unavailable) |
| `review checkout HEAD does not descend from the trusted pull request base`, `trusted pull request base is unavailable in the review checkout` | The base branch was force-pushed since GitHub recorded the pull request's base | A push to the pull request (or a rebase) records the new base; then `autoreview retry` |
| `review checkout HEAD is the pull request head or contains it` | A `base` request in a worktree at the pull request code | Check the pipeline's `checkout` and the session's branch on the review host |
| `review checkout is not clean`, `review checkout HEAD changed during the review` | Something changed the worktree before or during the run | Find the process that writes into the reviewer worktree |

For a Claude refusal, the engine logs the message in the `error` field of the
warnings "retrying reviewer after unusable output" and "rejecting reviewer
output", as `review_result_invalid: Claude adapter refused the run:
claude-adapter: <message>`, bounded and neutralized as in
[Claude Code failed](#claude-code-failed), and nowhere else: the recorded
failure, the notifications and GitHub get only the code. A missing head stays
`review_head_unavailable` without a reason. Every Claude adapter already
writes the message, so no `autoreview profiles install` is needed for it.

### Codex program failed

A Codex review keeps failing with `review_result_invalid`, and the session
output ends with the lines `[pohunek-codex-review-reason-v1:<base64>]` and
`[pohunek-codex-review exited with status N]`, N not 0, without a result frame:
Codex itself failed, for example on a model that the account does not support
or on an argument that its version does not know. The adapter prints the reason
line whenever Codex exits with a status other than 0. Its reason is the last
line of Codex's stderr that starts with `ERROR:` or `error:`, else the last
line that is not blank, with control characters, format characters and line or
paragraph separators shown as `?`, cut to 512 bytes at a character boundary.
The engine logs it in the `error` field of the warnings "retrying
reviewer after unusable output" and "rejecting reviewer output", as
`review_result_invalid: Codex exited without a result: <reason>`, and nowhere
else: the recorded failure, the notifications and GitHub get only the code. The
reason comes from Codex's output, so read it as a hint, not as a verdict. Fix
what it names (the `model` of the profile, the Codex login on the review host,
the Codex version), then `autoreview retry` the pull request.

The adapter prints the reason line with exit status 64 too when it refuses what
a Codex run with status 0 left: no report (`Codex did not produce a review
result`), a report that is not valid or names another head, or a checkout that
changed during the run (see [Codex checkout refused](#codex-checkout-refused)).
The reason is then its own message, `codex-adapter: <message>`, and the log
shows `review_result_invalid: Codex exited without a result: codex-adapter:
<message>`. When the head commit is missing after the run, the head marker
still comes last, right before the exit line, and the review stops with
`review_head_unavailable`. A profile whose adapter was written by an older
autoreview prints no reason line; run `autoreview profiles install` with the
same flags on the review host to write the adapter in `adapters/codex/`.

### Claude Code failed

A Claude reviewer or verifier keeps failing with `review_wrapper_invalid`: the
adapter's result line reports an exit code other than 0, output on stderr, or
another protocol or mode. When Claude Code exited with a status other than 0,
the engine logs the end of its stderr in the `error` field of the warnings
"retrying reviewer after unusable output" and "rejecting reviewer output", as
`review_wrapper_invalid: Claude Code exited with status N: <reason>`, and
nowhere else: the recorded failure, the notifications and GitHub get only the
code. The reason is the last 512 bytes of Claude Code's stderr, without blank
space at either end, starting at a character boundary, with line breaks,
control characters, format characters and line or paragraph separators shown
as `?`. It comes from Claude Code's output, so read it as a hint, not as a
verdict. Fix what it names (the Claude Code login on the review host, the
`model` of the profile, the Claude Code version), then `autoreview retry` the
pull request. A bare `review_wrapper_invalid` has no such reason: for example a
run with status 0 that wrote to stderr, a failed run with an empty stderr, an
unreadable result line, or one of another protocol or mode with status 0. The
result line in the session output holds the whole stderr in `stderr_base64`.
The engine reads the stderr that every Claude adapter already sends, so no
`autoreview profiles install` is needed for this reason.

### Head commit unavailable

A review stops with `review_head_unavailable` and publishes nothing. A
reviewer on a `checkout: base` pipeline, Claude or Codex, reads the change
through git, so the pull request head commit must be in the project repository
on the review host. Pohunek fetches only the base branch for a base checkout,
so the head of a fork pull request, or of a branch pushed after the project
repository last fetched, is missing. The adapter refuses the run before the
reviewer starts: the Claude adapter with a result line whose `refusal` is
`head_unavailable`, the Codex adapter with the line
`[pohunek-codex-review-refused-v1:head_unavailable]` right before its exit
line. Neither a fresh session nor the scheduler tries again, because the next
run would find the same repository. Fetch the head on the review host, then
retry the pull request:

```sh
git -C <project repository> fetch origin refs/pull/<n>/head
autoreview retry --repository <owner>/<repo> <n> <head prefix>
```

`<head prefix>` is 12 to 40 lowercase hexadecimal characters of the head SHA,
such as the head in the notification's Review ID; any other form is
`head_prefix_invalid`.

A new push to the pull request starts a new review, which stops the same way
until its head is in the repository.

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

A reviewer run ends in one of these codes. Where a code below ends the review,
a run of the verifier ends only the verification (see the paragraph after the
list):

- `review_output_timeout`: the session ran longer than `session.timeout_seconds`
  (default in the recommended config: 7200). Check the diff size and the model
  on the review host before you raise the timeout.
- `review_output_invalid`: the complete output broke an engine check: no
  findings list, another pull request or head, the summary length, the count of
  findings or tests (`max_findings`, `max_tests`), a finding's id, side, line,
  path, title, body, confidence or severity, a test's text, or a verdict on a
  prior finding. The recorded review failure carries only the code. The failed
  check is named in the `error` field of two log warnings: "retrying reviewer
  after unusable output" before a fresh run, and "rejecting reviewer output"
  when the run limit is used up. A broken prior-finding verdict and a verifier
  output that breaks a check have no reason there. The request of the reviewer's
  fresh run adds one fixed sentence: the output check `review_output_invalid`
  refused the run before. It carries neither the reason nor any text of the
  refused output, and no other code in this list adds a sentence. For a broken
  limit, tighten the prompt so the reviewer stays within it, or raise the bound
  in `limits` with care. The same code also comes from the review host: its
  answer to a read of the session output could not be used (for example its
  `ok` value is `null`, it names another session, worker or runtime generation,
  its history does not start at offset 0, or its offsets, data and `has_more`
  flag disagree). The review then fails at once, without a fresh session, and
  the log has only "review failed" with the code.
- `review_output_too_large`: the session output grew past
  `session.output_max_bytes` (at most 16 MiB) before it was complete. The
  review fails at once, without a fresh session. Codex keeps its transcript in
  the output, so raise that bound for Codex.
- `review_result_invalid`: a Codex result could not be read, is larger than
  `result_max_bytes`, or reports another head or a status other than
  `completed`, or the adapter printed no result frame, as when it refuses the
  checkout (see [Codex checkout refused](#codex-checkout-refused)) or Codex
  itself fails (see [Codex program failed](#codex-program-failed)). A Claude
  adapter refusal other than a missing head is `review_result_invalid` too.
- `review_wrapper_invalid`: a Claude result line could not be read, or reports
  an exit code other than 0, output on stderr, or another protocol or mode (see
  [Claude Code failed](#claude-code-failed)).
- `claude_result_invalid`: the Claude Code output in a usable result line is
  not one successful result message, for example not a JSON object, another
  `type` or `subtype`, `is_error` set, or a structured output that does not
  decode as a review. `claude_result_mismatch`: its prose result is a JSON
  object or `null` that does not read as the same review as its structured
  output. Neither gets a fresh session: "rejecting reviewer output" logs the
  code and the review fails at once. Check the Claude Code version on the
  review host, then `autoreview retry` the pull request.
- `review_head_unavailable`: the adapter refused a base checkout run because
  the pull request head commit is not in the review host's repository (see
  [Head commit unavailable](#head-commit-unavailable)). The review stops
  without a fresh session.
- `review_adapter_outdated`: a Codex report has no `prior_findings` list or a
  finding without `blocking`, or a Claude result line of protocol
  `claude-otel-noninteractive/v2` without a `refusal` has no `checked_checkout`
  or names another checkout than the pipeline's. It comes from an adapter older
  than the one in `adapters/codex/` or `adapters/claude/`, so the review stops
  without a fresh session. Write the profile with `autoreview profiles install`
  on the review host (with `--replace` when the command did not write it), then
  run `autoreview retry` for the pull request.

A run that ends in `review_output_invalid` after a complete output,
`review_result_invalid` or `review_wrapper_invalid`, or with an empty Claude
output (`review_output_incomplete`), gets a fresh session, up to
`session.reviewer_run_max_attempts`. Every other code in this list, and a
retried code once the run limit is used up, ends the review at once. For the
verifier, the same codes end only the verification: the claims are published
unverified, the log has "verification did not finish, publishing the claims
unverified" with the code instead of "review failed", and the review goes on
(see [verification](../concepts/verification.md#when-the-verifier-fails)). If
the same reviewer keeps failing, compare its agent profile and its prompt with
the protocol named in `policy.yaml`.

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
| `head_prefix_invalid` | The head prefix of `retry` or `resolve` is not 12 to 40 lowercase hexadecimal characters. Copy at least the 12 characters after `@` of the Review ID. |
| `head_sha_invalid` | The HEAD_SHA of `approve` is not the full head, 40 lowercase hexadecimal characters. Run `autoreview resolve` with the prefix and approve the `head_sha` it prints. |

Approving twice is safe. Check the outcome with `autoreview review` and look for
`publication_status: published` with a `github_review_id`.

## Related pages

- [Retry and recovery](retry-and-recovery.md)
- [Stale sessions and branches](stale-sessions-and-branches.md)
- [Upgrade and backup](upgrade-and-backup.md)
- [CLI reference](../../cli.md)
- [Configuration reference](../../configuration.md)
