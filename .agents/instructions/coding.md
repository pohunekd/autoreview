# Coding rules

## Layout and boundaries

- The engine is a Cargo workspace under `crates/`: `go-compat`, `policy`,
  `schedule`, `engine`, `external` and the `autoreview` binary. The `engine`
  crate reaches side effects (processes, HTTP) only through the `Integrations`
  trait; its `clippy.toml` disallows `std::process::Command`,
  `tokio::process::Command` and `reqwest::Client`. Its `mcp` and `metrics`
  modules answer the HTTP requests that the binary's listeners accept; they bind no socket. The imported reference
  engine lives in `cmd/` and `internal/` until it is deleted. Reviewer
  adapters are Python wrappers under `adapters/`, optional integrations under
  `integrations/`, prompts under `prompts/`, examples under `examples/`, and
  the public documentation under `docs/`.
- The service talks to GitHub only through the `gh` CLI and to pohunek only
  through its command-line interface. Do not import or depend on pohunek's
  internal packages; the public protocol is the contract.
- The engine holds the GitHub token; a reviewer run holds none. Only the
  engine's own `gh` calls receive a credential, and `GH_TOKEN` is set only for
  each `gh` child process, to the token of its credential.
- Credentials are an explicit list (`github.credentials`), first match wins, and
  a rule that no credential covers is an error. There is no `GH_TOKEN` shortcut.
- Author classes (trusted, untrusted) decide what a review may publish. An
  untrusted author's pull request can never be auto-approved, and its review
  runs against the trusted base checkout.

## Rust

- **The Rust guidelines are mandatory.** The repository follows the Microsoft
  Pragmatic Rust Guidelines, vendored in-repo at
  [`.agents/rust-guidelines/`](../rust-guidelines/SKILL.md). Before writing or
  modifying any `.rs` file, read the files that apply to the task from that
  directory and apply them; `SKILL.md` is the index of which file to read when.
  Start with `11_universal_guidelines.md` (all Rust work); add
  `02_application_guidelines.md` (binaries, error handling),
  `03_correctness_guidelines.md`, and `06`, `12`, `13`, `14`, `15` (library
  design) as the task warrants. The vendored files are read-only copies; do not
  edit them.
- Toolchain and workspace settings come from `rust-toolchain.toml` and the root
  `Cargo.toml`. A dependency is declared once in `[workspace.dependencies]` and
  inherited by the crates that use it. Cargo commands use `--locked`; the one
  `Cargo.lock` is committed.
- Errors are typed with `thiserror`, one error enum per crate. Handle specific
  variants; a failed `gh` or pohunek call is a result the engine handles, never
  an empty value that looks like success.
- Configuration fails fast (see Configuration below): a missing or invalid key
  returns a typed error that names the key. Fixed values that are not tunables
  are named constants with a doc comment, not inline literals.
- Logging uses `tracing` with `tracing-subscriber` JSON output (see Logging).
- No `unsafe`: the workspace lint is `unsafe_code = "forbid"`. A lint
  suppression uses `#[expect(..., reason = "...")]`, never a bare `#[allow]`.
- Run external programs with an argv list through the `Integrations` trait.
  Never build a shell string from repository content, pull request text, or
  model output.
- Panics are reserved for programmer errors, never for input.

## Go reference (read-only until deleted)

- The Go code (`cmd/`, `internal/`) is the behavioural reference for the port.
  Do not add features to it; change it only to fix a defect the port must
  reproduce or to keep it building. It is deleted once the port reaches parity.
- Go version as in `go.mod` (1.25 or newer). Code is formatted by `gofmt`,
  vetted by `go vet ./...`, and analysed by the staticcheck version pinned in
  `scripts/gates`. A lint suppression needs a comment with the reason.
- Standard library first. Add a module only when it clearly pays for itself,
  and state the reason in the PR.
- Errors are wrapped with `%w` and matched with `errors.Is` or `errors.As`. A
  failed `gh` or pohunek call is a result the engine handles, never an empty
  value that looks like success.
- Pass `context.Context` into every external call; timeouts come from
  configuration.
- Run external programs with `exec.Command` and an argv list. Never build a
  shell string from repository content, pull request text, or model output.
- No `unsafe`. Panics are reserved for programmer errors, never for input.

## Porting from the Go reference

The Rust engine opens the Go engine's database unchanged and continues its
history. Supported cutover: from a drained queue (no row with status `queued`,
`running` or `awaiting`). Resuming a Go review mid-workflow is not supported and
not tested. Rollback to the Go binary is supported under the same drained rule,
because nothing below changes what Go reads. Line references are to the Go
reference as merged in #61.

What is compatible, as rules every slice that defines a persisted type must
follow:

- **R1 Schema.** Same DDL text and `PRAGMA user_version = 1` (`store.go:37-73`,
  `128-143`), plus the additive Rust-only table `review_step_runs` (#270, under
  "What is not compatible, by decision"); a newer version is refused with
  `store_version_unsupported`. Open
  parameters as `store.go:89-124`: absolute path, directory mode 0700,
  `busy_timeout` 10000, `foreign_keys` on, WAL, `BEGIN IMMEDIATE` for every write
  transaction, one connection.
