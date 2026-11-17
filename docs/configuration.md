---
type: Reference
id: reference/configuration
title: Configuration reference
description: Every key of config.yaml - GitHub account, pohunek hosts, sessions, scheduling, limits, state store, publication and notifier backends - and the environment variables that carry secrets.
source_kind: manual
intents: [setup, update, debug, help]
---

# Configuration reference

> **Status: after 0.6.0.** This page describes autoreview on `main` after the
> 0.6.0 release. A part that is not available names its open issue.

`config.yaml` holds the technical settings of one autoreview service: which
GitHub account it acts as, which pohunek hosts run the reviewers, how often it
polls, its limits, where it keeps state and where notifications go. What gets
reviewed and how is in [policy.yaml](policy.md).

**Every key is required**, except `publish.marker`, `pohunek.runtime_dir` on macOS
and the optional sections `pohunek.host_state`, `mcp`, `metrics`, `eval`, `log` and `guards`.
autoreview has no built-in defaults: a missing key stops the service at startup
with an error that names it.
[`examples/config.yaml`](../examples/config.yaml) holds every key with a
recommended value and a comment. It loads once `policy_file` names a valid policy
and the variables of its `*_env` keys are set; its `notify` block routes
every level and the review queue to one `matrix` backend with placeholder values.
CI loads it with every example policy. Secrets are never in
the file; keys ending in `_env` name the environment variable that holds them, or the
line of the `--env-file` that does. Such a name must match `AUTOREVIEW_[A-Z0-9_]+`, as a
name in the `--env-file` must: `AUTOREVIEW_` and at least one of `A-Z`, `0-9` and `_`. Any
other name is `configuration_invalid` naming the key.

The service finds the file through `--config <path>` or the
`AUTOREVIEW_CONFIG` environment variable; with neither it refuses to start.

