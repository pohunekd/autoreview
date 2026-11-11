---
type: Reference
id: reference/cli
title: CLI reference
description: Every autoreview command - init, serve, mcp, validate, poll, candidates, reviews, review, awaiting, stats, approve, resolve, retry, review-queue and backup-db - with arguments and JSON output.
source_kind: manual
intents: [setup, debug, help]
---

# CLI reference

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0.

```
autoreview [--config <path>] <command> [arguments]
```

Secrets come only from the variables named by the `*_env` keys of the configuration.

Every command except `init` and `profiles install` reads the configuration from `--config` or
`AUTOREVIEW_CONFIG` and validates it together with the policy before doing
anything. Output is
one JSON object (or array) on stdout, except the text summary of `stats` without
`--json`; logs are JSON lines on stderr. A failed
command prints `{"error": "<code>", ...}` with a stable error code and exits
with status 1, so scripts and agents can branch on the code.

## Setup

### `autoreview init`

```
autoreview init --dir <DIR> --login <LOGIN> --pohunek-host <HOST> --example <NAME> \
  [--store-path <PATH>] [--gh-bin <PATH>] [--pohunek-bin <PATH>] \
  [--pohunek-state-home <PATH>] [--pohunek-runtime-dir <PATH>] \
  [--matrix-homeserver-url <URL>] [--matrix-room <ROOM>] [--matrix-mention-user <USER>]
```

Writes a new configuration directory from one of the [examples](../examples/README.md).
It asks nothing and reads no configuration; `--config` is refused. These flags are
required:

| Flag | Meaning |
|---|---|
| `--dir` | Clean absolute directory whose parent exists; it and `prompts/` are created when absent |
| `--login` | Lowercase GitHub login of the one credential |
| `--pohunek-host` | The pohunek host that runs the reviewers |
| `--example` | The example directory under `examples/`, for example `single-repo-comment` |

The layout flags are optional and come after 0.2.0. Each one replaces one value of
the reference file; without it the reference value is written:

| Flag | Key | Reference value |
|---|---|---|
| `--store-path` | `store.path` | `/var/lib/autoreview/autoreview.db` |
| `--gh-bin` | `github.gh_bin` | `/usr/bin/gh` |
| `--pohunek-bin` | `pohunek.bin` | `/usr/local/bin/pohunek` |
| `--pohunek-state-home` | `pohunek.state_home` | `/var/lib/autoreview/.local/state` |
| `--pohunek-runtime-dir` | `pohunek.runtime_dir` | `/run/autoreview` |
| `--matrix-homeserver-url` | `homeserver_url` of the `matrix` backend | `https://matrix.example.org` |
| `--matrix-room` | `room` of the `matrix` backend | `!roomid:example.org` |
| `--matrix-mention-user` | `mention_user` of the `matrix` backend | `@you:example.org` |

A path flag must be a clean absolute path; a Matrix flag must be non-empty text
without control characters. `init` checks neither for existence.