- **R2 Locks.** Discovery lock file `<db>.discovery.lock` (`store.go:118`),
  engine lock `<db>.discovery.lock.engine` (`engine.go:72`), both mode 0600,
  `flock` exclusive non-blocking via `rustix::fs::flock`, same codes
  `engine_already_running`, `store_unavailable`.
- **R3 Discovery entries.** Names `schedule`, `published`, `awaiting`,
  `rejected` (`discovery.go:17-20`); an unknown name is `store_state_invalid`
  (`store.go:198-199`); a missing or `null` map is empty (`store.go:203-214`).
- **R4 Container default.** Persisted types include the policy types stored
  inside `ReviewInput.rule` and `SessionRequest` (`model.go:122`,
  `integrations.go:51-55`) and `schedule::Record` (discovery entry `schedule`).
  Every persisted struct has `#[serde(default)]` at container level (Go leaves a
  missing field at its zero value) and no `deny_unknown_fields` (Go ignores
  unknown fields). It also has `remote = "Self"` in that attribute and an
  `autoreview_go_compat::object_only_serde!` call, so a JSON array is refused
  as Go refuses it. Read such a type only through the serde traits; its
  inherent `deserialize` function still accepts an array.
- **R5 Null-tolerant collections.** Every `Vec` and map field whose Go tag has
  no `omitempty` deserializes JSON `null` as empty through
  `autoreview_go_compat::null_default` (Go marshals a nil slice or map as
  `null`), for example `Pipeline.reviewers`, `incremental_reviewers`,
  `project_sources` (`policy.go:144-148`) and `Verifier.escalate_severities`
  (`policy.go:164`).
- **R6 Field order and omission.** Fields in Go struct order; a Go `omitempty`
  field gets `skip_serializing_if` with Go's zero test (empty string, 0, false,
  empty collection, `None`).
- **R7 Maps.** Every map in a persisted or hashed type is a `BTreeMap` (Go sorts
  map keys).
- **R8 Integers.** Go `int` and `int64` are `i64`.
- **R9 Time text.** Columns `created_at`, `updated_at`, `completed_at`,
  `approval_deadline` and every `time.Time` JSON field
  (`PublishedRecord.published_at` `model.go:377`, schedule `Record.due_at`
  `schedule/scheduler.go:35`, step `published_at` `review.go:877`) are written
  by one function, `autoreview_go_compat::go_time::format_rfc3339_nano`: UTC,
  `YYYY-MM-DDTHH:MM:SS`, then `.` and the nanoseconds with trailing zeros
  trimmed (nothing when zero), then `Z`, which is Go's `time.RFC3339Nano` in
  UTC. Retention compares this text in SQL (`store.go:495`) and the queue orders
  by it (`store.go:339,387`); a fixed nine-digit format would mis-order Go and
  Rust rows. Reading accepts any RFC3339 value with an offset, including Go's
  zero time `0001-01-01T00:00:00Z`.
  Exception: `Artifact.reviewed_at`, written by `CombineResults`
  (`protocol.go:353`), is Go's `time.RFC3339` in UTC with whole seconds and no
  fraction, written by `autoreview_go_compat::go_time::format_rfc3339_seconds`;
  `testdata/parity/timestamps.json` records both forms per instant.
- **R10 Identifiers.** Byte-identical: `PullRequest::workflow_id` =
  `"prr-v2-" + hex(sha256("pr-review-v2\0{repo}\0{number}\0{head}\0{request_id}\0{attempt}"))`
  (`model.go:68-73`, prefix `model.go:18`), `review_id` (`model.go:62-64`), `key`
  (`model.go:55`), `prior_key` (`model.go:164`, `%.12s` = first 12 bytes of an
  ASCII SHA), session name `github-review-<token>` and branch
  `autoreview/review/<token>` (`integrations.go:64-75`). `enqueueReview`
  deduplicates on `workflow_id` (`store.go:277`) and `priorFindings` requires
  the `prr-v2-` prefix (`discovery.go:126-128`).
- **R11 Input versions.** `ReviewInput` versions 2 to 5 are accepted
  (`model.go:21-32`) with the same version gates (`model.go:269-283`).
