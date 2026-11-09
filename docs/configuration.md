---
type: Reference
id: reference/configuration
title: Configuration reference
description: Every key of config.yaml - GitHub account, pohunek hosts, sessions, scheduling, limits, state store, publication and notifier backends - and the environment variables that carry secrets.
source_kind: manual
intents: [setup, update, debug, help]
---

# Configuration reference

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

`config.yaml` holds the technical settings of one autoreview service: which
GitHub account it acts as, which pohunek hosts run the reviewers, how often it
polls, its limits, where it keeps state and where notifications go. What gets
reviewed and how is in [policy.yaml](policy.md).

**Every key is required.** autoreview has no built-in defaults: a missing key
stops the service at startup with an error that names it.
[`examples/config.yaml`](../examples/config.yaml) holds every key with a
recommended value and a comment; its `notify` block needs a `matrix` backend
before it loads. Secrets are never in
the file; keys ending in `_env` name the environment variable that holds them.

The service finds the file through `--config <path>` or the
`AUTOREVIEW_CONFIG` environment variable; with neither it refuses to start.

## Complete file

The values below are those of `examples/config.yaml`, with a `matrix` backend in
`notify`.

```yaml
version: 1
policy_file: /etc/autoreview/policy.yaml

github:
  host: github.com
  gh_bin: /usr/bin/gh
  timeout_seconds: 30
  credential_check_hours: 24
  expiry_warning_days: 14
  # Exactly one credential with match ["*"] is accepted until #13. Its token
  # must belong to its login.
  credentials:
    - name: default
      login: review-bot
      token_env: AUTOREVIEW_GH_TOKEN
      match: ["*"]

pohunek:
  bin: /usr/local/bin/pohunek
  state_home: /var/lib/autoreview/.local/state
  runtime_dir: /run/autoreview
  # Session start includes the host's base fetch, so it needs more than 30 s.
  timeout_seconds: 45
  hosts: [review-host]
  route_overrides: {}

session:
  group: autoreview
  branch_prefix: autoreview/review/
  name_prefix: github-review-
  timeout_seconds: 7200
  output_poll_seconds: 15
  start_max_attempts: 3
  start_retry_seconds: 15
  cleanup_max_attempts: 3
  cleanup_retry_seconds: 15
  reviewer_run_max_attempts: 2
  reviewer_run_retry_seconds: 30
  terminal_cols: 240
  terminal_rows: 200
  # Codex keeps its whole transcript in the output, so the bound follows Codex.
  output_max_bytes: 8388608
  output_chunk_bytes: 65536
  output_wait_ms: 8000

schedule:
  poll_interval_seconds: 60
  worker_tick_seconds: 5
  max_concurrent_reviews: 8
  debounce_seconds: 120
  retry_delays_seconds: [60, 300, 900]
  step_retry_seconds: 900
  step_retry_initial_seconds: 2
  step_retry_max_interval_seconds: 60
  discovery_lock_timeout_seconds: 300
  discovery_lock_poll_ms: 500
  poll_limit: 100
  search_page_limit: 20
  scheduler_max_bytes: 16777216
  state_entry_limit: 10000

limits:
  result_max_bytes: 1048576
  max_findings: 100
  max_tests: 100
  min_summary_characters: 40
  max_summary_characters: 1500
  max_title_characters: 512
  max_body_characters: 2500
  max_discussion_characters: 40000
  max_path_characters: 4096
  max_test_characters: 1024
  excerpt_characters: 360

store:
  path: /var/lib/autoreview/autoreview.db
  review_retention_seconds: 2592000
  # A published decision older than this does not serve as the baseline of
  # an incremental review.
  published_retention_seconds: 7776000

cli:
  review_list_limit: 50

publish:
  # Last line of every review body and inline comment.
  signature: "Written by AI: autoreview (posted from a GitHub account, not by a human)"
  # Comments matching this are treated as machine-written in the discussion
  # of earlier findings.
  ai_signed_pattern: "Written by AI:"
  # GitHub accepts at most 65536 characters per review body or comment.
  github_body_max_bytes: 60000

notify:
  timeout_seconds: 30
  max_characters: 8000
  backends:
    - name: matrix
      type: matrix
      homeserver_url: https://matrix.example.org
      access_token_env: AUTOREVIEW_MATRIX_TOKEN
      room: "!roomid:example.org"
      mention_user: "@you:example.org"
  routes:
    action: [matrix]
    digest: [matrix]
    log: [matrix]
  review_queue:
    backends: [matrix]
    max_items: 50
```

