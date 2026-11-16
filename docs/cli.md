---
type: Reference
id: reference/cli
title: CLI reference
description: Every autoreview command - init, serve, mcp, validate, poll, candidates, reviews, review, awaiting, stats, report, findings list, approve, resolve, retry, review-queue, backup-db, prune-branches and guard scan - with arguments and JSON output.
source_kind: manual
intents: [setup, debug, help]
---

# CLI reference

> **Status: after 0.4.0.** This page describes autoreview on `main` after the
> 0.4.0 release. A part that is not available names its open issue.

```
autoreview [--config <path>] [--env-file <path>] <command> [arguments]
```

Secrets come only from the variables named by the `*_env` keys of the configuration.
With `--env-file <path>`, a variable that the file names is read from the file, and every
other one from the environment. The file holds `NAME=value` lines; see
[secrets](knowledge/safety/secrets.md#the---env-file) for its format and checks. A file
that fails a check is `configuration_invalid`; the message names the path, and the mode,
the owner or the line number.

Every command except `init`, `profiles install` and `guard scan` reads the
configuration from `--config` or `AUTOREVIEW_CONFIG` and validates it together
with the policy before doing anything. Output is
one JSON object (or array) on stdout, except the text summaries of `stats` and
`report` without `--json` and the JSON lines of `findings list`; logs are JSON lines on stderr. A failed
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
It asks nothing and reads no configuration; `--config` and `--env-file` are refused.
These flags are required:

| Flag | Meaning |
|---|---|
| `--dir` | Clean absolute directory whose parent exists; it and `prompts/` are created when absent. A created `--dir` gets mode 0700 when `store.path` (`--store-path`, or the reference value) lies inside it, as in the `scripts/setup` layout, and the umask's mode otherwise, so the service user can read a `/etc/autoreview` that root creates; `prompts/` always gets the umask's mode, and an existing `--dir` keeps its mode |
| `--login` | Lowercase GitHub user login of the one credential, such as `review-bot`, or `mona-cat_octo` for an Enterprise Managed User |
| `--pohunek-host` | The pohunek host that runs the reviewers |
| `--example` | The example directory under `examples/`, for example `single-repo-comment` |

The layout flags are optional. Each one replaces one value of
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
first start. [`scripts/setup`](knowledge/guides/getting-started.md) passes every
layout flag.

`init` loads the example's policy and checks every path before it writes
anything, and never overwrites. `--dir` and `prompts/` must be real directories
or absent: a symlink, a file or any other entry at either path is in the way, as
is any existing file of the set. `init` creates `--dir` and then `prompts/`, one
level at a time; it never creates the parent of `--dir`. When a later step
fails, it removes the files and directories it created in that run, and nothing
it did not create.

| Failure | Code | `configuration` field |
|---|---|---|
| `--config` or `--env-file` given, a flag missing, an unknown example | `command_invalid` | none |
| `--dir` relative or unclean, or its parent missing or not a directory; `--login`, `--pohunek-host` or a layout flag malformed | `configuration_invalid` | the flag |
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
  [--maximum-result-bytes <BYTES> --maximum-retention-seconds <SECONDS>] \
  --pohunek-timeout-seconds <SECONDS> [--replace]
```

Writes the pohunek agent profile `<NAME>` of a reviewer, and the reviewer
adapter it starts, on the machine it runs on. Run it on each review host, as
the user of its pohunek daemon, once per profile that `validate` lists. It
reads no configuration; `--config` and `--env-file` are refused. Every value is a flag without a
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
  `limits.max_discussion_characters`), and on a pipeline with
  `intent_check: issues` every reviewer and verifier request carries the closed
  issues (bounded by `limits.max_issue_characters`), so the value must cover
  them. Both limits count characters; JSON escaping can make one character up to
  7 bytes of a `claude-structured` request (`<` becomes `\\u003c`).
- `--maximum-result-bytes` (Codex only, required there): the largest report the
  adapter prints. Keep it at most `limits.result_max_bytes`, which the engine
  enforces.
- `--maximum-retention-seconds` (Codex only, required there): how long the
  adapter stays alive after its report, so that the engine can read it. Use at
  least `session.timeout_seconds`.
- `--pohunek-timeout-seconds`: how long each pohunek call may take before it is
  killed, a positive number of seconds. Use the `pohunek.timeout_seconds` of
  the configuration.
- `--replace`: overwrite a file of that name that this command did not write.

A Codex flag with `--agent claude`, or a missing one with `--agent codex`, is
`command_invalid`.

It calls the `pohunek` found on `PATH` for the host check, once when `--host`
is `local` and twice otherwise, and once for `pohunek plugin list --json`. Each
call runs as the engine's pohunek calls run: with `PAGER=cat`, with only the
variables a pohunek child inherits (see
[secrets](knowledge/safety/secrets.md#what-child-processes-receive)), and killed
after `--pohunek-timeout-seconds`. It keeps
the caller's `XDG_STATE_HOME` and `XDG_RUNTIME_DIR`, so it reaches the daemon of
the user who runs it. The
base runtime must be served by exactly one official package that is enabled,
selected and healthy; the profile pins its package id and digest.
`<config>` is `$XDG_CONFIG_HOME`, or `$HOME/.config` when that is unset, and
`<data>` is `$XDG_DATA_HOME`, or `$HOME/.local/share` when that is unset. A
set but empty `XDG_CONFIG_HOME` or `XDG_DATA_HOME` is `configuration_invalid`
before any pohunek call, because pohunek refuses it too. So is a path with an
empty, `.` or `..` part or a trailing `/` (`/srv//data`, `/srv/./data`,
`/srv/data/`) in the variable, or in `HOME` when the variable is unset.
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
| `--config` or `--env-file` given, a flag missing, an unknown agent or effort, a Codex flag that does not fit `--agent` | `command_invalid` | none |
| `--host`, `--name`, `--model`, `--effort` or `--agent-binary` refused; `XDG_CONFIG_HOME` or `XDG_DATA_HOME` set but empty; `XDG_CONFIG_HOME`, `XDG_DATA_HOME` or `HOME` relative, with an empty, `.` or `..` part or a trailing `/`, or neither set | `configuration_invalid` | the flag or the variable |
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