A `config.yaml` written for 0.4.0 needs five more keys from 0.5.0 on:
`schedule.usage_limit_retry_delays_seconds`,
`schedule.outcome_check_interval_seconds`, `limits.max_issue_characters`,
`store.outcome_retention_seconds` and `pohunek.head_checkout_host_hooks`.
`validate` names each missing one. A rollback
to 0.4.0 removes them again, see
[roll back](knowledge/runbooks/upgrade-and-backup.md#roll-back).

A `config.yaml` written before the repository review rules needs one more key,
`limits.max_review_rules_bytes`, together with `repository_review_rules` on
every pipeline of the [policy](policy.md#pipelines).

## Complete file

The values below are those of `examples/config.yaml`.

```yaml
version: 1
policy_file: /etc/autoreview/policy.yaml

github:
  host: github.com
  gh_bin: /usr/bin/gh
  timeout_seconds: 30
  credential_check_hours: 24
  expiry_warning_days: 14
  # The first credential whose match covers a repository serves it. Its token
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
  # refuse: a checkout: head review never starts on a host whose own pohunek
  # hook scripts would run in the pull request's worktree (see host inspect).
  head_checkout_host_hooks: refuse

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
  usage_limit_retry_delays_seconds: [900, 1800, 3600, 3600, 3600, 3600, 3600]
  step_retry_seconds: 900
  step_retry_initial_seconds: 2
  step_retry_max_interval_seconds: 60
  discovery_lock_timeout_seconds: 300
  discovery_lock_poll_ms: 500
  poll_limit: 100
  search_page_limit: 20
  scheduler_max_bytes: 16777216
  state_entry_limit: 10000
  # A pull request with open finding outcomes is read at most this often to
  # see whether it was merged or closed.
  outcome_check_interval_seconds: 3600

limits:
  result_max_bytes: 1048576
  max_findings: 100
  max_tests: 100
  min_summary_characters: 40
  max_summary_characters: 1500
  max_title_characters: 512
  max_body_characters: 2500
  max_discussion_characters: 40000
  max_issue_characters: 20000
  max_review_rules_bytes: 32768
  max_path_characters: 4096
  max_test_characters: 1024
  excerpt_characters: 360

store:
  path: /var/lib/autoreview/autoreview.db
  review_retention_seconds: 2592000
  # A published decision older than this does not serve as the baseline of
  # an incremental review.
  published_retention_seconds: 7776000
  # The outcome of a published finding is kept this long after its last change.
  outcome_retention_seconds: 31536000

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
  # Name of the hidden review markers: autoreview, or hermes-pr-review for merge
  # gates that read that name. Without the key, hermes-pr-review.
  marker: autoreview

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
| `credential_check_hours` | Hours between two checks of every token while `serve` runs, 1-8760; `serve` also checks once when it starts |
| `expiry_warning_days` | Days before a token's expiry from which a check raises `credential_expiring`, 1-366 |
| `credentials` | One or more credentials; the first whose `match` covers a repository serves it, see below |

### Credentials

A credential is one GitHub token and the account it belongs to. Each
repository is served by the first credential whose `match` covers it, so put
repository credentials above their owner's. The loader refuses:

- a `match` entry that an earlier entry covers, such as `acme/*` after `*` or
  `acme/api` after `acme/*`, with `configuration_invalid` naming
  `github.credentials.match`;
- a policy rule with a repository that no credential matches, with
  `credential_not_covered`;
- a credential that serves no repository of the policy, because the policy
  reviews none of its repositories or earlier credentials take them all, with
  `credential_unused`.

Every command accepts several credentials. Each `gh` call gets only the token of
the credential that serves its repository, and a review keeps the credential it
was dispatched with ([GitHub identity](knowledge/concepts/github-identity.md)).

```yaml
credentials:
  - name: default
    login: review-bot
    token_env: AUTOREVIEW_GH_TOKEN
    match: ["*"]
```

| Key | Meaning |
|---|---|
| `name` | Checked against `[a-z0-9][a-z0-9_-]{0,31}`; error messages name the credential by it. A dispatched review keeps the name it was dispatched with, so a rename stops it with `credential_not_covered` until `autoreview retry` |
| `login` | The account the token belongs to. Reviews are requested from, published by, and approved as this account. A dispatched review keeps the login it was dispatched with, so a login change stops it with `credential_not_covered` until `autoreview retry` |
| `token_env` | Environment variable that holds the token; its name must match `AUTOREVIEW_[A-Z0-9_]+` |
| `match` | The repositories the credential serves: lowercase `owner/repository`, `owner/*` or `*` |

How the credential applies:

- **Its login is the reviewer.** The `review-requested` trigger means a
  request for the credential's `login`; a pull request authored by that
  login is its own pull request and gets a comment.
- **Its login is a user login, in lowercase.** A plain login such as
  `review-bot`, or an Enterprise Managed User login: the handle, an underscore
  and the enterprise short code, such as `mona-cat_octo`. Write it in lowercase
  even when GitHub shows capitals from the identity provider. An app's
  `name[bot]` is refused.
- **The token stays with `gh`.** Every `gh` call gets the token through
  `GH_TOKEN` in that child's environment. No other process that autoreview
  starts sees it.
- **Startup checks every token.** Before it starts, `serve` asks GitHub for
  the login behind each credential's token. A token that GitHub refuses fails
  with `credential_invalid`, a token of another account with
  `credential_login_mismatch`; the message names the credential.
  `validate --check-credentials` runs the same check. While it runs, `serve`
  checks every token again every `credential_check_hours`, and a refused token
  suspends only its credential's repositories, see
  [suspension](knowledge/concepts/github-identity.md#suspension).
- **Expiry.** The check reads the token's expiry from GitHub;
  `validate --check-credentials` prints it as `expires_at`, `null` for a token
  without an expiry. A token that expires within `expiry_warning_days` raises
  `credential_expiring`, at most once a UTC day per credential while `serve`
  runs, see [expiry](knowledge/concepts/github-identity.md#expiry).

A fine-grained token covers one resource owner, and an organization may
require an admin to approve it before it sees anything. See
[GitHub identity](knowledge/concepts/github-identity.md) for the permissions a
token needs and when to use a machine user.

## pohunek

| Key | Meaning |
|---|---|
| `bin` | Absolute path of the `pohunek` CLI; `validate` and `serve` refuse a release outside the [supported range](install.md#prerequisites) |
| `state_home` | `XDG_STATE_HOME` for the CLI |
| `runtime_dir` | `XDG_RUNTIME_DIR` for the CLI. Required on Linux. On macOS (from 0.6.0 on, #513) it may be left out or given no value (`runtime_dir:` or `~`): the CLI then gets no `XDG_RUNTIME_DIR` and dials pohunek's macOS default, `/private/tmp/pohunek-<uid>`. That is the daemon's socket only when `pohunek service install` ran without `XDG_RUNTIME_DIR`: pohunek gives its launchd daemon the `XDG_RUNTIME_DIR` of the install when it is set, and then this key must name that value, as `scripts/setup` writes it. An empty or whitespace string is refused |
| `timeout_seconds` | Timeout of one pohunek call, including a session start with its base fetch |
| `hosts` | pohunek host names that may run reviewers, in priority order; one or more |
| `route_overrides` | Map `owner/repo: {host, project_id}` that pins a repository to one host and project; an entry's optional `untrusted: {host, project_id}` pins its untrusted authors' pull requests instead |
| `head_checkout_host_hooks` | `refuse` or `allow`. A `checkout: head` session always starts with `--no-repo-hooks`, so the pull request's own pohunek hooks never run, but the host's own scripts (`hooks/<event>` in pohunek's config directory) still run in its worktree. `refuse` starts such a review only when `pohunek host inspect --json` reports `host_hooks` with no script (`false`) for `post_create`, `session_start`, `session_stop`, `agent_state` and `pre_remove`; every other event it names must be `false` too, except `pre_create` and `post_remove`, which run in the project repository. It refuses the review with `review_host_hooks_refused` otherwise, also when the host does not report `host_hooks`. `allow` starts it whatever scripts the host has; use it only when those scripts run nothing from the checkout. A `checkout: base` review is not affected |

For each review autoreview asks the hosts in `hosts` in order for their pohunek
projects and takes the first host with a project that matches the repository, by
origin URL and the pipeline's `project_sources`. A host that fails (unreachable,
a pohunek error, a timeout) is skipped with the warning `review host skipped`
and is not asked again in the same discovery pass. Two matching projects on the
chosen host are `project_route_ambiguous`. When no host has the repository the
review fails with `project_route_not_found`. When a host failed and no other host
has the repository, the review waits for a review host instead: it uses no retry
of `schedule.retry_delays_seconds` and starts on the first pass after a host
answers (see
[waiting for a review host](knowledge/concepts/state-and-recovery.md#waiting-for-a-review-host)).
A `route_overrides` entry asks only its host.
Its `untrusted` route is taken by every pull request whose author the rule does
not trust, and by one whose class is not known; it asks only its own host, so a
failing host or a missing project never moves such a review to the entry's host.
Without `untrusted` all authors take the entry's route. With it the entry still
pins the trusted reviews of the repository, so they have no failover and skip
`pohunek.host_state`. A review queued or running when `untrusted` is added
keeps the route stored in its input, also after a restart; the new route
applies from the next dispatch of the pull request (a new head, a retry, a
retarget). When `untrusted` is present both its keys are required, also when
its value is null (`configuration_missing: pohunek.route_overrides.untrusted.host`).
See [run on several hosts](knowledge/guides/multihost.md) and
[untrusted authors](knowledge/safety/untrusted-authors.md#separate-review-host).

### pohunek.host_state

Optional. Without it the hosts are tried in the order of `hosts` and no host
state is read. With it, a repository without a `route_overrides` entry tries only
the hosts whose state the rules allow, in the order the rules set. The state is
the `HostCapacity` document of the `moc.host-state` pohunek plugin, read once per
host and discovery pass with `pohunek host command run HOST moc.host-state
host-state --timeout-ms MS` (`read_timeout_seconds` in milliseconds); each
review host needs a pohunek daemon that runs package commands (0.38.1 or
later) with the plugin enabled and selected. Every key is required
when the section is present; `host_state:` without a value is refused with
`configuration_missing: pohunek.host_state.max_age_seconds`.

| Key | Meaning |
|---|---|
| `max_age_seconds` | Largest distance of the state's `timestamp` from now, either way; a state further off is unknown. Positive |
| `read_timeout_seconds` | Time limit of one state read, passed to the daemon as `--timeout-ms`; a read that the daemon stops is an unknown state. From 1 to 60 (the daemon lowers a larger limit to 60 s), and at most `pohunek.timeout_seconds` minus 2: the pohunek CLI waits 1 s longer than the limit for the answer, and that wait must end before the call's own timeout |
| `unknown_state` | `skip` or `eligible`: a host whose daemon answers but cannot run the plugin's command or stops it at `read_timeout_seconds` (`host_state_unavailable`), whose state does not read as `HostCapacity` v1 (`host_state_invalid`), or whose timestamp is too far from now, either way (`host_state_stale`) |
| `order` | `idle_first`: hosts that report `idle` first, each group in the order of `hosts`; `configured`: the order of `hosts` |
| `on_battery` | `skip` or `eligible`: a host that runs on battery |
| `min_battery_percent` | Lowest battery charge of a host on battery, from 0 to 100; `0` sets no limit |
| `min_free_memory_mb` | Least free memory, in MB; `0` sets no limit |
| `required_models` | Local models that a host must serve, with its model server reachable; `[]` sets no requirement |
| `outside_window` | `skip` or `eligible`: a host outside the policy window of its plugin configuration |

```yaml
pohunek:
  hosts: [review-host-a, review-host-b]
  route_overrides: {}
  host_state:
    max_age_seconds: 120
    read_timeout_seconds: 20
    unknown_state: skip
    order: idle_first
    on_battery: skip
    min_battery_percent: 0
    min_free_memory_mb: 8192
    required_models: []
    outside_window: skip
```

A host whose state is unknown logs `review host state unknown` at WARN with the
code. A state read that fails as a pohunek call (the host does not answer
within `pohunek.timeout_seconds`, a daemon error other than a `command_*` code)
logs `review host state not read` and counts as a failed host of the pass, as a
failed project list does: the review waits for a review host and the outage is
announced. A plugin that hangs is stopped by the daemon at
`read_timeout_seconds` and is an unknown state, not an outage. The CLI waits
1 s longer for the daemon's answer, and the loader keeps that wait below
`pohunek.timeout_seconds`; an answer that needs more than that 1 s to come back
from the host ends the CLI's wait and is a failed read. A host that a rule
refuses logs `review host skipped` with
`error_code` `host_state_ineligible` and the `rule` (the key, or
`unknown_state`). It is asked for its projects only when no allowed host has the
repository: when a refused host has it, the review waits for a review host with
`host_state_ineligible`; when no host has it, the review fails with
`project_route_not_found` as without the section. A pinned repository is never
filtered by state.

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
| `debounce_seconds` | Quiet time after a push before a review starts, so a burst of pushes is reviewed once. From 1 to 600 |
| `retry_delays_seconds` | Delays between scheduler attempts of a failed review: 1 to 5 strictly increasing values, each at most 3600. A review that waits for a review host uses none of them. Their number also bounds the starts again of a head whose fetched `refs/pull/<n>/head` is at another commit (`pohunek_head_fetch_mismatch`) |
| `usage_limit_retry_delays_seconds` | Delays between scheduler attempts of a review that failed with `reviewer_usage_limited`, the rate or usage limit of the reviewer's model account: 1 to 24 values, none below the one before it, each from 1 to 21600. These attempts use none of `retry_delays_seconds`. Make their sum cover the limit window of the account; the example's 5 h 45 min covers a 5-hour window. Required |
| `step_retry_seconds`, `step_retry_initial_seconds`, `step_retry_max_interval_seconds` | Total time and backoff of a review step that keeps failing transiently |
| `discovery_lock_timeout_seconds`, `discovery_lock_poll_ms` | Concurrent discovery passes take turns on a lock |
| `poll_limit`, `search_page_limit` | GitHub search bounds; a truncated search is an error, never a silent drop |
| `scheduler_max_bytes`, `state_entry_limit` | Size bounds of the discovery state |
| `outcome_check_interval_seconds` | Shortest time between two reads of a pull request that left the search while it has open [finding outcomes](knowledge/concepts/finding-outcomes.md), to see whether it was merged or closed. Required |
| `signal_check_interval_seconds` | Optional. Shortest time between two [signal reads](knowledge/concepts/finding-outcomes.md#signals-from-github) of a pull request with open finding outcomes: a dismissed review that requested changes, and the reactions of the pull request author to inline findings. A positive number of seconds; absent, autoreview makes none of these GitHub reads |

## limits

Bounds on what a reviewer may return and what autoreview publishes. A result
over a bound is rejected and the reviewer gets a fresh session.

Every reviewer request states the summary, title and body bounds. The schema of
a `claude-structured` request also carries the summary bounds as `minLength`
and `maxLength`, so Claude Code asks the reviewer again within the run when the
summary is too short or too long.

| Key | Meaning |
|---|---|
| `result_max_bytes` | Size of one reviewer result |
| `max_findings`, `max_tests` | Entries in one result |
| `min_summary_characters`, `max_summary_characters` | Summary length; stated to the reviewer |
| `max_title_characters`, `max_body_characters` | Finding title and body |
| `max_discussion_characters` | Discussion of earlier findings handed to a re-review |
| `max_issue_characters` | Titles and bodies of the issues a pull request closes, handed to the reviewers of a pipeline with `intent_check: issues` ([policy](policy.md#pipelines)); a longer text is cut and marked |
| `max_review_rules_bytes` | Size of the `REVIEW.md` that a pipeline with `repository_review_rules: read` reads from the base commit ([policy](policy.md#pipelines)); a larger file fails the review with `review_rules_too_large`. Every adapter profile's `--maximum-input-bytes` must stay at least 7 times this value plus the prompt, because JSON escaping can make one byte of the text 7 bytes of the request |
| `max_path_characters`, `max_test_characters` | Path and test entry |
| `excerpt_characters` | Length of finding excerpts in notifications |

## store

| Key | Meaning |
|---|---|
| `path` | SQLite database of discovery state and reviews. Back it up with `autoreview backup-db` |
| `review_retention_seconds` | How long a finished review stays |
| `published_retention_seconds` | How long a published decision counts as the baseline of an incremental review |
| `outcome_retention_seconds` | How long the [outcome of a published finding](knowledge/concepts/finding-outcomes.md) stays after its last change |

## cli

| Key | Meaning |
|---|---|
| `review_list_limit` | Most reviews `autoreview reviews` lists |

## publish

| Key | Meaning |
|---|---|
| `signature` | Last line of every review body and inline comment, published as written for every credential. Reviews are posted from a GitHub account, so readers must be told they are machine-written |
| `ai_signed_pattern` | Text that marks a comment as machine-written: the last line of the comment starts with it, leading symbols such as an emoji ignored on both sides. Such a comment is never treated as a human decision on an earlier finding |
| `github_body_max_bytes` | Bound of one review body or comment |
| `marker` | Name of the two hidden markers that start every review body, `<!-- NAME:v1:...-->` and `<!-- NAME-base:v1:...-->`: `autoreview`, or `hermes-pr-review` for merge gates that read that name. Optional: without the key, `hermes-pr-review`. A retry finds a review published under either name, so the name can change while reviews run |

## notify

Notifications have three levels (see [policy notifications](policy.md#notifications)).
`routes` sends each level to one or more named backends. Several backends of
the same type are fine, for example one Matrix room per level. A single event can have
its own route next to the levels, see [event routes](#event-routes).

| Key | Meaning |
|---|---|
| `timeout_seconds` | Timeout of one delivery |
| `max_characters` | Longer messages are cut |
| `backends` | List of named backends, see below |
| `routes` | `action`, `digest` and `log`, each a list of one or more backend names; optionally an event name with the same value |
| `review_queue.backends` | Backends that receive the hourly review queue |
| `review_queue.max_items` | Most pull requests listed in one queue message |

Each message gets one attempt on each backend of its route, bounded by
`timeout_seconds`. A backend that does not accept the message drops it with a
warning, at every level; the other backends still get it, and neither the
review nor discovery waits for it. A message of a review counts as delivered
only when every backend took it. On a route with several backends a review
resumed after a shutdown sends an `action` message only to the backends that
have not taken it; a one-backend route sends it again after a shutdown, and a
`digest` or `log` message also goes again to a backend that answered during the
shutdown, because a dropped failure looks the same. `autoreview review-queue` posts to every
queue backend and fails with the error code of the first one that rejects the
message, so its timer shows the failure. A route or `review_queue.backends`
that is empty or names a backend twice is refused at load. Each backend has its
own settings, so two `matrix` backends may use different homeservers, tokens or
mentioned users.

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
the level rules: one or more backends, each named once. A key that names no
event is refused with `configuration_invalid: notify.routes.<key>: names no
notification event`, and a backend name that `backends` does not define with
`configuration_invalid: notify.routes.<key>: names no configured backend`.
Without event keys, routing is by level only.

### Backend `none`

```yaml
- name: quiet
  type: none
```

A `none` backend delivers nothing. Each message routed to it is logged as
`notification logged only` with the backend name, the event and the level; the
text is not logged. A review queue with only `none` backends sends nothing, and
`autoreview review-queue` still prints the queue.

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

The homeserver URL must use HTTPS (plain HTTP only on `127.0.0.1`). Messages are
`m.text`; the review queue is an `m.notice` with empty mentions, so it never
notifies anyone.

### Backend `webhook`

```yaml
- name: team-chat
  type: webhook
  # The variable holds the incoming-webhook URL; the URL is a secret.
  url_env: AUTOREVIEW_WEBHOOK_URL
  format: slack   # json, slack or discord
```

One HTTP `POST` with a JSON body per message, `Content-Type: application/json`.
The URL must use HTTPS (plain HTTP only on `127.0.0.1`) and is read only from the
variable that `url_env` names. It never appears in errors, logs or `validate`
output: a refused URL fails with `configuration_invalid:
notify.backends.<name>.url_env: webhook URL must use HTTPS or loopback HTTP`.

| `format` | Body |
|---|---|
| `json` | `{"event": "<event>", "level": "<level>", "text": "<text>"}`; the review queue is `{"event": "review_queue", "text": "<text>"}` |
| `slack` | `{"text": "<text>"}` with `&`, `<` and `>` escaped, so the text cannot form a mention or a link; Slack-compatible services such as Mattermost take it too |
| `discord` | `{"content": "<text>", "allowed_mentions": {"parse": []}}`, cut to 2000 UTF-16 code units, so no `@everyone`, user or role is pinged |

The text is the plain text of the message, cut to `max_characters`. A webhook
mentions nobody, also at the `action` level. The review queue sends its plain
text. Any `2xx` answer is a delivery; `429` and `5xx` fail with
`webhook_unavailable`, every other answer with `webhook_request_rejected`, and a
URL that does not parse with `webhook_request_invalid`. A redirect (`3xx`) is not
followed and fails with `webhook_request_rejected`, and no request carries a `Referer`,
so the URL and the text never reach another host. At the `digest` and
`log` levels a review message that fails is dropped with the warning `webhook
notification dropped`.

Other channels can be added as backend types without changing the policy;
see the [notifications concept](knowledge/concepts/notifications.md).

## mcp

Optional; see the [MCP server guide](knowledge/guides/mcp-server.md). When the
section is present, every command validates it at load, so a broken section
stops `serve` and `validate` too; only `autoreview mcp` requires it. Without it
every other command works as before. When the section is present, every key is
required.

| Key | Meaning |
|---|---|
| `listen` | `IP:PORT` the server listens on, behind the TLS reverse proxy |
| `allowed_hosts` | `Host` header values the server accepts, at least one |
| `allowed_hosts` entries | Each a bare `host` or `host:port`; a scheme, a path or a user part is refused |
| `request_timeout_seconds` | Longest time one request may take, from reading its body to its answer; a slower request gets 503 and its tool call is cancelled. It must cover the longest tool call: `retry` runs a whole discovery pass. On shutdown the server waits this long for requests in flight. From 1 to 86400 (one day) |
| `header_timeout_seconds` | Longest time a client may take to send the request headers. From 1 to 86400 (one day) |
| `request_max_bytes` | Largest request body; a larger one gets 413. From 1 to 16777216 (16 MiB) |
| `max_connections` | Most connections served at once; further connections wait to be accepted. From 1 to 4096 |
| `clients` | List of clients, at least one |
| `clients[N].name` | Name in the logs: 1 to 32 lowercase letters, digits or hyphens, unique |
| `clients[N].scope` | `read` or `write` |
| `clients[N].token_sha256` | SHA-256 digest of the client's token, 64 lowercase hexadecimal characters, unique |

The configuration never holds a token, only its digest. A missing or invalid
client field is reported with its index, for example
`configuration_missing: mcp.clients[1].scope`.

## metrics

Optional; see [Prometheus metrics](stats.md#prometheus-metrics). Without it
`serve` opens no metrics port and counts nothing for it. When the section is
present, every command validates it at load, so a broken section stops `serve`
and `validate` too, and every key is required; a `metrics:` key without a value
is refused with `configuration_missing: metrics.listen`. Only `autoreview serve`
runs the endpoint.

| Key | Meaning |
|---|---|
| `listen` | `IP:PORT` the endpoint listens on; keep it on loopback, such as `127.0.0.1` |
| `allowed_hosts` | `Host` header values the endpoint accepts, at least one |
| `allowed_hosts` entries | Each a bare `host` or `host:port`; a `host` alone accepts any port |
| `request_timeout_seconds` | Longest time one connection may stay open; it serves one request and is then closed. On shutdown `serve` waits this long for the connections. From 1 to 86400 |
| `header_timeout_seconds` | Longest time a client may take to send the headers. From 1 to 86400 |
| `max_connections` | Most connections served at once; more wait to be accepted. From 1 to 4096 |

```yaml
metrics:
  listen: "127.0.0.1:9464"
  allowed_hosts: ["127.0.0.1"]
  request_timeout_seconds: 10
  header_timeout_seconds: 10
  max_connections: 4
```

The endpoint checks no token: it serves counts and the names of repositories,
review hosts, rules and error codes only. Put a reverse proxy with
authentication in front of it when a scraper on another host reads it; see the
[threat model](knowledge/safety/threat-model.md). The endpoint is bound before
the engine lock, so a second `serve` with the same section fails with
`metrics_listen_failed`; see [`autoreview serve`](cli.md#autoreview-serve).

## eval

Optional; only `autoreview eval run`, `autoreview eval add` and
`autoreview eval prune` read it. When the
section is present, every command validates it at load and every key is
required; an `eval:` key without a value is refused with
`configuration_missing: eval.credential`. Run an eval from its own config file:
`eval.credential` must name an entry of `github.credentials` with a read-only
token, and the credential check of a configuration refuses a credential that no
policy rule serves, so the eval file holds a policy that covers the case
repositories and that credential.

The eval prefixes are checked against `session.name_prefix` and
`session.branch_prefix` of the same file, so set both to the values of the
production file; the check cannot see a production prefix that the eval file
does not repeat.

| Key | Meaning |
|---|---|
| `credential` | Name of an entry of `github.credentials`, a read-only credential that every request of an eval uses |
| `session_name_prefix` | Prefix of the eval reviewer session names; it must not start with, or be the start of, `session.name_prefix` |
| `session_branch_prefix` | Prefix of the eval reviewer branches, usable in a git ref name; it must not start with, or be the start of, `session.branch_prefix`, so `prune-branches` never deletes an eval branch |
| `max_concurrent_trials` | Most trials that run at once, from 1 to `limits.max_concurrent_reviews` |
| `max_trials` | Most trials (cases times repeats) of one run; a larger run is refused before any session starts |
| `max_sessions` | Most reviewer and verifier sessions of one run, retries included |
| `trial_timeout_seconds` | Longest time of one trial, from 1 to 86400 |

```yaml
eval:
  credential: eval-readonly
  session_name_prefix: eval-review-
  session_branch_prefix: autoreview/eval/
  max_concurrent_trials: 2
  max_trials: 60
  max_sessions: 240
  trial_timeout_seconds: 2700
```

## log

Optional. Without it autoreview logs to stderr only: the journal under systemd,
the launchd output file on macOS. With it, `serve`, `review-queue` and `mcp`
write their log lines to a file of their own in `directory` and rotate it by
size. When the section is present, every command validates it at load, so a
broken section stops `validate` too, and every key is required; a `log:` key
without a value is refused with `configuration_missing: log.directory`.

| Key | Meaning |
|---|---|
| `directory` | Clean absolute path of the directory that holds the log files, without control characters. autoreview creates it with mode 0700 when it is missing; a path that names an existing file is refused |
| `max_file_bytes` | Size from which a log file is rotated, in bytes. From 1048576 (1 MiB) to 1073741824 (1 GiB) |
| `kept_files` | Rotated files kept besides the current one. From 1 to 100 |

```yaml
log:
  directory: /var/lib/autoreview/logs
  max_file_bytes: 10485760
  kept_files: 5
```

Each command writes one file in `directory`; the other commands validate the
section and write none:

| Command | File |
|---|---|
| `serve` | `autoreview.log` |
| `review-queue` | `autoreview-queue.log` |
| `mcp` | `autoreview-mcp.log` |

Before a line that would take the file past `max_file_bytes`, the file becomes
`<file>.1`, `<file>.1` becomes `<file>.2` and so on up to
`<file>.<kept_files>`, whose old content is replaced, and a new file is
started. A line is never split, so a line longer than the limit fills a new file
alone and that file passes the limit by that one line. The new file is created
as `<file>.new` before any rename, so when that or a rename fails, the line is
appended to the current file, which may then pass the limit, and the rotation is
tried again after 60 seconds; only when the last rename fails, after the current
file became `<file>.1`, lines go to `<file>.1` until the retry, which only
renames `<file>.new` into place and moves no other file.

With the file in use, a log line goes to the file only. stderr gets only the
lines the file could not take, in full; one error line of the writer per 60
seconds (`log write failed` or `log rotation failed`, with the count of such
lines held back since the previous one); and panics. A line the file takes only
in part is cut out of the file again, so the file never holds a partial line; if
that fails too, the next line starts on a new line and only the cut piece is
not valid JSON. Lines logged before the
configuration is read go to stderr. The JSON that a command prints on stdout does
not change.

Created files have mode 0600 and a created directory 0700; existing modes are
left alone. When the directory cannot be created or a file cannot be opened, the
command stops with `configuration_invalid` naming `log.directory` and the system
error.

A command holds an exclusive `flock` on `<file>.lock` until it exits. A second
process of the same command on the same directory, or a filesystem that cannot
lock, leaves the file alone: the process logs one `log file in use` warning on
stderr and keeps logging there.

## guidance

Optional. Without it autoreview counts nothing and sends no `guidance_suggested`
event. With it, a repository whose reviews keep finding the same kind of mistake
gets one suggestion to write a rule for it in its agent guidance (`AGENTS.md` or
`CLAUDE.md`). A reviewer may give each finding a `category`, a slug such as
`missing-test`. When `recurrence_threshold` findings of one category, in one
repository, were settled as `fixed` within the last `window_days` days and after
the last suggestion for that category, the review that settled the last of them
raises the event [`guidance_suggested`](policy.md#notifications). It names the
repository, the category and the pull request, key and body of each counted
finding, and its route follows `notify.routes` like any event; its default level
is `digest`. Raising it records the time, so the next suggestion for the
category needs a new set of fixed findings. A finding is counted from the
publication that follows the first start with the section; findings published
before are not counted. The count stops at `store.outcome_retention_seconds`,
after which the finding outcomes are pruned. autoreview only suggests: it writes
to no repository and opens no pull request.

When the section is present, every command validates it at load, so a broken
section stops `validate` too, and every key is required; a `guidance:` key
without a value is refused with `configuration_missing:
guidance.recurrence_threshold`.

| Key | Meaning |
|---|---|
| `recurrence_threshold` | Findings of one category that raise a suggestion. From 2 to 100 |
| `window_days` | Days back from now in which a fixed finding counts. From 1 to 365 |

```yaml
guidance:
  recurrence_threshold: 3
  window_days: 30
```

## guards

Optional. `guards.text` holds the settings of the prompt-injection guard that a
policy rule turns on with [`guard`](policy.md#guard). Only reviews of such a
rule read and scan text, with extra GitHub read calls; a review whose stored
rule has a guard while this section is missing is held as suspect without a
scan. When `guards` is present, `guards.text` and
both its keys are required; `guards:` without a value is refused with
`configuration_missing: guards.text`. Every command reads the lexicon file once
at load, so a broken file stops `serve` and `validate` alike.

| Key | Meaning |
|---|---|
| `text.max_input_bytes` | Largest amount of pull request text the guard reads for one review, in bytes. From 1 to 4194304 (4 MiB, the size up to which the guard's scan time is measured). Text is read in field order (title, body, commit messages, comments, review threads, patches); a review whose text reaches the bound is suspect, because the rest was not scanned |
| `text.lexicon_file` | Clean absolute path of the phrase lexicon: UTF-8, at most 65536 bytes. Each line is a weight from 1 to 100, whitespace and a phrase; blank lines and lines that start with `#` are skipped. A file without a phrase, or with a weight out of range, a phrase without a letter or digit or a repeated phrase, is refused with the line number |

```yaml
guards:
  text:
    max_input_bytes: 1048576
    lexicon_file: /etc/autoreview/guard-lexicon.txt
```

A lexicon file:

```text
# weight phrase
40 ignore previous instructions
25 approve this pull request
```

[`examples/guard-lexicon.txt`](../examples/guard-lexicon.txt) is a starter
lexicon. [Enable the text guard](knowledge/guides/text-guard.md) says how to
measure a lexicon and the rule's `guard` on a corpus with
[`autoreview guard scan`](cli.md#autoreview-guard-scan) before a rule uses them.

## Cross-key rules

Besides each key's own range, `validate` enforces:

- `store.path` is a clean absolute path.
- `store.review_retention_seconds` is at least one day, and the policy's
  `notes_handoff.reminder_hours` is shorter than
  `store.published_retention_seconds`.
- `store.review_retention_seconds` is at least the longest
  `author_limit.window_hours` of the policy's rules.
- `schedule.discovery_lock_timeout_seconds` is at most one hour.
- `session.output_chunk_bytes` and `limits.result_max_bytes` are at most
  `session.output_max_bytes` (itself at most 16 MiB), and
  `session.output_wait_ms` is at most 8000.
- `limits.min_summary_characters` is at most 500 and at most
  `limits.max_summary_characters`; `limits.excerpt_characters` is at most a
  quarter of `notify.max_characters`.
- `publish.github_body_max_bytes` is at most 65536.
- `publish.signature` is one line of at most 200 bytes that starts with
  `publish.ai_signed_pattern`, leading symbols such as an emoji ignored on both,
  and cannot mention anyone, embed HTML or imitate the review marker.
  `publish.ai_signed_pattern` needs text after its leading symbols.
- `publish.marker` is `autoreview` or `hermes-pr-review`; `marker:` without a
  value is refused as missing.
- `log.directory` is a clean absolute path and does not name an existing file.
- A policy rule with [`guard`](policy.md#guard) needs `guards.text`; without it
  the load fails with
  `configuration_missing: guards.text: the policy rule for <match> sets guard`.

`validate` also checks these limits. A number outside them fails with
`configuration_invalid` and the message `configured limit is outside the safe
range`, which names no key:

- `schedule.poll_interval_seconds` is at most 3600, and
  `schedule.worker_tick_seconds` at most `schedule.poll_interval_seconds`.
- `schedule.max_concurrent_reviews` is at most 32, and
  `schedule.scheduler_max_bytes` at most 64 MiB.
- `schedule.step_retry_seconds` is at most 3600;
  `schedule.step_retry_max_interval_seconds` lies from
  `schedule.step_retry_initial_seconds` to `schedule.step_retry_seconds`.
- `session.start_max_attempts` is at most 5, `session.cleanup_max_attempts` at
  most 10 and `session.reviewer_run_max_attempts` at most 3; each of their
  `*_retry_seconds` is at most 300.
- `session.output_poll_seconds` is at most `session.timeout_seconds`, and
  `session.output_chunk_bytes` at most 783240.
- `session.terminal_cols` is from 120 to 400, `session.terminal_rows` from 100
  to 250.
- `cli.review_list_limit` is at most `schedule.state_entry_limit`.
- `notify.max_characters` is at most 32000, `notify.review_queue.max_items` at
  most 999.

## Environment variables

| Variable | Meaning |
|---|---|
| `AUTOREVIEW_CONFIG` | Path of `config.yaml` when `--config` is not given |
| Every name in an `*_env` key | A GitHub token or a backend secret. It must match `AUTOREVIEW_[A-Z0-9_]+`; a line of the `--env-file` with the same name wins over it |

autoreview reads no other variable; `GH_TOKEN` in its own environment is
ignored and is set only for each `gh` child, to the token of its credential.
The `gh` and `pohunek` processes that autoreview starts inherit only the
variables listed in
[secrets](knowledge/safety/secrets.md#what-child-processes-receive), never a
variable named by an `*_env` key or starting with `AUTOREVIEW_`.

Never put a secret value in `config.yaml`, `policy.yaml` or a prompt. To check
that a variable is set without printing it:
`test -n "$AUTOREVIEW_GH_TOKEN" && echo set`.