- **R12 Published text.** The marker stays exactly
  `<!-- hermes-pr-review:v1:{repo}#{n}@{head}:decision={d}:findings={k}:digest={hex} -->`
  (`publish.go:238-249`): a retry after a lost acknowledgement finds the
  existing review by substring (`github.go:781,806-811`) and external merge
  gates read it (`publish.go:232-236`). The finding heading that
  `discussion.go:29` matches, the signature rule (`config.go:342-352`,
  `discussion.go:51-62`) and `republishedPrefix` `P` (`prior.go:11`) are
  unchanged. #7 changes the marker after cutover and reads both forms. The body
  still starts with the unchanged marker; a comment that names the base branch
  follows it on the same line and does not start with the marker's prefix, so a
  reader that matches the marker as a substring is unaffected (#297, see "What
  is not compatible, by decision").
- **R13 Policy digest.** `digestOf` (`policy.go:657-686`) is reproduced byte for
  byte, including Go's `%s` of `Trigger` (`policy.go:60-65`); checked by golden.
- **R14 Go semantics.** `fmt` `%.12s` truncates by runes (all its inputs are
  ASCII SHAs); `strings` functions work on bytes and `for range` on runes; Go
  RE2 `\b`, `\s`, `\w`, `\d` are ASCII, so ported patterns use `(?-u:...)` or
  explicit ASCII classes; `sort.Strings` is byte order (`Vec<String>::sort`);
  `json.Unmarshal` into an `int` field rejects `1.5` and so does serde for
  `i64`.

What is not compatible, by decision:

- **Configuration:** the Rust binary reads `config.yaml` (#5), not
  `HERMES_PR_REVIEW_*`. The cutover runbook (issue X1) converts the deployed
  environment into `config.yaml` by hand.
- **Logs:** JSON lines from `tracing-subscriber` (`timestamp`, `level`,
  `fields`, `target`) instead of `log/slog` (`time`, `level`, `msg`); log shape
  is not a documented contract. When the Claude wrapper, the Claude outer
  message or the Codex result fails to decode (`review_wrapper_invalid`,
  `claude_result_invalid`, `review_result_invalid`), Go's `error` field holds
  the JSON or base64 decoder text; Rust logs only the code, so no decoder text
  quotes untrusted output. For a Codex run that exits without a result, Rust
  logs `review_result_invalid` with the adapter's reason line: a line of
  Codex's stderr or the adapter's refusal after the run, bounded to 512 bytes
  and neutralized (#283, #304). For a Claude Code run with an exit code other
  than 0, Rust logs `review_wrapper_invalid` with the end of its stderr, bounded
  and neutralized the same way (#304).
- **CLI wording and usage text** (`main.go:16`); the JSON error object keeps its
  keys and codes (`command.go:22-47`).
- **In-flight Go reviews** (statuses `queued`, `running`, `awaiting` at
  cutover): not supported.
- **Discovery notifications (#210):** a discovery pass collects its
  notifications and sends each once after it has saved its state and released
  the discovery lock. It sees a failure at every level, because it sends with
  `Integrations::notify_unmuted`. After a failure, the later notifications of
  the same route (#187) in that batch get no attempt, and after a shutdown none gets
  one (logged with the code `shutdown`). Each one not delivered is logged as
  `discovery notification dropped` and never resent. Go sends them inside the pass under the step
  retry (`discovery.go:36-41`), so an unreachable `action` backend holds the
  lock for `step_retry_seconds`, fails the pass and repeats the message on the
  next pass. The regression test is
  `failing_notification_backend_does_not_stall_discovery`.
- **Event routes (#187):** `notify.routes` accepts an event name as a key next
  to the three levels; that event goes to its own route and every other event
  to the route of its level. The level still decides the mention and the
  muting. The engine picks the route (`Config::notify_route`) and passes it to
  `Integrations::notify` and `notify_unmuted` with the level. Go routes by level
  only (`notify.go:43-45`), and its environment has no event rooms, so a rollback
  to Go sends every event by its level. The regression test is
  `event_routes_carry_their_events_and_the_rest_keep_their_levels`.
- **Review notifications (#212):** a running review gives each notification one
  attempt, bounded by `notify.timeout_seconds`, without the step retry. A
  failure that the notifier returns is logged as `review notification dropped`
  with the event, the review key, the level and the code (`shutdown` after a
  cancel, which stores nothing), and the `notify/...` step is stored as `false`,
  so the review goes on and a resumed review does not resend it. A `digest` or
  `log` failure that the notifier mutes is stored as `true`, as a sent message.
  Go's `notify` reads any stored value as sent. `autoreview review-queue` posts
  its digest once, logs `review queue digest dropped` and fails with the code.
  Go retries both under the step retry (`review.go:212-216`,
  `review_queue.go:79`), so an unreachable `action` backend holds a review slot
  for `step_retry_seconds` and then fails the review. The regression tests are
  `failing_notification_backend_does_not_stall_a_review` and
  `review_queue_sends_once_with_one_transaction_per_run`; the latter replaces
  the Go ports `review_queue_retries_with_one_transaction_per_run` and
  `review_queue_gives_up_when_the_retry_window_ends`.
- **Review host fallback (#180):** the engine asks the hosts of `pohunek.hosts`
  in order and takes the first host whose projects serve the repository. A
  host that fails is skipped with the warning `review host skipped` and is not
  asked again in that discovery pass (its code and permanence are kept); with
  no match the route fails with the first skipped host's error whose code is
  retried, else the first skipped host's error, or `project_route_not_found`
  when every host answered. Two matches on one host stay
  `project_route_ambiguous`, but a repository that two hosts serve runs on the
  first one, which Go refused as ambiguous: with `auto` in `project_sources` an
  untrusted pipeline can reach a trusted host unless `route_overrides` pins it.
  A `route_overrides` entry asks only its host. Go asks every host, fails the
  route when one host fails and reports a repository on two hosts as
  `project_route_ambiguous` (`pohunek.go:85-125`). The route is stored in the
  review input as in Go. The tests are
  `hosts_are_tried_in_order_and_a_failing_host_is_skipped` and
  `failing_first_host_is_skipped_and_the_review_runs_on_the_second`. With
  `pohunek.host_state` (Rust only) the hosts of a repository without a route
  override are first filtered and ordered by their `moc.host-state` state
  (`discovery::host_state`); when no allowed host serves the repository, the
  held-back hosts are asked, and a route that only they serve waits for a host
  as `host_state_ineligible`. A state read that fails as a pohunek call is a host
  failure of the pass; a daemon `command_*` answer (exit status 40 to 52, also
  the daemon's timeout at `read_timeout_seconds`) is an unknown state. The
  tests are `host_state_rules_judge_each_state`,
  `hosts_are_ordered_idle_first_and_each_state_is_read_once_per_pass` and
  `only_a_command_code_of_the_daemon_leaves_the_host_state_unknown`.
- **Reserved finding prefix:** a reviewer whose `finding_prefix` is `P` is refused
  when the policy is parsed, with the message `reviewer <id>: finding_prefix P is
  reserved for republished findings`. `P` numbers the prior findings published
  again (`republishedPrefix`, `prior.go:11`). Go accepts it, so a reviewer's `P002`
  can collide with a republished `P001` (`protocol.go:363`). A policy that Go
  accepts with `P` needs another letter before it runs on Rust. The regression
  test is `reserved_finding_prefix_is_rejected_naming_the_reviewer`.
- **Codex checkout line (#252):** the Codex prompt carries the trusted line
  `- Trusted checkout: base` or `head`, from the pipeline's `checkout` (any
  value but `head` is `base`, as the session's worktree), and a base checkout
  adds how to read the change through git. Go's prompt has neither
  (`protocol.go:76-80`). The adapter checks a prompt without the line as `head`,
  so on a rollback to Go a Codex pipeline with `checkout: head` keeps working and
  one with `checkout: base` fails in the adapter. The regression tests are
  `test_base_checkout_at_the_base_branch_tip_is_reviewed_and_left_unchanged`
  (adapter) and `codex_review_of_a_fork_pull_request_runs_on_the_base_checkout`.
- **Claude checkout check (#253):** the Claude reviewer and verifier prompts
  carry the trusted lines `- Trusted head SHA:`, `- Trusted base SHA:` and
  `- Trusted checkout: base` or `head`, and a base checkout adds how to read the
  change through git. Go's prompts have none of them (`protocol.go`,
  `verify.go`). The Claude adapter checks a prompt without the checkout line
  as `head`: `HEAD` must be the SHA of its `Head SHA:` line. On a rollback to
  Go a Claude pipeline with `checkout: head` keeps working, and one with
  `checkout: base` fails in the adapter unless the older Claude wrapper is put
  back. Either adapter refuses a base checkout whose head commit is not in the
  repository; the engine reads that refusal as `review_head_unavailable`, which
  neither the reviewer run nor the scheduler retries. Go reads it as an
  unusable output. Every other Claude result line names the checkout the
  adapter checked (`checked_checkout`), and Rust refuses a Claude reviewer or
  verifier result without it, or with another checkout than the pipeline's, as
  `review_adapter_outdated`, so an older Claude wrapper stops every Claude
  review after the switch. Go ignores the field. In `protocol_matches_go_golden`
  each Claude golden case gets `"checked_checkout":"base"` before it is parsed.
  The regression tests are
  `test_base_checkout_without_the_head_is_refused_before_claude_runs` (adapter),
  `adapter_refusal_of_a_missing_head_is_not_retried` (Claude and Codex parsers)
  and `base_checkout_review_of_a_missing_head_stops_without_publishing`.
- **History check of Codex pipelines (#237):** the linear history check of an
  incremental review runs for every protocol, so a `codex-envelope` re-review
  whose head does not extend the published head linearly, or whose history
  cannot be read, is full (`history_not_linear`, `history_unknown`). Go returns
  before the check for any other protocol (`discovery.go:90`), so a Codex rule
  with `on_clean: approve` can approve a rebased head on an incremental review.
  No stored row changes, and a rollback to Go brings the Go behaviour back. The
  regression test is `rewritten_history_gets_a_full_review_on_a_codex_pipeline`.
- **Retargeted pull requests (#240):** the published record stores the base
  branch of the reviewed diff (`base_branch`, omitted when empty), and a review
  whose pull request now targets another base branch is full
  (`base_branch_changed`). A record without the field, written by Go or before
  #240, has an unknown base branch and does not widen. Go never compares the
  base (`reviewScope`, `discovery.go:72-80`), so an `on_clean: approve` rule
  can approve commits that joined the diff only through a retarget. Go ignores
  the field when it reads the record and drops it when it rewrites the
  `published` entry, so after a return to Rust such a record does not widen
  until the next publication. The regression test is
  `retargeted_pull_request_gets_a_full_review_before_it_is_approved`.
  A running review whose pull request is retargeted publishes nothing and ends
  superseded with `base_branch_changed` (#279); Go's `superseded` list
  (`schedule/scheduler.go:184`) lacks the code, so after a rollback such a row
  counts as a failure that is not retried. A retarget without a push schedules
  a full review of the same head (#280); Go leaves that head completed. The
  regression tests are
  `retarget_during_a_review_publishes_nothing_and_the_next_review_is_full` and
  `retarget_without_a_push_gets_a_full_review_of_the_same_head`.
  A schedule record keeps the failed run it notified last as `handled_workflow`
  and the retry count before that failure as `handled_retries` (#313), both
  omitted when empty or zero. A retarget keeps them, and so does the completed
  record of a return to the published base branch, so a return to the base
  branch of that run handles the failure again without a notice; a manual
  retry keeps `handled_workflow` and restarts `handled_retries` at 0. Go
  ignores both fields and drops them when it rewrites the `schedule` entry; Go
  has no retarget review, and after a return to Rust such a record notifies
  that failure once more. The regression tests are
  `retarget_is_reviewed_once_per_base_branch_and_ended_runs_never_block` and
  `stopped_review_of_a_retarget_is_retried_by_head_prefix`.
- **Codex results like Claude results (#23):** a `codex-envelope` report must
  carry `prior_findings` and the `blocking` flag of every finding, as the
  adapter in `adapters/codex/` writes them. A report without them is refused
  with `review_adapter_outdated`, which is not retried. A Codex finding blocks
  only when it is flagged blocking at `blocker`, `critical` or `high`. A Codex
  pipeline gets the prior findings and the stored-result check
  (`previous_review_not_stored`) of a Claude pipeline; its prompt carries the
  prior findings, the discussion and the note limit, and its verdicts pass
  `validate_prior_verdicts`. Go refuses both fields (`DisallowUnknownFields`,
  `protocol.go:246`), blocks every Codex finding and gives a Codex pipeline no
  prior findings (`discovery.go:90`, `review.go:94`). In
  `protocol_matches_go_golden` the golden case `codex_valid_report`, a report
  without both fields, expects `review_adapter_outdated`. A rollback to Go
  needs the older Codex wrapper back, and Go refuses a stored Codex input with
  prior findings, so a Codex review in flight at the rollback stops. The
  regression tests are `codex_finding_blocks_only_when_it_says_so`,
  `codex_report_without_blocking_or_prior_findings_is_refused`,
  `codex_verdicts_settle_the_prior_findings` and
  `open_prior_finding_carries_over_until_it_is_fixed_on_a_codex_pipeline`.
- **Review host outage (#241):** when the route of a due review fails because
  a host failed its project list (a `pohunek_*` host failure code) and no other
  host has the repository, the review waits in `retry_wait` at its due time with
  its attempt and `retries` unchanged, and the schedule record gets
  `awaits_host: true`. One `review_stopped` notice per host outage says that
  reviews wait for a review host. The announced hosts are stored in the
  `rejected` discovery entry under the keys `host:<host>` with the host error
  code, and a host's key is removed on the first pass in which that host
  answers. Go fails the route through `AfterFailure` (`discovery.go:305`), uses
  the retry budget and stops the review. Go reads the record and ignores
  `awaits_host`, and its `reportRejected` (`discovery.go:57-61`) drops the
  `host:` keys on its first pass, so a rollback brings the Go behaviour back and
  a return to Rust announces a lasting outage again. Any other route error
  (`project_route_not_found`, `project_route_ambiguous`) still fails as in Go.
  The regression test is `review_waits_through_an_outage_of_every_review_host`.
  A host with a `host:` key that no due review asked in a pass is asked once at
  the end of the pass for its project list, so its answer removes the key
  (#247), and the review queue lists a record with `awaits_host` as
  `AI review waits for a review host: <code>`, where Go lists `AI review queued`.
  The regression test is
  `host_recovery_without_a_due_review_ends_the_announced_outage`.

- **MCP server (#179):** `autoreview mcp` has no Go counterpart. The `mcp`
  section exists only in `config.yaml`, which Go does not read, and the server
  writes the store only through the engine functions of the CLI commands
  (`approve_head`, `retry`), so every row stays readable by Go after a rollback.

- **Lost session runtime (#213):** a session whose runtime pohunek reports as
  lost (`runtime.state` `lost`, or state `failed` with a `loss_reason`) fails its
  start with `pohunek_session_lost`, not permanent and retried by the
  scheduler. Its log `diagnostic` is pohunek's `loss_cause`, else its
  `loss_reason`, when it matches `^[a-z][a-z0-9_]{0,63}$`. When the removal of
  that session fails too, the code stays `pohunek_session_lost`; the orphan pass
  of the start removes it again and raises `cleanup_manual` if it stays. Go
  reports `session_identity_invalid` (permanent, `pohunek.go:178-179`) or, after
  a failed removal, `pohunek_session_cleanup_failed` (`pohunek.go:209`). Go never
  writes the code; a row with it stays readable after a rollback, but Go's
  `Retryable` (`schedule/scheduler.go:173`) and explanation do not know it. The
  explanations of
  `pohunek_base_fetch_timeout` and `pohunek_base_fetch_authentication_failed`
  name the SSH agent; Go's do not (`schedule/diagnostic.go:65-68`). The
  regression test is `lost_runtime_at_start_is_a_retryable_session_loss`.
- **Authors section (#32, #33, #34):** every repository rule must carry an
  `authors` section (`authors is required`). Discovery resolves the author class
  of each candidate, reviews an untrusted author under the untrusted section's
  rule (base checkout, never an automatic approval, no notes hand-off) or skips
  them, and stores that rule and `author_class` with the review. A login matches
  by author type (`User` for a plain login, `Bot` for `name[bot]`, from the
  GraphQL `__typename` the pull request read already fetches); a missing or other
  type is untrusted. Go has no author classes, so nothing is compared with Go.
  The stored rule is the protection; `validate_input` and `plan_publication`
  check the class as well, and a Go engine that ignores `author_class` still runs
  the stored rule.
  Go refuses the key
  (`field authors not found`, `policy.go:311`), so a policy that runs on Rust does
  not load on Go. A rollback to a binary built before #33 needs a policy whose
  rules all say `trusted: any`, and one built before #32 needs no `authors`.
  `trusted: any` adds nothing to the digest, so the Go digest goldens
  hold. `testdata/review-policy.yaml` stays without the section for the Go tests;
  the Rust tests add `trusted: any` to its rules before parsing, and the golden
  case `fixture_via_load` loads such a copy from a temporary directory.
  Each pass stores the pull requests it skips in the `rejected` discovery entry
  under the keys `skipped:<repository#number>` with the reason `untrusted_author`,
  so `review` and the review queue report them (#226); Go's `reportRejected`
  (`discovery.go:45-63`) drops those keys on its first pass, and a return to Rust
  stores them again on its next pass.
- **Enterprise Managed User logins (#227):** the pull request decoder and
  `authors.trusted.logins` accept a managed user's login, `handle_shortcode`
  (`autoreview_policy::login_valid`): groups of letters and digits joined by
  single hyphens, one `_`, then letters and digits, 39 bytes at most. An app's
  `name[bot]` and every other login keep Go's grammar. Go's `authorPattern`
  (`cmd/autoreview/github.go:40`) refuses the underscore, so Go fails such a
  pull request as `github_response_invalid` (`github.go:174`, `github.go:228`).
  No stored row changes, and a rollback to Go brings the Go behaviour back. The
  regression tests are `author_logins_follow_the_github_login_grammar` and
  `trusted_logins_follow_the_github_login_grammar`.
- **Served Enterprise Managed User login (#276):** `github.credentials.login`,
  `init --login`, the review input's `publisher_login` and the permission read
  check `autoreview_policy::user_login_valid`, the user branch of the #227
  grammar: a plain login or `handle_shortcode`; an app's `name[bot]` is refused.
  Go's `githubLoginPattern` (`cmd/autoreview/config.go:146`) refuses the
  underscore and a trailing hyphen, which the plain login of the policy accepts
  (`review-bot-`). Go checks it at startup (`invalid GitHub configuration`,
  `config.go:287`) and in `validateInput` (`review.go:75`), so a rollback to Go
  with such a login does not start. No stored row changes. The regression tests
  are `missing_or_unsafe_configuration_fails_fast`,
  `validate_input_rejects_inconsistent_input`,
  `example_passes_the_loader_and_the_engine_validation`,
  `init_login_follows_the_user_login_grammar` and
  `permission_is_read_from_the_role_name`.
- **Repositories of Enterprise Managed Users (#294):** `repository_valid`
  (`crates/engine/src/model/ids.rs`) and the policy's rule `match` check the
  owner with `autoreview_policy::user_login_valid`, so a managed user's
  `handle_shortcode/name` is accepted; every other owner keeps Go's grammar.
  Go's `repositoryPattern` (`cmd/autoreview/config.go:147`) and `ownerPattern`
  (`policy/policy.go:213`, applied at `policy.go:639`) refuse the underscore.
  On a rollback to Go a policy with such a `match` does not load, such a
  `pohunek.route_overrides` key stops the start (`config.go:304`), and a stored
  review of such a repository fails `reviewIdentityValid` (`model.go:539`,
  called from `review.go:40` and `discovery.go:540`). No stored row changes
  shape. `testdata/parity/ids.json` gets no case: Go writes it, and
  `TestWriteParityGolden` compares it byte for byte. The regression tests are
  `repository_owner_follows_the_github_login_grammar` and
  `rule_match_owner_follows_the_github_login_grammar`.
- **Step runs table (#270):** the store holds the additive table
  `review_step_runs` (`crates/engine/src/store/steps.rs`), which Go's schema
  lacks. Every open creates it with `CREATE TABLE IF NOT EXISTS` after the Go
  schema, and `user_version` stays 1, so the Go store still opens the file. Go
  neither writes nor reads the table; its prune deletes the rows with their
  review through `ON DELETE CASCADE`, and reviews that Go runs after a rollback
  count in no step. `go_store_golden_opens` leaves the table out of the Go golden
  comparison and checks its SQL on its own; the golden is not regenerated. The
  regression tests are `go_store_golden_opens`,
  `review_records_its_steps_with_their_retries`,
  `shutdown_after_the_last_reviewer_result_still_records_the_reviewers_step` and
  `verifier_failure_before_its_verification_is_stored_records_the_resumed_run`.
- **Review reuse across base branches (#297):** the review body carries
  `<!-- hermes-pr-review-base:v1:<hex SHA-256 of the base branch name> -->`
  right after the legacy marker. The Rust lookup (`review_page_step`,
  `crates/external/src/github/reviews.rs`) searches for the whole stored marker
  text, which includes that comment, so it does not reuse a review of the same
  head for another base branch, and it does not match a review that carries the
  legacy marker alone (published by Go or by a binary built before #297): it
  posts a new review where Go reuses that one. It skips a review GitHub
  dismissed, reuses a later match, and otherwise posts the review again; Go
  reuses a dismissed review (`github.go:781`). A review rendered by a binary
  built before #297 carries the legacy marker alone. On a rollback Go finds
  Rust's reviews by the legacy marker as a substring, so it reuses a review
  made for another base branch and a dismissed one. The regression tests are
  `retarget_without_a_push_publishes_a_new_approval` and
  `dismissed_review_is_skipped_and_a_later_match_is_reused`.

Intentional parsing divergences (stricter than Go; each gets a golden case in G1
with the expected Rust error code):

- JSON object keys match exactly; Go's `encoding/json` matches
  case-insensitively. Rust ignores a case-variant key such as `"TITLE"` as
  unknown, so the field keeps its default where Go sets it, and Rust accepts a
  row whose case-variant key holds a value that Go refuses, such as
  `"NUMBER": -9223372036854775809`. Neither engine writes such a key.
- A duplicate field in a JSON object read as a struct is an error; Go keeps the
  last value.
- A top-level JSON `null` where an object is expected is an error; Go yields the
  zero value.
- In stored state, a JSON `null` for a scalar, enum or struct field, for a map
  value or for a list entry is an error. In a GitHub or pohunek response, a
  `null` field that Go decodes into a non-pointer type and a `null` list entry
  read as the zero value, as in Go, through `null_default` and `null_entries`. A
  lone surrogate escape such as `\ud800` and `-0` in an integer field are errors
  everywhere. Go reads the zero value for the `null`, U+FFFD for the lone
  surrogate and `0` for `-0`. In a search result either one fails the whole
  search. For a `-0` pull request number Go rejects only that pull request; for
  a lone surrogate Go keeps the pull request with U+FFFD in place of the escape.
- A persisted review input whose policy string enum holds an unknown value
  (protocol, checkout, trigger kind, on_findings, on_clean, on_notes,
  notification level or event) is refused when the row is read, with the code
  `store_state_invalid`. Go's read of the same row succeeds
  (`cmd/autoreview/store.go:313`) and Go rejects some of these values later, at
  run time (`cmd/autoreview/review.go:38-86`), so this is stricter than Go; Go
  writes only values that passed policy validation, so no Go-written row holds
  one.
- The AI signature symbols in `config/symbols.rs` are Unicode 17.0.0 `So`, as
  in Go 1.27. Go 1.26, which `go.mod` names, has Unicode 15.0.0, a subset, and
  reads a signature that leads with one of the 834 newer symbols as unsigned,
  so that comment can settle a finding without the user.
- Logins, repository names and pull request URLs compare and lowercase ASCII
  only. Go's `strings.EqualFold` and `strings.ToLower` also fold non-ASCII
  letters, such as the Kelvin sign with `k`, so Rust can miss a match that Go
  finds: a finding stays open, or a pull request is not matched. GitHub logins
  are ASCII. Publish reconciliation matches review authors as Go does,
  including the Kelvin sign and the long s, so it never posts a second review.
- The store opens its database path as a plain file name. Go's SQLite driver
  reads the path as a `file:` URI, so a `?` or `%` in the path selects another
  file than the name says. Rust opens the name as written.
- JSON nested deeper than 128 levels is refused (the `serde_json` limit); Go's
  decoder accepts far deeper nesting. Neither `gh` nor either engine writes it.
- A `gh` call that prints `HTTP 422` and then runs past its timeout fails as
  `github_request_failed` (retryable), because the process runner keeps no
  stderr on a timeout. Go reads that stderr and fails the call as
  `github_request_rejected` (permanent).
- The GraphQL node types of the discussion read refuse an unknown key, a
  case-variant key included, because the query names every field it reads.
  Go ignores unknown keys and matches keys case-insensitively. The REST
  comparison and the response envelopes keep the R4 rule.
- The pohunek CLI output is read fail-closed: a key that differs from a known
  field only in ASCII case is refused, where Go matches it case-insensitively.
  Duplicate fields, lone surrogates and out-of-range numbers are refused as in
  the other decoders; a `null` field reads as the zero value, as in Go.
- The GitHub review event (`Decision`) and the review outcome (`Outcome`) are
  enums. Rust refuses a stored value outside their listed values with a serde
  error; Go reads any string. Neither engine writes another value. The store
  slice maps the refusal to `store_state_invalid`.
- A persisted or GitHub API time whose instant falls outside the years -9999
  to 9999 after its offset is applied, such as `9999-12-31T23:59:59-01:00`, is
  an error; Go reads it, but Go's JSON encoder refuses to write a year above
  9999.
- The policy file is read with `serde_saphyr`, which refuses three inputs that
  Go's YAML decoder accepts: a null mapping key at any level (Go drops the
  entry), a leading-zero integer such as `030` (Go reads it as YAML 1.1 octal)
  and a null list entry (Go drops the entry). Three differences are in the
  text only: quoted values in policy errors use Rust's `{:?}`, which escapes
  control and non-ASCII characters unlike Go's `%q`; a null mapping value and
  a document that is only `---` or `~` fail as invalid YAML instead of Go's
  later validation error; and of two YAML errors in one file, Rust may report
  the other one first. A float in an integer field (Go truncates `24.5` to 24)
  is refused; an empty or comment-only file fails the version check instead of
  Go's `EOF`; non-UTF-8 input reports the Rust decoding error. Every case stays
  `configuration_invalid` at the caller (`cmd/autoreview/config.go:195-196`).
- A JSON object in the prose result of a Claude session that does not read as
  the structured artifact is refused with `claude_result_mismatch`. Go skips the
  comparison when its typed decode fails, for example on `{"findings": 5}`, and
  accepts the output. Rust refuses it, because the prose is model output and an
  unchecked prose object is not safe to accept.
- Two keys of one JSON object that differ only in ASCII case are refused in
  model output, in the wrapper (`review_wrapper_invalid`), the outer message
  (`claude_result_invalid`), the prose result (`claude_result_mismatch`) and the
  Codex report (`review_result_invalid`). Go matches keys case-insensitively and
  keeps the last value, so such a pair decides the value by position. The check
  applies to every object of the decoded text, known or unknown fields, which is
  stricter than Go for unknown fields.

Intentional notification divergences (stricter than Go; checked by a
differential run against the Go reference, not by a golden case):

- A `matrix_max_characters` below 1 is refused with `matrix_request_invalid`
  when a message is not blank. Go panics on that input
  (`cmd/autoreview/notify.go:50-51`). Until #5 checks this bound at startup,
  both engines accept such a limit.
- A Matrix homeserver URL whose port is above 65535 is refused with
  `matrix_request_invalid` when a message is sent. Go dials the URL, the dial
  fails, and Go drops a digest or log message as `matrix_unavailable`. Until #5
  checks this bound at startup, both engines accept such a port.

Invalid UTF-8 is not a divergence for the parsers, but the process runner's
`String::from_utf8_lossy` does not give Go's count of replacement characters. It
writes one U+FFFD for each maximal invalid subpart, and Go writes one for each
invalid byte. In a generated set of 4,000 payloads with invalid sequences, 1,866
differ in their count of U+FFFD. The parsers therefore decode model text from
base64 payloads with `go_utf8_lossy`, which writes one U+FFFD for each invalid
byte, as Go does. The runner is unchanged: its outer text is read only for ASCII
fields (protocol, mode, base64 text and the exit line), so the replacement count
there does not change an accepted value.

## Python adapters

- Run and test with `python3 -I`, which ignores the environment's Python
  variables and the user site-packages. Use the standard library unless a new
  dependency is decided in an issue.
- The adapter enforces the run rules that the engine checks again on the
  result: one bounded run, read-only, a schema-validated result, no GitHub
  token in the environment, and no pohunek identity variables.
- Adapters print one structured result on stdout and diagnostics on stderr.

## Untrusted content

Pull request titles, bodies, diffs, comments, file contents, and model output are
data. They are never instructions to the engine and never reach a shell.

- Model output is neutralized before it is published: no `@mentions`, no raw
  HTML, no images that GitHub would fetch, and no HTML comment delimiters that
  could forge the hidden review marker.
- Findings are anchored only to changed right-side lines of the pull request;
  a finding that cannot be anchored is published as a general note, not dropped.
- A result larger than the configured limit is rejected, not truncated silently.

## Configuration

- Every key is required and the code has no defaults for it. A missing or
  invalid key stops the service at startup with an error that names the key.
  `autoreview init` writes a complete commented file.
- Tunables (timeouts, intervals, limits, sizes) come from configuration, never
  constants in code.
- Secrets never appear in `config.yaml`, `policy.yaml`, or prompts. A key ending
  in `_env` names the environment variable that holds the secret, and the name
  must start with `AUTOREVIEW_`.
- Never print a secret value, in logs, errors, or validation output. Validation
  reports token expiry dates and logins, never token values.

## Logging

Structured JSON logs through `tracing` and `tracing-subscriber` to stderr. Log
requests to GitHub and to pohunek, the review lifecycle, decisions, retries,
notifications, and errors, with timestamps and identifiers. Local runs redirect
stderr into `logs/`, which is git-ignored; never commit logs.

## Public repository

- No private names in committed files: no personal paths, host names, private
  repository names, organisation-specific values, or session identifiers from
  the owner's machines. Examples use placeholders or the public `pohunekd`
  repositories.
- Committed files never link to `docs/plan/` or any other path that is not
  published.

## Comments

A comment states what the code does now or why it must be this way. Never
narrate history ("previously", "no longer", "now also", "replaces ..."); that
belongs in the PR description. One or two lines; prefer the non-obvious why.