```
autoreview validate [--check-credentials]
```

Loads the configuration and the policy, reads every prompt and prints the
policy digest, the agent profiles the policy needs and the version of the
`pohunek` CLI at `pohunek.bin`. It runs `pohunek --version`, which reaches no
daemon; without `--check-credentials` it makes no GitHub call and no other
pohunek call. Prints:

```json
{
  "policy_digest": "...",
  "agent_profiles": ["autoreview-codex"],
  "pohunek_version": "0.42.0"
}
```

The supported pohunek releases are 0.41.0 or later, before 1.0.0. A release
outside that range fails with `pohunek_version_unsupported`; a run that fails
or an output other than `pohunek X.Y.Z` fails with `pohunek_version_invalid`.
Both name `pohunek.bin`.

Before that, it checks that the store is private to the service user: the
directory of `store.path` must grant the group and other users nothing (mode
0700 or narrower), and so must the database and its `-wal` and `-shm` files
(0600 or narrower). A path that does not exist yet passes: the store creates a
missing directory with mode 0700 and a missing database with mode 0600, whatever
the umask, and SQLite gives `-wal` and `-shm` the mode of the database. A wider
mode fails with
`configuration_invalid` naming `store.path`; the message names each path with
the `chmod 700` or `chmod 600` that fixes it. `validate` never changes a mode
itself: run the commands as the owner of the store.

