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
stops the service at startup with an error that names it. `autoreview init`
writes a complete file with every key, a recommended value and a comment, so
you start from a working file and change what you need. Secrets are never in
the file; keys ending in `_env` name the environment variable that holds them.

The service finds the file through `--config <path>` or the
`AUTOREVIEW_CONFIG` environment variable; with neither it refuses to start.

## Complete file

The values below are the ones `autoreview init` writes.

```yaml
version: 1
policy_file: /etc/autoreview/policy.yaml

github:
  host: github.com
  gh_bin: /usr/bin/gh
  timeout_seconds: 30
  credential_check_hours: 24
  expiry_warning_days: 14
  # Evaluated top to bottom; the first credential whose match covers a
  # repository reviews it. Each token must belong to its login.
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
  branch_prefix: autoreview/
  name_prefix: autoreview-
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
  max_summary_characters: 20000
  max_title_characters: 512
  max_body_characters: 20000
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
  signature: "Written by AI: autoreview (posted from {login}'s GitHub account, not by a human)"
  # Comments matching this are treated as machine-written in the discussion
  # of earlier findings.
  ai_signed_pattern: "Written by AI:"
  # GitHub accepts at most 65536 characters per review body or comment.
  github_body_max_bytes: 60000

notify:
  timeout_seconds: 30
  max_characters: 8000
  backends:
    - name: log
      type: none
  routes:
    action: [log]
    digest: [log]
    log: [log]
  review_queue:
    backends: [log]
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
| `host` | GitHub host; `github.com` or a GitHub Enterprise Server host |
| `gh_bin` | Absolute path of the GitHub CLI |
| `timeout_seconds` | Timeout of one `gh` call |
| `credential_check_hours` | How often every token is checked while the service runs |
| `expiry_warning_days` | Days before a token's expiry at which `credential_expiring` is raised |
| `credentials` | One or more credentials, see below |

### Credentials

A credential is one GitHub token, the account it belongs to, and the
repositories it serves. Use one credential with `match: ["*"]` for a single
token, or several for fine-grained tokens per owner or per repository, and
for different accounts per owner (for example a machine user for a public
organization and your own account for private repositories).

```yaml
credentials:
  - name: pohunek-core            # fine-grained, only zajca/pohunek
    login: zajca
    token_env: AUTOREVIEW_GH_TOKEN_POHUNEK
    match: [zajca/pohunek]
  - name: pohunekd-site           # fine-grained, only the website repository
    login: pohunekd-review
    token_env: AUTOREVIEW_GH_TOKEN_POHUNEKD_SITE
    match: [pohunekd/pohunek.dev]
  - name: pohunekd-bot            # machine user, the rest of the organization
    login: pohunekd-review
    token_env: AUTOREVIEW_GH_TOKEN_POHUNEKD
    match: [pohunekd/*]
```

| Key | Meaning |
|---|---|
| `name` | `[a-z0-9][a-z0-9_-]{0,31}`, unique; shown in logs and notifications |
| `login` | The account the token belongs to. Reviews of the covered repositories are requested from, published by, and approved as this account |
| `token_env` | Environment variable that holds the token; it must start with `AUTOREVIEW_` |
| `match` | Lowercase `owner/repository`, `owner/*`, or `*` |

How credentials apply:

- **First match wins.** A repository is served by the first credential whose
  `match` covers it; put repository credentials above their owner's. A
  credential an earlier one fully covers is an error, like a shadowed policy
  rule.
- **Every policy rule is covered.** `validate` fails with
  `credential_not_covered` when a repository a rule can match has no
  credential, and with `credential_unused` when a credential serves no rule.
- **Each account is its own reviewer.** The `review-requested` trigger means a
  request for the credential's `login`; a pull request authored by that
  login is its own pull request and gets a comment; `{login}` in
  `publish.signature` is that login. Several credentials may share a login.
- **Tokens never mix.** Every `gh` call gets only the token of the repository
  it concerns, through `GH_TOKEN` in that child's environment. No other child
  process sees any token, and searches run per credential, limited to its
  `match`.
- **Startup is strict, runtime is isolated.** `serve` and `validate` check
  every token: one that is rejected, expired, or belongs to another login
  stops startup (`credential_invalid`, `credential_login_mismatch`). While
  running, a token that fails its check every `credential_check_hours` or is
  rejected by GitHub suspends only its credential: its repositories are not
  reviewed, the `credential_invalid` event is raised, and every other
  credential keeps working. A review in flight retries its GitHub step like
  any transient failure; if the credential is not back within
  `schedule.step_retry_seconds`, the review stops (`review_stopped`) and can be
  retried later. Replacing the token and restarting the service resumes the
  credential. The same name is an error code when a command or the startup
  check fails, and an event when a running service suspends a credential.
- **A review keeps its account.** A review stores the name of the credential
  that served it when it was dispatched and publishes with it, so a waiting
  approval is posted by the account that reviewed. If that credential no
  longer exists or no longer covers the repository after a configuration
  change, the review stops with `credential_not_covered` and a retry uses the
  credential that serves the repository then.
- **Expiry is watched.** GitHub reports a token's expiry date in the
  `GitHub-Authentication-Token-Expiration` response header. Within
  `expiry_warning_days` of it, `credential_expiring` is raised once a day.

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
| `hosts` | pohunek host names that may run reviewers; one or more |
| `route_overrides` | Map `owner/repo: {host, project_id}` that pins a repository registered on more than one host |

For each review autoreview finds the repository among the pohunek projects of
every host in `hosts`, by origin URL and the pipeline's `project_sources`.
Exactly one project must match; none is `project_route_not_found`, several is
`project_route_ambiguous` unless `route_overrides` picks one. See
[run on several hosts](knowledge/guides/multihost.md).

## session

How reviewer sessions are named, started, read and cleaned up.

| Key | Meaning |
|---|---|
| `branch_prefix` | Prefix of the review branches created on the review host |
| `name_prefix` | Prefix of the pohunek session names |
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
| `signature` | Last line of every review body and inline comment. `{login}` is replaced by the login of the credential that publishes the review. Reviews are posted from a GitHub account, so readers must be told they are machine-written |
| `ai_signed_pattern` | Text that marks a comment as machine-written. Such a comment is never treated as a human decision on an earlier finding |
| `github_body_max_bytes` | Bound of one review body or comment |

## notify

Notifications have three levels (see [policy notifications](policy.md#notifications)).
`routes` sends each level to one or more named backends. Several backends of
the same type are fine, for example one Matrix room per level.

| Key | Meaning |
|---|---|
| `timeout_seconds` | Timeout of one delivery |
| `max_characters` | Longer messages are cut |
| `backends` | List of named backends, see below |
| `routes` | `action`, `digest` and `log`, each a list of backend names |
| `review_queue.backends` | Backends that receive the hourly review queue |
| `review_queue.max_items` | Most pull requests listed in one queue message |

A message goes to every backend of its level. An `action` message counts as
delivered only when every one of its backends accepted it; otherwise the step
fails and is retried for the backends that did not accept it, because the person who has to act may watch only one
channel. A `digest` or `log` message that a backend rejects is dropped for that
backend with a warning. `autoreview review-queue` fails with the backend's
error code when a queue backend rejects the message, so its timer shows the
failure.

### Backend `none`

```yaml
- name: log
  type: none
```

Writes nothing but the structured log. Use it to run without notifications.

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

```yaml
- name: team-chat
  type: webhook
  url_env: AUTOREVIEW_WEBHOOK_URL
  format: slack         # json, slack or discord
```

POSTs one JSON body per message. `json` is autoreview's own message object
(level, event, title, text, review id, links); `slack` and `discord` wrap the
text in those services' incoming-webhook format. The URL is a secret for most
services, so it comes from the environment.

Other channels can be added as backend types without changing the policy;
see the [notifications concept](knowledge/concepts/notifications.md).

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

The same variables can come from a file given with `--env-file` (owner-only,
mode `0600`), which the workflow plugin uses; see the [CLI reference](cli.md).

Never put a secret value in `config.yaml`, `policy.yaml` or a prompt. To check
that a variable is set without printing it:
`test -n "$AUTOREVIEW_GH_TOKEN" && echo set`.