## Top-level keys

| Key | Meaning |
|---|---|
| `version` | `1` |
| `policy_file` | Absolute path of [policy.yaml](policy.md); prompt paths in the policy are relative to its directory |

## github

| Key | Meaning |
|---|---|
| `host` | GitHub host; `github.com` in this release |
| `gh_bin` | Absolute path of the GitHub CLI |
| `timeout_seconds` | Timeout of one `gh` call |
| `credential_check_hours` | Interval of the token check while the service runs, which is planned in #19 |
| `expiry_warning_days` | Days before a token's expiry for the expiry warning, which is planned in #19 |
| `credentials` | Exactly one credential with `match: ["*"]`, see below |

### Credentials

A credential is one GitHub token and the account it belongs to. The loader
accepts exactly one credential, with `match: ["*"]`; any other list is refused
with `configuration_invalid` naming `github.credentials`. Several credentials,
per owner or per repository, are planned in #13.

```yaml
credentials:
  - name: default
    login: review-bot
    token_env: AUTOREVIEW_GH_TOKEN
    match: ["*"]
```

| Key | Meaning |
|---|---|
| `name` | Checked against `[a-z0-9][a-z0-9_-]{0,31}`; not used yet |
| `login` | The account the token belongs to. Reviews are requested from, published by, and approved as this account |
| `token_env` | Environment variable that holds the token; it must start with `AUTOREVIEW_` |
| `match` | `["*"]` |

How the credential applies:

- **Its login is the reviewer.** The `review-requested` trigger means a
  request for the credential's `login`; a pull request authored by that
  login is its own pull request and gets a comment.
- **The token stays with `gh`.** Every `gh` call gets the token through
  `GH_TOKEN` in that child's environment. No other process that autoreview
  starts sees it.
- **Startup checks the login.** `serve` asks GitHub for the token's login
  before it starts and refuses with `configuration_invalid` naming
  `github.credentials.login` when the token belongs to another account.
  Checks while running and the suspension of a failing credential are planned
  in #19.
- **Expiry.** A warning before a token expires is planned in #19.

A fine-grained token covers one resource owner, and an organization may
require an admin to approve it before it sees anything. See
[GitHub identity](knowledge/concepts/github-identity.md) for the permissions a
token needs and when to use a machine user.

## pohunek

| Key | Meaning |
|---|---|
| `bin` | Absolute path of the `pohunek` CLI |
| `state_home` | `XDG_STATE_HOME` for the CLI |
| `runtime_dir` | `XDG_RUNTIME_DIR` for the CLI |
| `timeout_seconds` | Timeout of one pohunek call, including a session start with its base fetch |
| `hosts` | pohunek host names that may run reviewers, in priority order; one or more |
| `route_overrides` | Map `owner/repo: {host, project_id}` that pins a repository to one host and project |

For each review autoreview asks the hosts in `hosts` in order for their pohunek
projects and takes the first host with a project that matches the repository, by
origin URL and the pipeline's `project_sources`. A host that fails (unreachable,
a pohunek error, a timeout) is skipped with the warning `review host skipped`
and is not asked again in the same discovery pass. Two matching projects on the
chosen host are `project_route_ambiguous`. When no host has the repository the
review fails with `project_route_not_found`; when a host failed, it fails with the
code of the first skipped host whose code is retried, or of the first skipped host
when none is. A `route_overrides` entry asks only its host.
See [run on several hosts](knowledge/guides/multihost.md).

## session

How reviewer sessions are named, started, read and cleaned up.