It writes `config.yaml`, `policy.yaml`, `prompts/generic.md`, `prompts/verifier.md`
and the prompts the example adds. `config.yaml` is `examples/config.yaml`, the
complete file of the [configuration reference](configuration.md#complete-file),
with `policy_file`, the credential `login`, `pohunek.hosts` and the given layout
flags set; every value it sets is written as a double-quoted string. Without the
layout flags, edit its paths and its placeholder `matrix` backend before the
first start. [`scripts/setup`](knowledge/guides/getting-started.md) (after 0.2.0)
passes every layout flag.

`init` loads the example's policy and checks every path before it writes
anything, and never overwrites. `--dir` and `prompts/` must be real directories
or absent: a symlink, a file or any other entry at either path is in the way, as
is any existing file of the set. `init` creates `--dir` and then `prompts/`, one
level at a time; it never creates the parent of `--dir`. When a later step
fails, it removes the files and directories it created in that run, and nothing
it did not create.

| Failure | Code | `configuration` field |
|---|---|---|
| `--config` given, a flag missing, an unknown example | `command_invalid` | none |
| `--dir` relative or unclean, or its parent missing; `--login`, `--pohunek-host` or a layout flag malformed | `configuration_invalid` | the flag |
| A path of the set is in the way | `init_file_exists` | the path |
| A path cannot be checked, created or written (permissions, disk full) | `init_write_failed` | the path |

On success it prints:

```json
{
  "directory": "/home/you/autoreview",
  "files": ["/home/you/autoreview/config.yaml", "..."],
  "agent_profiles": ["autoreview-codex"]
}
```

`agent_profiles` lists the profiles the policy needs, as `validate` prints them.

### `autoreview profiles install`

```
autoreview profiles install --host <HOST> --agent claude|codex --name <NAME> \
  --model <MODEL> --effort <EFFORT> --agent-binary <PATH> --maximum-input-bytes <BYTES> \
  [--maximum-result-bytes <BYTES> --maximum-retention-seconds <SECONDS>] [--replace]
```

Writes the pohunek agent profile `<NAME>` of a reviewer, and the reviewer
adapter it starts, on the machine it runs on. Run it on each review host, as
the user of its pohunek daemon, once per profile that `validate` lists. It
reads no configuration; `--config` is refused. Every value is a flag without a
default:

- `--host`: the pohunek host name of this machine, as `pohunek.hosts` lists it.
  It is local when `pohunek host governance inspect <HOST> --json` reports the
  `host_id` of `pohunek host governance inspect local --json`; `local` itself is
  accepted. pohunek has no command that writes a profile on another host.
- `--agent`: `claude` for `claude-structured` reviewers, `codex` for
  `codex-envelope` reviewers.
- `--name`: the profile name, as `reviewers.<id>.agent_profile` names it.
- `--model`: the model of the review run, `[A-Za-z0-9][A-Za-z0-9._-]*`.
- `--effort`: `low`, `medium`, `high`, `xhigh` or, for Claude only, `max`.
- `--agent-binary`: the absolute path of the Claude Code or Codex executable.
  It must be a regular file that the user running the command may execute,
  after links are followed, and must not contain a control character. The
  execute check is the same `access(X_OK)` that the adapter makes, so owner,
  groups (supplementary ones included) and ACLs count as the kernel counts them.
  The path is kept as given, so a link that a self-updating agent moves is
  followed at each run.
- `--maximum-input-bytes`: the largest request the adapter accepts. A re-review
  request also carries the prior findings and the discussion (bounded by
  `limits.max_discussion_characters`), so the value must cover them.
- `--maximum-result-bytes` (Codex only, required there): the largest report the
  adapter prints. Keep it at most `limits.result_max_bytes`, which the engine
  enforces.
- `--maximum-retention-seconds` (Codex only, required there): how long the
  adapter stays alive after its report, so that the engine can read it. Use at
  least `session.timeout_seconds`.
- `--replace`: overwrite a file of that name that this command did not write.

A Codex flag with `--agent claude`, or a missing one with `--agent codex`, is
`command_invalid`.

It calls the `pohunek` found on `PATH` for the host check, once when `--host`
is `local` and twice otherwise, and once for `pohunek plugin list --json`. The
base runtime must be served by exactly one official package that is enabled,
selected and healthy; the profile pins its package id and digest. `<config>` is `$XDG_CONFIG_HOME`, or `$HOME/.config`
when that is unset, and `<data>` is `$XDG_DATA_HOME`, or `$HOME/.local/share`.
It writes these files, and creates their missing directories with mode 0700.
`<config>/pohunek` must exist; the command creates only `agents` and
`agents/manifests` in it, so a missing one is `profile_write_failed` naming
the agents directory:

| File | Mode | Content |
|---|---|---|
| `<data>/autoreview/adapters/<NAME>/claude-adapter` or `codex-adapter` | 0700 | The adapter of `adapters/claude/` or `adapters/codex/`, embedded in this binary |
| `<data>/autoreview/adapters/<NAME>/claude-adapter.claude-binary` or `codex-adapter.codex-binary` | 0600 | The `--agent-binary` path, which the adapter reads |
| `<config>/pohunek/agents/manifests/<NAME>.toml` (Claude only) | 0600 | An empty detection manifest, because the adapter renders no prompt box |
| `<config>/pohunek/agents/<NAME>.toml` | 0600 | The profile, written last |

The profile is the one that the adapter's `render_profile.py` writes from a
base profile with the same package pin and no `[env]` entries: the adapter as
`program`, its arguments, bracketed-paste input and no resuming. A Claude
profile passes `--permission-mode plan`, which blocks the edit tools; what else
keeps the run read-only is in
[reviewers and adapters](knowledge/concepts/reviewers-and-adapters.md#agent-profiles). Its
`[env]` table is empty: the agent runs with the daemon's environment and its
default config home, and with no telemetry settings. A profile with telemetry
or a second config home is written with the adapter's `render_profile.py`,
which copies only those keys of a base profile (`OTEL_*`,
`CLAUDE_CODE_ENABLE_TELEMETRY`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`) and names
every dropped key. pohunek ignores every
profile when `agents` or `agents/manifests` is writable by the group or others,
so such a directory, like such a `<data>/autoreview`, `<data>/autoreview/adapters`
or adapter directory, is `profile_write_failed`.

The first line of the profile and of the manifest marks the file as written by
this command and holds the SHA-256 of the rest of the file without its
`package` and `digest` lines, the two keys that pohunek rewrites when it repins
the package (`plugin profile migrate`, `service upgrade`). Each file is decided
before anything is written:

| File | Without `--replace` | With `--replace` |
|---|---|---|
| absent | written | written |
| the planned bytes and mode | kept | kept |
| profile or manifest, marked, and the digest matches its body | rewritten | rewritten |
| profile or manifest without a marker, or with a body edited after it was written | `profile_foreign` | rewritten |
| adapter or path file with other bytes or another mode | rewritten | rewritten |
| a link, a directory or another non-regular file | `profile_foreign` | `profile_foreign` |

A refused file leaves every file as it was. A rewrite goes through a temporary
file with a name of its own next to it and a rename, so pohunek never reads a
partial file and a temporary file left by a killed run does not block the next
one. When a later step fails, the command gives every file it rewrote its
earlier bytes and mode back and removes the files and directories it created,
so a working profile and its path file stay consistent. The
`result` is `replaced` when `--replace` overwrote a file, `created` when the
profile was absent, `updated` when another file was written, and `unchanged`
when every file was kept, so a second run with the same flags prints
`unchanged`.

| Failure | Code | `configuration` field |
|---|---|---|
| `--config` given, a flag missing, an unknown agent or effort, a Codex flag that does not fit `--agent` | `command_invalid` | none |
| `--host`, `--name`, `--model`, `--effort` or `--agent-binary` refused; `XDG_CONFIG_HOME`, `XDG_DATA_HOME` or `HOME` relative, or neither set | `configuration_invalid` | the flag or the variable |
| A pohunek call fails or prints unexpected JSON | `pohunek_command_failed` | the pohunek command |
| `--host` names another machine or cannot be inspected | `profile_host_not_local` | `--host` |
| No single official, enabled and selected package serves the runtime | `runtime_package_missing` | the runtime; the message names the install command |
| A file of that name that this command did not write | `profile_foreign` | the path |
| A path cannot be checked, created or written | `profile_write_failed` | the path |

On success it prints:

```json
{
  "host": "review-host",
  "name": "autoreview-sonnet",
  "path": "/home/you/.config/pohunek/agents/autoreview-sonnet.toml",
  "program": "/home/you/.local/share/autoreview/adapters/autoreview-sonnet/claude-adapter",
  "adapter_sha256": "<64 hex digits>",
  "result": "created"
}
```

`adapter_sha256` is the SHA-256 of the written adapter. It equals
`git show v<version>:adapters/claude/claude-adapter | sha256sum` (or
`adapters/codex/codex-adapter`) at the release tag of the binary, whose archive
the release attests. The adapter needs Python 3.11 or newer as `python3` on the
`PATH` of the pohunek daemon. After an upgrade of autoreview, run the command
again with the same flags on each review host; it prints `updated` when the new
binary carries another adapter.

### `autoreview validate`

Loads the configuration and the policy, reads every prompt and prints the
policy digest and the agent profiles the policy needs. It makes no GitHub or
pohunek call. Prints:

```json
{
  "policy_digest": "...",
  "agent_profiles": ["autoreview-codex"]
}
```

Token checks in `validate` are planned in #15, and the pohunek version check in #9.

## Service

### `autoreview serve`

Runs discovery and reviews until stopped. Before it starts, it asks GitHub for
the login of the token and refuses with `configuration_invalid` naming
`github.credentials.login` when the token belongs to another account, see
[credentials](configuration.md#credentials). A second `serve` on the same database is refused. Reviews
running at shutdown resume from their stored steps on the next start.

### `autoreview mcp`

Serves the review tools over MCP Streamable HTTP on `mcp.listen` until stopped,
see the [MCP server guide](knowledge/guides/mcp-server.md). It runs beside
`serve` on the same database and needs the `mcp` section of the configuration;
without it the command fails with `configuration_missing` naming `mcp`.

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
| `autoreview stats [--since DATE [--until DATE]] [--json]` | Review statistics from the store: the current state and, with `--since`, daily counts over UTC days; see [review statistics](stats.md) |
| `autoreview review-queue` | Sends the review queue to `notify.review_queue.backends` and prints it. Run it from a timer |

## Act

### `autoreview approve`

```
autoreview approve --repository OWNER/REPO NUMBER HEAD_SHA
```

Approves a clean review that waits for a human (`on_clean: await`). `HEAD_SHA`
is the full head commit, 40 lowercase hexadecimal characters; any other form is
`head_sha_invalid`. The approval applies only if it is still the pull request
head. The engine publishes on its next tick:

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

Finds the waiting review for a head prefix and prints its full `head_sha`.
`HEAD_PREFIX` is 12 to 40 lowercase hexadecimal characters of the head commit,
such as the 12 that notifications show in `Review ID`; any other form is
`head_prefix_invalid`. Read-only. Chat integrations use it before `approve`.

### `autoreview retry`

```
autoreview retry --repository OWNER/REPO NUMBER HEAD_PREFIX
```

Starts a new attempt of a review that stopped after its retries
(`review_stopped`). `HEAD_PREFIX` takes the same form as for `resolve`: 12 to 40
lowercase hexadecimal characters of the head commit, otherwise
`head_prefix_invalid`. The head must still be the pull request head.

## Maintenance

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
| `init_file_exists` | `init` found a file in the way; the `configuration` field names it |
| `init_write_failed` | `init` could not check, create or write a path; the `configuration` field names it |
| `profile_host_not_local`, `runtime_package_missing` | `profiles install` was run on the wrong host, or the official runtime package is missing |
| `profile_foreign`, `profile_write_failed` | `profiles install` found a file it did not write, or could not write the path it names |
| `pohunek_command_failed` | `profiles install` could not read the answer of a pohunek command |
| `repository_not_allowed`, `review_identity_invalid` | Malformed repository or pull request number, or an approval whose identity differs from the stored review |
| `head_prefix_invalid` | `HEAD_PREFIX` of `retry` or `resolve` (`head_prefix` of the MCP tools) is not 12 to 40 lowercase hexadecimal characters |
| `head_sha_invalid` | `HEAD_SHA` of `approve` (`head_sha` of the MCP tool) is not the full head commit, 40 lowercase hexadecimal characters |
| `review_not_awaiting_approval`, `approval_not_required` | The review does not wait for an approval |
| `review_superseded`, `approval_expired` | The wait ended without a publication |
| `head_changed` | The pull request has a newer head than the one given |
| `review_already_running`, `review_retry_not_dispatched` | A retry is not needed or not possible right now |
| `engine_already_running` | Another `serve` uses the same database |
| `discovery_busy` | Another discovery pass held the lock past `schedule.discovery_lock_timeout_seconds` |
| `pull_request_not_open`, `pull_request_draft`, `review_label_removed`, `review_request_not_active` | The pull request no longer asks for a review: closed, back in draft, label removed or review request withdrawn |
| `pohunek_base_fetch_failed`, `pohunek_base_fetch_timeout`, `pohunek_base_fetch_dns_failed`, `pohunek_base_fetch_authentication_failed` | The review host could not fetch the base branch from GitHub |
| `state_entry_limit_exceeded` | The discovery state hit `schedule.state_entry_limit` |
| `store_unavailable` | The SQLite store could not be opened or written |
| `backup_path_invalid`, `backup_path_exists`, `backup_failed` | `backup-db` target problems |
| `stats_range_invalid` | `--since` or `--until` of `stats` (`since`, `until` of `review_stats`) is not a `YYYY-MM-DD` day or is after today (UTC), `--until` comes without `--since`, or before it |
| `stats_range_too_long` | The `stats` range covers more than 366 days |
| `project_route_not_found`, `project_route_ambiguous` | The repository is registered on no review host that answered, or as two projects on the host that serves it |
| `pohunek_request_failed`, `pohunek_request_timeout`, `pohunek_session_start_failed` | The review host did not answer or could not start a session |
| `pohunek_session_lost` | The review host lost the session's worker while the session started; retried on a fresh branch. A leftover session that cannot be removed raises `cleanup_manual`, see [stale sessions](knowledge/runbooks/stale-sessions-and-branches.md#when-a-session-cannot-be-removed) |
| `pohunek_base_fetch_ref_locked`, `pohunek_session_warning` | The session start hit a locked base fetch or reported a warning; retried on a fresh branch |
| `pohunek_session_cleanup_failed` | A session could not be removed, see `cleanup_manual` |
| `review_output_timeout`, `review_output_invalid`, `review_result_invalid` | A reviewer did not return a usable result in time |
| `review_head_unavailable` | A reviewer on a base checkout stopped because the pull request head commit is not in the review host's repository; fetch it there, then `retry` |
| `review_adapter_outdated` | A Codex report has no `prior_findings` or a finding without `blocking`, or a Claude result names no checked checkout or another one; the profile runs an older adapter than `adapters/codex/` or `adapters/claude/`; run `profiles install` for it |
| `github_search_limit_exceeded` | A search returned more than `schedule.poll_limit`; narrow the rules or raise the limit |
| `github_request_failed`, `github_request_rejected` | GitHub did not answer or refused the request |
| `matrix_unavailable`, `matrix_request_rejected` | The Matrix homeserver did not accept a message |
| `mcp_listen_failed` | `autoreview mcp` could not listen on `mcp.listen` |
| `review_not_found` | The MCP tool `review_detail` got a workflow ID that the store does not know |
| `mcp_scope_denied`, `mcp_tool_unknown` | An MCP client without the `write` scope called a write tool, or called a tool the server does not have |
| `mcp_client_missing`, `mcp_result_invalid` | An MCP tool call reached the tools without an authenticated client, or its result could not be encoded; both are server defects to report |
| `workflow_failed` | A failure without a more specific code: a review whose stored state misses a part it needs, an error that carries no code, or a command whose standard output is closed; a review that fails with it is not retried |

The [troubleshooting runbook](knowledge/runbooks/troubleshooting.md) maps codes
to fixes.
