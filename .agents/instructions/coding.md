# Coding rules

## Layout and boundaries

- The engine is a Cargo workspace under `crates/`: `go-compat`, `policy`,
  `schedule`, `engine`, `external` and the `autoreview` binary. The `engine`
  crate reaches side effects (processes, HTTP) only through the `Integrations`
  trait; its `clippy.toml` disallows `std::process::Command`,
  `tokio::process::Command` and `reqwest::Client`. The imported reference
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
  `128-143`); a newer version is refused with `store_version_unsupported`. Open
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
  unchanged. #7 changes the marker after cutover and reads both forms.
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
  is not a documented contract.
- **CLI wording and usage text** (`main.go:16`); the JSON error object keeps its
  keys and codes (`command.go:22-47`).
- **In-flight Go reviews** (statuses `queued`, `running`, `awaiting` at
  cutover): not supported.

Intentional parsing divergences (stricter than Go; each gets a golden case in G1
with the expected Rust error code):

- JSON object keys match exactly; Go's `encoding/json` matches
  case-insensitively. Rust ignores a case-variant key such as `"TITLE"` as
  unknown, so the field keeps its default where Go sets it, and Rust accepts a
  row whose case-variant key holds a value that Go refuses, such as
  `"NUMBER": -9223372036854775809`. Neither engine writes such a key.
- A duplicate key in a reviewer or verifier result is an error; Go keeps the
  last value.
- A top-level JSON `null` where an object is expected is an error; Go yields the
  zero value.
- A JSON `null` for a scalar, enum or struct field is an error, and so is a
  lone surrogate escape such as `\ud800` and `-0` in an integer field. Go reads
  the zero value for the `null`, U+FFFD for the lone surrogate and `0` for `-0`.
- A persisted review input whose policy string enum holds an unknown value
  (protocol, checkout, trigger kind, on_findings, on_clean, on_notes,
  notification level or event) is refused when the row is read, with the code
  `store_state_invalid`. Go's read of the same row succeeds
  (`cmd/autoreview/store.go:313`) and Go rejects some of these values later, at
  run time (`cmd/autoreview/review.go:38-86`), so this is stricter than Go; Go
  writes only values that passed policy validation, so no Go-written row holds
  one.
- The GitHub review event (`Decision`) and the review outcome (`Outcome`) are
  enums. Rust refuses a stored value outside their listed values with a serde
  error; Go reads any string. Neither engine writes another value. The store
  slice maps the refusal to `store_state_invalid`.
- A persisted time whose instant falls outside the years -9999 to 9999 after
  its offset is applied, such as `9999-12-31T23:59:59-01:00`, is an error; Go
  reads it, but Go's JSON encoder refuses to write a year above 9999.
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

Invalid UTF-8 is not a divergence: the process runner decodes child stdout with
`String::from_utf8_lossy` before parsing, which matches Go's replacement with
U+FFFD.

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
  must start with `AUTOREVIEW_`. Secrets can also come from an owner-only
  `--env-file`.
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