`--check-credentials` also runs the [startup check](#autoreview-serve) of every
credential and adds what GitHub answered, in file order:

```json
"credentials": [
  {"name": "default", "login": "review-bot", "expires_at": "2026-12-31T23:59:59Z"}
]
```

`expires_at` is the token's expiry in UTC; it is `null` when GitHub sends no
expiry, as for a token that never expires. Token values are never printed.

### `autoreview guard scan`

```
autoreview guard scan --lexicon <PATH> --max-input-bytes <BYTES> --threshold <SCORE>
  --fields <LIST> <CORPUS>
```

Runs the [text guard](policy.md#guard) over a corpus of pull request texts, to
measure a lexicon, fields and a threshold before a rule uses them; see
[enable the text guard](knowledge/guides/text-guard.md). It reads no
configuration, makes no network call and writes nothing. Every option is
required and means what its key means: `--lexicon` as
`guards.text.lexicon_file` (any path), `--max-input-bytes` as
`guards.text.max_input_bytes` (1 to 4194304), `--threshold` as
`guard.threshold` (at least 1) and `--fields` as `guard.fields`, comma
separated, each once. A value out of range is `command_invalid`; a lexicon file
that cannot be read or does not validate is `guard_lexicon_invalid`.

`CORPUS` is a JSONL file: one object per pull request, each on one line, every
key required (wrapped here):

```json
{"repository": "acme/widget", "number": 7, "truncated": false,
 "texts": [{"kind": "body", "text": "Fix the parser."}]}
```

`kind` is a field code (`title`, `body`, `commit_message`, `comment`,
`review_thread`, `patch`, where a `patch` text is the unified diff of one
file), and `truncated` says that the fetch left text out.
[`scripts/guard-corpus`](knowledge/guides/text-guard.md#fetch-a-false-positive-corpus)
writes such a file. A file that cannot be read, has no line or a line that is
not such an object (an empty line included) is `guard_corpus_invalid`; the
log line names the line number and the kind of error, never its text.

Each pull request is scanned as a review's guard step scans it: only the texts
of `--fields`, bounded by `--max-input-bytes` in field order, with the same
scan. It is suspect when its score reaches `--threshold` or its text was left
out (`truncated` in the line, or cut at the bound). Prints signal codes only:

```json
{
  "pull_requests": [
    {"repository": "acme/widget", "number": 7, "score": 100,
     "signals": [{"field": "body", "signal": "unicode_tags", "count": 1}],
     "truncated": false, "suspect": true}
  ],
  "summary": {"pull_requests": 1, "flagged": 1, "flagged_percent": 100.0,
              "hidden_text": 1, "truncated": 0, "signals": {"unicode_tags": 1}}
}
```

`flagged` counts the suspect pull requests and `flagged_percent` is their share,
rounded down to two decimals. `hidden_text` counts the pull requests with at
least one signal of the hidden-text class (every signal except
`bidi_control_in_patch`, `chat_template_token` and `lexicon_phrase`),
`truncated` those whose text was left out, and `signals` the pull requests per
signal code found.

## Service

### `autoreview serve`

Runs discovery and reviews until stopped. Before it starts, it checks the
pohunek version as [`validate`](#autoreview-validate) does and logs it as
`pohunek version checked` with `pohunek_version`. Then it checks every
credential in file order: it asks GitHub for the login behind the token
(`GET /user`) with that credential's token only. A token that GitHub refuses
(revoked, expired or never valid) fails with `credential_invalid`, and a token
of another account with `credential_login_mismatch` naming
`github.credentials.login`. An expiry that is not a time fails with
`github_response_invalid`. Each message names the credential, see
[credentials](configuration.md#credentials). Each checked credential is logged
as `credential checked` with its name, and with `expires_at` when GitHub sent one.
While it runs, it checks every token again when the engine starts and every
`github.credential_check_hours`, logs each accepted check as `credential checked`
with the same fields, and raises `credential_expiring` for a token that expires
within `github.expiry_warning_days`. A token that GitHub refuses
while it runs suspends only its credential and raises `credential_invalid`, see
[suspension](knowledge/concepts/github-identity.md#suspension).

A second `serve` on the same database is refused. Reviews running at shutdown
resume from their stored steps on the next start. A SIGINT or SIGTERM during
the pohunek version check, the credential check or while the engine runs is a
normal stop: `serve` logs
`pull request review engine stopping` and exits with status 0.

With a [`metrics` section](configuration.md#metrics), `serve` also answers
`GET /metrics` on `metrics.listen`, see
[Prometheus metrics](stats.md#prometheus-metrics). It binds that address after
the credential check and before it takes the engine lock, so a second `serve` with
the same section fails with `metrics_listen_failed` before it reaches
`engine_already_running`; one without the section, or with another port, fails
with `engine_already_running`.

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
per rule and trigger, after it has classified each author as a pass does. It
runs the same candidate search and author permission reads, and it opens the
store as `reviews` does (an empty one is created when none exists), but writes
no state.

- `candidates`: each pull request with its `author_class` (`trusted` or
  `untrusted`) and the `rule` it would run under; for an untrusted author that is
  the rule of the `untrusted` section.
- `rejected`: pull requests whose data or author permission read was refused,
  with the `code`.
- `skipped`: pull requests whose rule skips their untrusted author
  (`untrusted: skip`), with `author`, `fork`, `author_class` and the `reason`
  `untrusted_author`.

## Inspect

| Command | Prints |
|---|---|
| `autoreview reviews [LIMIT]` | The newest reviews, at most `cli.review_list_limit`, each with its `author_class` |
| `autoreview review --repository OWNER/REPO NUMBER` | Everything the engine knows about one pull request: current head, reviews with their `author_class`, published decisions, waits, hand-offs, and `skipped` with the reason when the last discovery pass skipped it |
| `autoreview awaiting` | Reviews that wait for a human approval |
| `autoreview stats [--since DATE [--until DATE]] [--json]` | Review statistics from the store: the current state and, with `--since`, daily counts over UTC days; see [review statistics](stats.md) |
| `autoreview report --since DATE [--until DATE] [--repo OWNER/REPO] [--pipeline NAME] [--json]` | Review timing, decision shares and finding outcomes over UTC days; see [below](#autoreview-report) |
| `autoreview findings list --repo OWNER/REPO --since DATE` | The recorded outcome of each published finding of one repository, one JSON object per line; see [below](#autoreview-findings-list) |
| `autoreview review-queue` | Sends the review queue to `notify.review_queue.backends` and prints it. Run it from a timer |

A review's `author_class` is the class it was dispatched with, `trusted` or
`untrusted`; a review stored before author classes has no `author_class`.

### `autoreview findings list`

```
autoreview findings list --repo <owner/name> --since <YYYY-MM-DD>
```

Prints the [finding outcomes](knowledge/concepts/finding-outcomes.md) of one
repository: one JSON object per line for every finding published at or after
midnight UTC of `--since`, oldest first (then by pull request number and finding
key). Both flags are required. It calls neither GitHub nor pohunek and has no row
limit; a repository or a range without findings prints nothing and exits with
status 0.

| Field | Value |
|---|---|
| `repository`, `number` | The pull request; the repository as lowercase `owner/name` |
| `finding_key` | The head SHA cut to 12 characters, `:` and the finding ID |
| `tracked_key` | The key a later review settles the finding by; empty once it is no longer tracked |
| `workflow_id`, `finding_id`, `head_sha` | The review that published the finding first, its ID there and the head it was published on |
| `reviewer_id`, `pipeline`, `rule` | The reviewer (the first one of a merged finding), the pipeline and the rule `match` |
| `severity`, `confidence`, `path`, `line` | As published |
| `blocking` | `true` when the finding held the merge back when it was published |
| `outcome` | `open`, `refuted`, `not_blocking`, `fixed`, `answered`, `open_at_merge` or `open_at_close` |
| `outcome_head_sha` | The head the outcome was decided at; empty while `open` |
| `published_at`, `updated_at` | When the finding was published and when its row last changed, RFC 3339 in UTC |
| `ended` | `merged` or `closed` once a state read saw the pull request end, empty before; only a pull request with an `open` row is read, so `ended` stays empty for one whose rows were all decided before it ended; a reopened pull request keeps its earlier end until it ends again with an `open` row, which only a review after the reopen adds |
| `end_head_sha` | The head the pull request ended on; empty while `ended` is empty |

The fields always come in this order. A row holds no title or body of a finding.
A `--repo` that is not a lowercase `owner/name` is `repository_not_allowed`, a
`--since` that is not an existing `YYYY-MM-DD` day or a missing flag is
`command_invalid`, and a store that cannot be opened is `store_unavailable`, or
`store_version_unsupported` when a newer release wrote it. Like every command
that opens the store, it creates an empty one at `store.path` when none exists.
A reader that stops early, such as `head`, ends the output: the command prints
no further row, logs nothing and exits with status 0.

### `autoreview report`

```
autoreview report --since <YYYY-MM-DD> [--until <YYYY-MM-DD>] [--repo <owner/name>]
  [--pipeline <name>] [--json]
```

Prints the [review report](knowledge/concepts/review-report.md) of the UTC days
from `--since` to `--until` (today when absent), both included: the time from
the creation of an attempt and from the review request to its first publication, the
shares of decisions, the reviewer disagreement rate, the verifier's demotion rate per
reviewer, and the outcomes of the findings published
in the range per reviewer, severity and repository. It calls neither GitHub nor
pohunek. Without `--json` it prints a short text table; scripts read the JSON:

```
Report 2026-11-14 to 2026-11-16 (UTC days), repository every, pipeline every
Reviews: 6 decided, 5 published, 1 approval expired
  approved without a human 33.3%, waited for a human 16.7%
  changes requested 33.3%, commented 16.7%
  attempt creation to publication: median 1200 s, p90 3000 s
  review request to publication: median 1500 s, p90 3300 s
  reviewers disagreed: 1 of 4 attempts with two or more reviewers, 25.0%
  verifier on reviewer lint-bot: 3 checked, 2 demoted, 66.7%
  verifier on reviewer review-bot: 3 checked, 0 demoted, 0.0%
  reviewer review-bot blocked alone: 1
Findings                findings blocking open fixed open_at_merge fixed% demoted
total                          9        4    1     2             1  66.7%       2
reviewer lint-bot              5        1    0     0             0   none       2
reviewer review-bot            4        3    1     2             1  66.7%       0
severity major                 5        3    1     1             1  50.0%       2
severity minor                 4        1    0     1             0 100.0%       0
repository acme/gadgets        3        1    0     0             0   none       0
repository acme/widgets        6        3    1     2             1  66.7%       2
```

| JSON field | Value |
|---|---|
| `schema_version` | `2`; a changed field gets a new version |
| `generated_at`, `since`, `until` | When the report was made, and the range |
| `repository`, `pipeline` | The filters, `null` when absent |
| `reviews` | `published`, `approval_expired`, `decided` (their sum), `approved_without_human`, `approved_after_wait`, `changes_requested`, `commented`; the shares `approved_without_human_percent`, `waited_for_human_percent`, `changes_requested_percent`, `commented_percent` of `decided`; `attempt_to_publication_median_seconds`, `attempt_to_publication_p90_seconds`: from the creation of a published attempt to its completion; `request_to_publication_median_seconds`, `request_to_publication_p90_seconds`: from the review request to the completion of its first published attempt, one per request with a known request time (none for a label trigger); a later attempt under the same request adds nothing; `multi_reviewer`: the published attempts with the results of two or more reviewers, `disagreed`: those where at least one reviewer made a blocking claim and at least one made none, and `disagreed_percent` of `multi_reviewer`; `by_reviewer` (each entry with its `key`, a reviewer ID): `verifier_checked`, the blocking findings of that reviewer that a finished verification of an attempt completed in the range gave a verdict, `verifier_demoted`, those found `not_blocking` or `refuted`, `verifier_demoted_percent` of `verifier_checked`, and `blocked_alone`, the disagreeing attempts in which that reviewer made a blocking claim |
| `findings` | `total`, then `by_reviewer`, `by_severity` and `by_repository` (each entry with its `key`): `findings`, `blocking`, one count per outcome, and `fixed_percent` |

A share is a percentage with one decimal, `null` when its whole is `0`. A
`--repo` that is not a lowercase `owner/name` is `repository_not_allowed`, a
`--pipeline` that no rule of the policy runs is `pipeline_not_configured`, the
days are refused with the codes that `report` shares with `stats`
(`stats_range_invalid`, `stats_range_too_long`; their messages name `stats`),
and a range that starts before the store's retention of the finding outcomes,
or with `--pipeline` holds a day pruned without the counts per pipeline, is
`report_range_pruned`. With `--pipeline` the review part covers pruned reviews,
as with `--repo`.

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
safe. After a newer commit, a changed review request or `approval.timeout_hours`
the wait is cleared and the command answers `review_not_awaiting_approval`;
`review_superseded` or `approval_expired` come only while the ended wait is not
cleared yet. In every case nothing is posted.

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

### `autoreview prune-branches`

```
autoreview prune-branches --git-binary <PATH> [--host <HOST>] [--older-than-days <DAYS>] [--yes]
```

Lists the review branches that no review needs and, with `--yes`, deletes them; without
`--yes` nothing changes.

Only local review hosts are pruned. pohunek has no command that lists or deletes the
branches of another machine, so run the command on the machine that holds both the store
and the review host's repositories, as the user of the service. A host whose repositories
are on another machine is reported as skipped with `host_not_local`, and its branches stay.

- `--git-binary`: the absolute path of `git`.
- `--host`: a host of `pohunek.hosts` or of a `route_overrides` entry (also an `untrusted`
  one); every such host when absent. A host counts as local only when
  `pohunek host governance inspect` reports the same `host_id` for it as for `local`.
- `--older-than-days`: keeps the branches whose review was last updated fewer than this many
  days ago. A branch whose review the store no longer holds counts as older than
  `store.review_retention_seconds`.

The command lists the branches first, then reads the store and the sessions. The
repositories are the roots of `pohunek project list` on the host; the branches are the ones
whose name starts with `session.branch_prefix`. Two services that share a review host need
different prefixes: a branch of the other service's reviews has no owner in this store.
Each listed branch gets a `verdict`:

| Verdict | Meaning |
|---|---|
| `delete` | No review, session or worktree needs it; `--yes` deletes it |
| `symbolic_ref` | The ref is symbolic (`git symbolic-ref`); git lists it with its target's commit, so it is never deleted; kept |
| `name_unknown` | No workflow ID follows the prefix; kept |
| `review_active` | Its review is queued, running or awaiting, or its session cleanup is pending; kept |
| `session_live` | A pohunek session of the host works on it; kept |
| `checked_out` | A worktree of the repository has it checked out; kept |
| `too_recent` | Its review was updated fewer than `--older-than-days` days ago; kept |
| `age_unknown` | `--older-than-days` is set and the age is unknown; kept |

```json
{"delete": true, "hosts": [
  {"host": "review-a", "repositories": [{"path": "/srv/repos/example", "branches": [
    {"branch": "autoreview/review/prr-v2-<digest>-opus-run1", "commit": "<sha>",
     "verdict": "delete", "deleted": true}]}]},
  {"host": "review-b", "skipped": "host_not_local", "repositories": []}]}
```

Before each deletion the log line `branch delete` names the host, the repository, the branch
and the commit. A deletion runs `git update-ref --no-deref -d` with the listed commit, so a
branch that moved since the listing stays and gets `"error": "git_command_failed"`. Restore a
deleted branch with `git -C <path> branch <branch> <commit>` while the commit still exists.
git runs without the system and the user configuration (`GIT_CONFIG_NOSYSTEM`,
`GIT_CONFIG_GLOBAL=/dev/null`, no `HOME`), without the hooks directory
(`core.hooksPath=/dev/null`) and file system monitor, and only in the repository at the root
itself, never in one above it. From git 2.55, `hook.reference-transaction.enabled=false` also
stops every `reference-transaction` hook that the repository configuration names; git 2.54
runs such a hook anyway, so with any git a repository whose configuration names one is not
pruned and is reported with `"error": "repository_hook_configured"`.

The repositories must be owned by the user who runs the command: git reads `safe.directory`
only from the system, the user and the command-line configuration, none of which sets it
here, so a repository of another user fails with `git_command_failed`. A host whose pohunek
calls fail is reported with the error code in `skipped`, and a repository whose git calls
fail with it in `error`; the other hosts go on. A missing store database is
`store_unavailable`.

The document is printed in every case once the hosts were read. The command then exits with
status 1 and logs `command failed` with `branch_prune_incomplete`, without a second object on
stdout, when a deletion failed, or when the host that `--host` names was skipped or has a
repository in `error`. Without `--host`, a skipped host or repository keeps exit status 0.

## Error codes

| Code | Meaning |
|---|---|
| `configuration_missing`, `configuration_invalid` | A key is missing or invalid; the message names it |
| `credential_not_covered`, `credential_unused` | A policy rule has a repository that no credential matches, a credential serves no repository of the policy, or a review's credential no longer fits it, see [credentials](configuration.md#credentials) |
| `credential_invalid`, `credential_login_mismatch` | GitHub refuses a credential's token, or the token belongs to another account than its `login`; the message names the credential, see [`serve`](#autoreview-serve). A review step or a discovery search that GitHub answers with HTTP 401 also fails with `credential_invalid`; such a review is not retried automatically, see [suspension](knowledge/concepts/github-identity.md#suspension). At startup, `github_response_invalid` names the credential whose token expiry is not a time |
| `command_invalid` | Wrong arguments |
| `init_file_exists` | `init` found a file in the way; the `configuration` field names it |
| `init_write_failed` | `init` could not check, create or write a path; the `configuration` field names it |
| `profile_host_not_local`, `runtime_package_missing` | `profiles install` was run on the wrong host, or the official runtime package is missing |
| `profile_foreign`, `profile_write_failed` | `profiles install` found a file it did not write, or could not write the path it names |
| `guard_lexicon_invalid`, `guard_corpus_invalid` | `guard scan` could not read its lexicon file or it does not validate, or its corpus file cannot be read, has no line or has a malformed line; the log line names the cause and the line number |
| `pohunek_command_failed` | `profiles install` could not read the answer of a pohunek command about the local daemon, or that command ran past `--pohunek-timeout-seconds`; a failed or timed-out check of a named `--host` is `profile_host_not_local` |
| `repository_not_allowed`, `review_identity_invalid` | Malformed repository or pull request number, or an approval whose identity differs from the stored review |
| `head_prefix_invalid` | `HEAD_PREFIX` of `retry` or `resolve` (`head_prefix` of the MCP tools) is not 12 to 40 lowercase hexadecimal characters |
| `head_sha_invalid` | `HEAD_SHA` of `approve` (`head_sha` of the MCP tool) is not the full head commit, 40 lowercase hexadecimal characters |
| `review_not_awaiting_approval`, `approval_not_required` | The review does not wait for an approval, or its wait ended (a newer commit, a changed review request, the deadline) and was cleared |
| `review_superseded`, `approval_expired` | The wait ended without a publication and is not cleared yet |
| `head_changed` | `retry` got a head that is no longer the pull request head, or a running review saw a new head (it ends as `superseded`) |
| `review_already_running`, `review_retry_not_dispatched` | A retry is not needed or not possible right now |
| `review_already_completed` | `retry` found a completed review of this head (it succeeded or went on to wait for an approval) against the current base branch and no later failed review of it, or one whose base branch is no longer known; nothing to do when that review is current, approve it if it still waits, or push a new commit for a fresh review |
| `engine_already_running` | Another `serve` uses the same database |
| `workflow_state_unknown` | A review is recorded as running, but the store holds no run of it; the scheduler does not start it again |
| `discovery_busy` | Another discovery pass held the lock past `schedule.discovery_lock_timeout_seconds` |
| `pull_request_not_open`, `pull_request_draft`, `review_label_removed`, `review_request_not_active` | The pull request no longer asks for a review: closed, back in draft, label removed or review request withdrawn |
| `pohunek_base_fetch_failed`, `pohunek_base_fetch_timeout`, `pohunek_base_fetch_dns_failed`, `pohunek_base_fetch_authentication_failed`, `pohunek_base_fetch_permission_denied`, `pohunek_base_fetch_ref_missing`, `pohunek_base_fetch_ref_locked`, `pohunek_base_fetch_disk_full` | The review host could not fetch the base branch from GitHub |
| `state_entry_limit_exceeded` | The discovery state hit `schedule.state_entry_limit` |
| `store_path_invalid` | `store.path` is not absolute or has no directory |
| `store_unavailable` | The SQLite store could not be opened or written |
| `store_version_unsupported` | A newer release wrote the store; this release does not open it |
| `backup_path_invalid`, `backup_path_exists`, `backup_failed` | `backup-db` target problems |
| `host_not_local`, `git_command_failed`, `repository_hook_configured` | `prune-branches` skipped a host whose repositories are not on this machine, a git call failed (also for a repository owned by another user), or a repository configuration names a `reference-transaction` hook |
| `branch_prune_incomplete` | `prune-branches` printed its document and exits with status 1: a deletion failed, or the `--host` host or one of its repositories was skipped |
| `stats_range_invalid` | `--since` or `--until` of `stats` or `report` (`since`, `until` of `review_stats`) is not a `YYYY-MM-DD` day or is after today (UTC), `--until` comes without `--since`, or before it |
| `stats_range_too_long` | The `stats` or `report` range covers more than 366 days |
| `pipeline_not_configured` | `--pipeline` of `report` names a pipeline that no rule of the policy runs |
| `report_range_pruned` | The `report` range starts before now minus `store.outcome_retention_seconds`, or, with `--pipeline`, holds a day with reviews that a release before the counts per pipeline pruned, so pruned data would be missing |
| `author_limit_reached` | The rule's `author_limit` with `over: skip` did not start the review: its author started the most reviews the rule allows in the window. `retry` starts it |
| `project_route_not_found`, `project_route_ambiguous` | The repository is registered on no review host that answered, or as two projects on the host that serves it |
| `pohunek_request_failed`, `pohunek_request_timeout`, `pohunek_session_start_failed` | The review host did not answer or could not start a session |
| `pohunek_version_invalid`, `pohunek_version_unsupported` | `pohunek --version` of `pohunek.bin` failed or printed no `pohunek X.Y.Z`, or the release is outside the supported range, see [`validate`](#autoreview-validate) |
| `host_state_ineligible` | Only a host that a `pohunek.host_state` rule held back has the repository; the review waits for a review host |
| `pohunek_session_lost` | The review host lost the session's worker while the session started; retried on a fresh branch. A leftover session that cannot be removed raises `cleanup_manual`, see [stale sessions](knowledge/runbooks/stale-sessions-and-branches.md#when-a-session-cannot-be-removed) |
| `pohunek_base_fetch_ref_locked`, `pohunek_session_warning` | The session start hit a locked base fetch or reported a warning; retried on a fresh branch |
| `pohunek_session_cleanup_failed` | A session could not be removed, see `cleanup_manual` |
| `review_output_timeout`, `review_output_incomplete`, `review_output_invalid`, `review_output_too_large`, `review_result_invalid`, `review_nonce_mismatch`, `review_wrapper_invalid` | A reviewer did not return a usable result in time, or its output was too large |
| `claude_result_invalid`, `claude_result_mismatch` | Claude Code's result message was unusable, or its prose disagreed with its structured output; not retried |
| `reviewer_usage_limited` | Claude Code ended a reviewer run on the rate or usage limit of its model account (a result message with `api_error_status` 429); the reviewer is not run again inside the review, the scheduler tries the review again after each delay of `schedule.usage_limit_retry_delays_seconds` without spending `schedule.retry_delays_seconds`, then stops with a `review_stopped` notice |
| `review_head_unavailable` | A reviewer on a base checkout stopped because the pull request head commit is not in the review host's repository; with pohunek 0.43.0 or later (CLI and daemon) the session start fetches it, so this code means the capability read failed (log `head fetch capability unknown, starting without the fetch`): `retry`; with an older pohunek fetch it there, then `retry` |
| `review_head_skills_refused` | The Codex adapter refused a `checkout: head` review whose repository skills (`.agents/skills/`, `.codex/skills/`) differ from the trusted base; not retried. Review the pull request on a `checkout: base` pipeline, or have a maintainer review the skill change |
| `review_host_repo_hooks_unsupported` | A `checkout: head` review was not started: the pohunek CLI or the review host's daemon is older than 0.43.1 and cannot start the session with `--no-repo-hooks`; not retried. Upgrade both, then `retry` |
| `review_host_hooks_refused` | A `checkout: head` review was not started: the review host reports a pohunek hook script of its own for `post_create`, `session_start`, `session_stop`, `agent_state` or `pre_remove`, or reports no `host_hooks`, and `pohunek.head_checkout_host_hooks` is `refuse`; not retried. See [configuration](configuration.md#pohunek) |
| `review_host_hook_required` | pohunek refused a `checkout: head` session because its `[hooks] required` lists an event that has no host script, which only the skipped repository hooks could run; not retried |
| `pohunek_head_fetch_failed`, `pohunek_head_fetch_ref_missing` | The session start could not fetch the pull request head (pohunek 0.43.0 or later); a failed fetch is retried, a missing `origin` or `refs/pull/<n>/head` is not |
| `pohunek_head_fetch_mismatch` | The session start fetched `refs/pull/<n>/head` at another commit than the trusted head SHA; the head starts again after the debounce, once for each entry in `schedule.retry_delays_seconds`, then stops with a `review_stopped` notice that names the expected and the fetched commit |
| `review_adapter_outdated` | A Codex report has no `prior_findings` or a finding without `blocking`, or a Claude result names no checked checkout or another one, or is in mode `text` (an adapter of 0.3.0 or earlier answers every request so); the profile runs an older adapter than `adapters/codex/` or `adapters/claude/`; run `profiles install` for it |
| `github_search_limit_exceeded` | A search returned more than `schedule.poll_limit`; narrow the rules or raise the limit |
| `github_request_failed`, `github_request_rejected` | GitHub did not answer or failed the request (`github_request_failed`, retried), or rejected it as invalid with HTTP 422 (`github_request_rejected`, not retried); HTTP 401 is `credential_invalid` |
| `matrix_unavailable`, `matrix_request_rejected` | The Matrix homeserver did not accept a message |
| `webhook_unavailable`, `webhook_request_rejected`, `webhook_request_invalid` | A webhook did not accept a message, or its URL does not parse |
| `mcp_listen_failed` | `autoreview mcp` could not listen on `mcp.listen` |
| `metrics_listen_failed` | `autoreview serve` could not listen on `metrics.listen` |
| `review_not_found` | The MCP tool `review_detail` got a workflow ID that the store does not know |
| `mcp_scope_denied`, `mcp_tool_unknown` | An MCP client without the `write` scope called a write tool, or called a tool the server does not have |
| `mcp_client_missing`, `mcp_result_invalid` | An MCP tool call reached the tools without an authenticated client, or its result could not be encoded; both are server defects to report |
| `base_branch_changed` | The pull request's base branch changed while its review ran; the review ends as `superseded` and the pull request is reviewed again, see [incremental reviews](knowledge/concepts/incremental-reviews.md) |
| `review_history_limit_exceeded` | The store holds more reviews than `schedule.state_entry_limit`; the discovery pass dispatches nothing rather than over a review it cannot see |
| `review_request_event_missing` | GitHub lists no review request event for a pull request that asks for a review; `candidates` shows it as rejected with this code |
| `base_branch_unsupported` | The pull request's base branch name is longer than 255 bytes, or has a character other than ASCII letters, digits, `.`, `_`, `/` and `-`, or `..` (an empty name is `github_response_invalid`); GitHub allows such names, autoreview does not, see [prompt injection](knowledge/safety/prompt-injection.md); `candidates` shows it as rejected with this code, and a review whose pull request moved to such a branch fails with it, and an approval wait for it ends as `superseded`; not retried |
| `pohunek_session_not_found`, `pohunek_response_invalid` | A pohunek call named a session that the review host does not have, or its answer did not read; a route lookup that fails this way waits for a review host, as after `pohunek_request_failed` |
| `session_identity_ambiguous`, `session_identity_invalid` | The review host lists two sessions of the review's name, or the session of that name has another profile, project or branch, reports malformed IDs or runtime generation, or is not running live in a linked worktree; not retried |
| `host_state_invalid`, `host_state_stale`, `host_state_unavailable` | The state read of a host did not read as `HostCapacity` v1, was older than `max_age_seconds`, or could not run; `pohunek.host_state.unknown_state` decides whether the host is used, see [configuration](configuration.md#pohunek) |
| `handoff_review_not_stored`, `handoff_review_mismatch` | A notes hand-off found no stored review of its head, or one of another pull request or head; not retried |
| `github_review_too_large` | The review body or an inline comment is longer than `publish.github_body_max_bytes`; not retried |
| `github_review_state_conflict` | GitHub holds a review of this head with the review marker but another decision; not retried |
| `matrix_message_invalid`, `matrix_request_invalid`, `notify_backend_unknown`, `notify_route_invalid`, `review_queue_too_long` | A message was not sent: it is empty; the Matrix request could not be built (no transaction ID could be drawn, `notify.max_characters` is below 1, the room or transaction is a dot segment, the content did not encode, or the URL does not parse); a route names no configured backend; or the review queue has no room for one entry within `notify.max_characters` |
| `artifact_invalid` | The merged findings of several reviewers exceed `limits.max_findings`, although each result alone is within it: raise `limits.max_findings`, restart `serve` and `retry` the pull request. Also the defect of results that do not match the reviewers; not retried automatically |
| `embedded_asset_invalid`, `github_request_invalid`, `github_review_invalid`, `review_incomplete`, `review_input_invalid`, `review_protocol_invalid`, `review_request_invalid`, `review_trigger_invalid`, `store_state_invalid` | A stored review, a request that the engine built or a schema embedded in the binary is not usable; not retried; a defect or a damaged store to report |
| `workflow_failed` | A failure without a more specific code: a review whose stored state misses a part it needs, an error that carries no code, or a command whose standard output is closed (`findings list` instead stops with status 0); a review that fails with it is not retried |

The [troubleshooting runbook](knowledge/runbooks/troubleshooting.md) maps codes
to fixes.