| Key | Meaning |
|---|---|
| `group` | pohunek-companion group that every review session is placed in |
| `branch_prefix` | Prefix of the review branches created on the review host; it must be usable in a git ref name (no whitespace, control character, `..`, `@{` or `~^:?*[\`, and no leading `-` or `/`). Change it only while no review runs |
| `name_prefix` | Prefix of the pohunek session names, without control characters. A running review finds its session by this name, so change it only while no review runs |
| `timeout_seconds` | Longest time one reviewer session may run |
| `output_poll_seconds` | How often a running session's output is read |
| `start_max_attempts`, `start_retry_seconds` | Session start attempts inside one review; each attempt gets its own branch |
| `cleanup_max_attempts`, `cleanup_retry_seconds` | Session removal attempts; after them the event `cleanup_manual` is raised |
| `reviewer_run_max_attempts`, `reviewer_run_retry_seconds` | Fresh sessions for a reviewer whose output was unusable |
| `terminal_cols`, `terminal_rows` | PTY size of reviewer sessions |
| `output_max_bytes`, `output_chunk_bytes`, `output_wait_ms` | Bounds for reading session output |

## schedule

| Key | Meaning |
|---|---|
| `poll_interval_seconds` | Time between discovery passes |
| `worker_tick_seconds` | How often the worker checks for reviews to start, resume or expire |
| `max_concurrent_reviews` | Reviews running at once, across all review hosts of this service |
| `debounce_seconds` | Quiet time after a push before a review starts, so a burst of pushes is reviewed once |
| `retry_delays_seconds` | Delays between scheduler attempts of a failed review |
| `step_retry_seconds`, `step_retry_initial_seconds`, `step_retry_max_interval_seconds` | Total time and backoff of a review step that keeps failing transiently |
| `discovery_lock_timeout_seconds`, `discovery_lock_poll_ms` | Concurrent discovery passes take turns on a lock |
| `poll_limit`, `search_page_limit` | GitHub search bounds; a truncated search is an error, never a silent drop |
| `scheduler_max_bytes`, `state_entry_limit` | Size bounds of the discovery state |

## limits

Bounds on what a reviewer may return and what autoreview publishes. A result
over a bound is rejected and the reviewer gets a fresh session.

| Key | Meaning |
|---|---|
| `result_max_bytes` | Size of one reviewer result |
| `max_findings`, `max_tests` | Entries in one result |
| `min_summary_characters`, `max_summary_characters` | A summary must be substantive and bounded |
| `max_title_characters`, `max_body_characters` | Finding title and body |
| `max_discussion_characters` | Discussion of earlier findings handed to a re-review |
| `max_path_characters`, `max_test_characters` | Path and test entry |
| `excerpt_characters` | Length of finding excerpts in notifications |

## store

| Key | Meaning |
|---|---|
| `path` | SQLite database of discovery state and reviews. Back it up with `autoreview backup-db` |
| `review_retention_seconds` | How long a finished review stays |
| `published_retention_seconds` | How long a published decision counts as the baseline of an incremental review |

## cli

| Key | Meaning |
|---|---|
| `review_list_limit` | Most reviews `autoreview reviews` lists |

## publish

| Key | Meaning |
|---|---|
| `signature` | Last line of every review body and inline comment, published as written for every credential. Reviews are posted from a GitHub account, so readers must be told they are machine-written |
| `ai_signed_pattern` | Text that marks a comment as machine-written. Such a comment is never treated as a human decision on an earlier finding |
| `github_body_max_bytes` | Bound of one review body or comment |

## notify

Notifications have three levels (see [policy notifications](policy.md#notifications)).
`routes` sends each level to one named backend. Several backends of the same
type are fine, for example one Matrix room per level. A single event can have
its own route next to the levels, see [event routes](#event-routes).

| Key | Meaning |
|---|---|
| `timeout_seconds` | Timeout of one delivery |
| `max_characters` | Longer messages are cut |
| `backends` | List of named backends, see below |
| `routes` | `action`, `digest` and `log`, each a list of exactly one backend name; optionally an event name with the same value |
| `review_queue.backends` | Backends that receive the hourly review queue |
| `review_queue.max_items` | Most pull requests listed in one queue message |

Each message gets one attempt, bounded by `timeout_seconds`. A message that
its backend does not accept is dropped with a warning, at every level, and
neither the review nor discovery waits for it. `autoreview review-queue` fails with the backend's
error code when a queue backend rejects the message, so its timer shows the
failure. `review_queue.backends` must name one `matrix` backend; a list of
`none` backends only, or an empty list, is refused at load. Several backends
per level are planned in #27.

### Event routes

A key of `routes` that names an event of the
[policy notifications](policy.md#notifications) gives that event its own route:

```yaml
routes:
  action: [matrix-action]
  digest: [matrix-digest]
  log: [matrix-digest]
  approved: [matrix-approvals]   # every other event keeps the route of its level
```

An event with its own route goes only to the backends of that route; every
other event goes to the route of its level. The level still decides the rest:
an `action` event mentions `mention_user` on its own route too, and a failed
`digest` or `log` message is dropped as on its level route. The value follows
the level rules: exactly one `matrix` backend until M3, with the same
`homeserver_url`, `access_token_env` and `mention_user`. A key that names no
event is refused with `configuration_invalid: notify.routes.<key>: names no
notification event`, and a backend name that `backends` does not define with
`configuration_invalid: notify.routes.<key>: names no configured backend`.
Without event keys, routing is by level only.

### Backend `none`

A `none` backend writes nothing but the structured log. It is accepted only in
`review_queue.backends` next to a `matrix` backend. A route that names it is
refused at load with `configuration_invalid: config: invalid Matrix <level> room`
(`<event>` for an event route).
Running without notifications is planned in #29.

### Backend `matrix`

```yaml
- name: matrix-action
  type: matrix
  homeserver_url: https://matrix.example.org
  access_token_env: AUTOREVIEW_MATRIX_TOKEN
  room: "!roomid:example.org"
  # Mentioned only in action messages.
  mention_user: "@you:example.org"
```

The homeserver URL must use HTTPS (plain HTTP only on loopback). Messages are
`m.text`; the review queue is an `m.notice` with empty mentions, so it never
notifies anyone.

### Backend `webhook`

Planned in #29.

Other channels can be added as backend types without changing the policy;
see the [notifications concept](knowledge/concepts/notifications.md).

## mcp

Optional; only `autoreview mcp` reads it, see the
[MCP server guide](knowledge/guides/mcp-server.md). Without it every other
command works as before. When the section is present, every key is required.

| Key | Meaning |
|---|---|
| `listen` | `IP:PORT` the server listens on, behind the TLS reverse proxy |
| `allowed_hosts` | `Host` header values the server accepts, at least one |
| `clients` | List of clients, at least one |
| `clients[].name` | Name in the logs: 1 to 32 lowercase letters, digits or hyphens, unique |
| `clients[].scope` | `read` or `write` |
| `clients[].token_sha256` | SHA-256 digest of the client's token, 64 lowercase hexadecimal characters, unique |

The configuration never holds a token, only its digest.

## Cross-key rules

Besides each key's own range, `validate` enforces:

- `store.path` is a clean absolute path.
- `store.review_retention_seconds` is at least one day, and the policy's
  `notes_handoff.reminder_hours` is shorter than
  `store.published_retention_seconds`.
- `schedule.discovery_lock_timeout_seconds` is at most one hour.
- `session.output_chunk_bytes` and `limits.result_max_bytes` are at most
  `session.output_max_bytes` (itself at most 16 MiB), and
  `session.output_wait_ms` is at most 8000.
- `limits.min_summary_characters` is at most 500 and at most
  `limits.max_summary_characters`; `limits.excerpt_characters` is at most a
  quarter of `notify.max_characters`.
- `publish.github_body_max_bytes` is at most 65536.
- `publish.signature` is one line that starts with `publish.ai_signed_pattern`
  and cannot mention anyone, embed HTML or imitate the review marker.

## Environment variables

| Variable | Meaning |
|---|---|
| `AUTOREVIEW_CONFIG` | Path of `config.yaml` when `--config` is not given |
| Every name in an `*_env` key | A GitHub token or a backend secret. It must start with `AUTOREVIEW_` |

autoreview reads no other variable; `GH_TOKEN` in its own environment is
ignored and is set only for each `gh` child, to the token of its credential.
The `gh` and `pohunek` processes that autoreview starts inherit no variable
named by an `*_env` key or starting with `AUTOREVIEW_`, nor the Go engine's
`GH_TOKEN` and `MATRIX_ACCESS_TOKEN`.

Never put a secret value in `config.yaml`, `policy.yaml` or a prompt. To check
that a variable is set without printing it:
`test -n "$AUTOREVIEW_GH_TOKEN" && echo set`.
