---
type: Concept
id: concept/reviewers-and-adapters
title: Reviewers and adapters
description: What a reviewer is, the two protocols (claude-structured and codex-envelope), what an adapter enforces, the Claude and Codex adapters, the pohunek agent profiles they run and how `autoreview profiles install` writes both, finding prefixes, mixing models, and how a new agent would be added.
source_kind: manual
intents: [setup, help]
---

# Reviewers and adapters

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0.

A **reviewer** is one coding agent run in its own pohunek session. The agent
reads the pull request in a worktree, writes findings, and returns them in a
format the engine can check. It never writes to GitHub and never changes the
repository. The policy names reviewers in `reviewers` and groups them in
`pipelines`; see [policy](../../policy.md#reviewers).

## Protocols

A protocol is the contract between the engine and one kind of agent run. Each
reviewer declares exactly one protocol.

| Protocol | Agent run | Result the engine reads | Used for |
|---|---|---|---|
| `claude-structured` | A Claude Code run with a JSON schema that the runtime enforces | The schema-enforced structured output of the run | Reviewers and the verifier |
| `codex-envelope` | A Codex run that prints a result envelope | A base64 result frame followed by a line that reports exit status 0 | Reviewers only |

Both protocols return a summary, findings, the tests the reviewer ran, the
blocking flag of each finding and one verdict per prior finding (see
[the Codex adapter](#the-codex-adapter) for the Codex report).

With `claude-structured` the runtime itself refuses output that does not match
the schema, and the engine also checks the output against the request. The
schema bounds the summary by `limits.min_summary_characters` and
`limits.max_summary_characters`, so the runtime asks the reviewer again within
the run for a summary outside them. The free-text answer of the run may be
prose. When it is JSON it has to agree with the structured output, or the run
is rejected. A result line without the checkout that the adapter checked, or
with another checkout than the request names, comes from an older adapter and
is refused with `review_adapter_outdated`, without a fresh session.

With `codex-envelope` the engine looks for one result frame at the end of the
output and decodes it strictly. The frame must not be larger than
`limits.result_max_bytes`, must contain only the known fields, and must name the
reviewed head SHA. A report without `prior_findings`, or with a finding without
`blocking`, comes from an older adapter and is refused with
`review_adapter_outdated`, without a fresh session.

## What the adapter enforces

Each protocol has an adapter: the wrapper that starts the run and turns its
output into the result the engine reads. The adapter enforces these rules, and
the engine checks them again on the result:

- **One bounded run.** One non-interactive run per reviewer session, limited by
  `session.timeout_seconds`. A run that does not finish in time gives no usable
  result.
- **Read-only.** The run is set up so that the agent reads the repository and
  does not change it or the pull request. The generic prompt also tells the
  agent that it is a read-only reviewer.
- **Schema-validated result.** A result that does not match the schema is
  unusable. Whether it gets a fresh session, up to
  `session.reviewer_run_max_attempts`, depends on its code: a Codex result
  that cannot be read (`review_result_invalid`) does, a Claude Code result
  message that does not decode (`claude_result_invalid`) ends the review at
  once (see [troubleshooting](../runbooks/troubleshooting.md#reviewer-output)).
- **No GitHub token.** The run receives no GitHub token. Only the engine's own `gh`
  calls receive one. See [secrets](../safety/secrets.md).
- **No pohunek identity.** The variables that bind a call to a pohunek session
  are removed from the environment of the run.

The output is also bounded. A result larger than `limits.result_max_bytes` is
rejected, and each finding and summary is bounded by the `limits` keys.

## Agent profiles

A pohunek **agent profile** is the named way a session starts an agent on a
host. autoreview does not replace the agent runtime. It creates one profile for
each reviewer, on top of the official pohunek runtime package for the agent:
Claude Code for `claude-structured`, Codex for `codex-envelope`. The runtime
package must already be installed on the review host.

`autoreview validate` prints the profiles the policy needs.
[`autoreview profiles install`](../../cli.md#autoreview-profiles-install) writes
one of them, with the adapter it starts, for the pohunek daemon of the machine it
runs on. The adapters are embedded in the autoreview binary, so the release
attestation of the archive covers them and an upgrade brings the new adapters:
run the command again with the same flags after each upgrade. The profile it
writes is the profile that `render_profile.py` (below) writes from a base profile
with the same package pin and no `[env]` entries; a test of each adapter and a
test of autoreview compare both with the same golden file under
`adapters/<agent>/tests/`. The command marks the profile as its own and never
overwrites a profile without that mark unless `--replace` is given; pohunek may
repin the package of a marked profile without making it foreign.

What keeps a profile written by the command read-only, beyond the adapter rules
below:

- A Claude profile passes `--permission-mode plan`, never `default`. Claude
  Code keeps plan mode's block on the edit tools in print mode, and only an
  approved plan lifts it, which a print-mode run cannot give. Its built-in
  read-only commands, read-only `git` among them, run without approval in every
  mode. Any other shell command runs only when an allow rule in the Claude user
  settings of the daemon user approves it, or, where Claude Code's auto mode is
  available, when the auto-mode classifier approves it. Keep the allow rules to
  read-only commands, and set `useAutoModeDuringPlan` to `false` in those user
  settings, so that every other command is denied (#243).
- Its `[env]` table is empty, so no variable is copied from another profile.
  The agent runs with the daemon's environment (pohunek's allowlist of session
  variables) and its default config home (`~/.claude`, `~/.codex`); a profile
  for a second subscription or with telemetry settings is written with
  `render_profile.py` from a base profile instead. Of the base profile's
  `[env]`, `render_profile.py` copies only the telemetry keys (names that start
  with `OTEL_`, and `CLAUDE_CODE_ENABLE_TELEMETRY` for Claude) and the config
  home of the runtime (`CLAUDE_CONFIG_DIR` for Claude, `CODEX_HOME` for Codex).
  It drops every other key and prints the dropped names, never their values, on
  stderr. With `CLAUDE_CONFIG_DIR`, the Claude user settings of the previous
  bullet (allow rules, `useAutoModeDuringPlan`) are the ones in that directory.

### The Claude adapter

The adapter for `claude-structured` lives in `adapters/claude/` and uses only
the Python standard library (Python 3.11 or newer). It has two parts:

- `claude-adapter` is the program a profile starts. It runs under `python3 -I`
  (its first line is `#!/usr/bin/env -S python3 -I`). It reads one request that
  pohunek writes into the session, runs Claude Code once in print mode with
  `--setting-sources user`, `--no-session-persistence` and, for a structured
  request, `--json-schema`, and prints one result line that carries the exit
  code and the output of the run, and the field `checked_checkout` with the
  checkout it checked (`base` or `head`). The Claude Code run gets the session
  environment without the pohunek session identity variables, the GitHub token
  variables (`GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`,
  `GITHUB_ENTERPRISE_TOKEN`), `MATRIX_ACCESS_TOKEN` and every `AUTOREVIEW_`
  variable. The adapter then stays alive so that the coordinator can read the
  result before it removes the session. The engine refuses a result line
  without a `refusal` whose exit code is not 0 as `review_wrapper_invalid` and
  logs the end of its stderr with it, when it has one (see
  [troubleshooting](../runbooks/troubleshooting.md#claude-code-failed)). A line
  without a `refusal` whose protocol is `claude-otel-noninteractive/v2` and
  whose `checked_checkout` is not the pipeline's checkout is
  `review_adapter_outdated` instead, whatever its exit code.
  Before and after the run it checks the review checkout against the trusted
  lines of the request (`- Trusted head SHA:`, `- Trusted base SHA:`,
  `- Trusted checkout:`), as the [Codex adapter](#the-codex-adapter) does, with
  local read-only git calls only. A `base` checkout gets the Codex base checks:
  clean work tree, both trusted commits present, `HEAD` neither the head nor a
  commit that contains it, the trusted base `HEAD` or an ancestor of it, and the
  same `HEAD` after the run. A `head` checkout needs `HEAD` to be the trusted
  head. A request without the checkout line is
  checked as `head`, with the head SHA of its `Head SHA:` line. A refused
  checkout prints only one result line, with exit code 64, no Claude Code
  output, the reason in `stderr_base64` and the field `refusal`:
  `head_unavailable` when the head commit is not in the repository (the engine
  stops the review with `review_head_unavailable`, without a fresh session), or
  `checkout_invalid` (the engine reads `review_result_invalid`, logs the
  message as its reason and starts a fresh session). A refusal before the run
  starts no Claude Code; a refusal after it drops Claude Code's output. See
  [troubleshooting](../runbooks/troubleshooting.md#head-commit-unavailable).
- `render_profile.py` writes a profile from a base Claude profile. It copies the
  runtime package pin, the arguments and the allowed environment keys (see
  [agent profiles](#agent-profiles)) of the base profile,
  refuses an argument that the adapter controls (`--print`, `--output-format`,
  `--settings`, `--setting-sources`, `--json-schema`, `--resume`, `--continue`
  and their short forms), and sets the model and the effort level of the new
  profile. The profile also turns off resuming and disables prompt-box
  detection, because the adapter reads its input without rendering one.

The empty detection manifest (`rules = []`) is deliberate. The Claude runtime
declares pohunek's `screen_idle` input readiness, which waits for an idle rule
on a screen region. An override manifest without one makes pohunek fall back to
`bracketed_paste` readiness: the request is written once the adapter's terminal
accepts a paste, which is what the adapter needs. `pohunek doctor` therefore
lists an installed Claude profile as a warning (check `agent_profile_<name>`),
and `pohunek service check` and the upgrade preflight list it under
`profile_warnings` with the code `profile_without_screen_readiness`. The
warning is expected and needs no change. Do not add the `prompt_box_body` idle
rule that pohunek suggests: the adapter draws no prompt box, so the rule never
matches and every session start would wait for input that is never written.

Read-only is enforced by the renderer as an allowlist for the permission mode
and a refusal list for everything that widens access. The base profile must set
`--permission-mode` exactly once, to `plan` or `default`; any other value
(`acceptEdits`, `auto`, `dontAsk`, a bypass mode) and a missing mode are
refused. It must not carry any of `--dangerously-*`, `--allow-dangerously-*`,
`--add-dir`, `--agents`, `--allowedTools` and `--allowed-tools`,
`--disallowedTools` and `--disallowed-tools`, `--tools`, `--mcp-config`,
`--strict-mcp-config`, `--plugin-dir`, `--plugin-url`, `--permission-prompt-tool`
and `--permission-prompts`, with or without `=value`.

Configuration comes from the profile and a file next to the program, never from
a default in the code:

| Setting | Where | Meaning |
|---|---|---|
| Claude Code executable | `--claude-binary` of `render_profile.py`: written to the file `<wrapper>.claude-binary` next to the resolved wrapper, and passed as the first program argument | Absolute path. The file must be a regular file owned by the current user and not writable by the group or others. A missing or invalid file, or a first argument that differs from the file, stops the run with a message. |
| Input limit | `--maximum-input-bytes` | The largest request, and the largest schema, that the adapter accepts. |
| Model and effort | `--model`, `--effort` | `--effort` is one of `low`, `medium`, `high`, `xhigh`, `max`. |
| Detection manifest | `--manifest-name` | Name of the manifest that `render_profile.py` writes next to the profile. |
| Telemetry and config home | `env` table of the base profile | Optional. Only `OTEL_*`, `CLAUDE_CODE_ENABLE_TELEMETRY` and `CLAUDE_CONFIG_DIR` are copied; every other key is dropped and its name printed on stderr. Without them the run has no telemetry and uses `~/.claude`. |

The base profile must use `base = "claude"`. A bare `--version` call, which
pohunek uses to detect the runtime, gets no profile arguments and no profile
environment, only the launch `PATH`. The adapter therefore finds the Claude Code
executable through the file next to itself, never through the home directory,
the working directory or a `PATH` lookup, and prints the version line of that
executable. It runs the executable with the environment of a review run,
without the variables listed above.

The tests of the adapter are in `adapters/claude/tests` and run with
`scripts/gates adapters`. They use fake executables only. The adapter keeps the
result protocol name `claude-otel-noninteractive/v2`, which the engine reads; the
rename is planned in #24.

### The Codex adapter

The adapter for `codex-envelope` lives in `adapters/codex/` and uses only the
Python standard library (Python 3.11 or newer). It has two parts:

- `codex-adapter` is the program a profile starts. It runs under `python3 -I`.
  Before it reads the request it sets the fixed terminal title
  `autoreview codex adapter` (OSC 0), never on a bare `--version`. The Codex
  runtime declares pohunek's `manifest_idle` input readiness: pohunek writes the
  request only after the Codex detection manifest reads idle from the screen
  or the terminal title, and any non-blank title that does not start with a
  spinner glyph reads idle (`osc_title_idle`). The adapter draws nothing else,
  so without the title pohunek refuses the start with
  `session_input_not_ready`. The readiness gates only that first write: a
  later title, from `codex exec` or anything else in the session, does not
  affect the run, because the engine sends no further input and does not read
  the session state, only the output.
  It reads one request that pohunek writes into the session and checks the
  review checkout before it starts Codex. The request names the checkout in
  the trusted line `- Trusted checkout: base` or `head`; a request without the
  line is checked as `head`, and a second line or another value stops the run.
  - `head`: `HEAD` must be the trusted head SHA of the request, the work tree
    must be clean and the trusted base commit must be present.
  - `base`: the work tree must be clean, the trusted base and head commits must
    be present, `HEAD` must be neither the trusted head nor a commit that
    contains it, and the trusted base must be `HEAD` or an ancestor of it. Pohunek
    starts the worktree at the fetched base branch tip, which is the trusted base
    or a descendant of it; after a force push of the base branch the run is
    refused.
    The adapter reads only the local repository: no network, no write (its git
    calls run with `--no-optional-locks`, so `git status` leaves the index
    alone). Pohunek fetches only the base branch, and the read-only Codex
    sandbox cannot fetch, so the head commit must already be in the repository
    (see
    [troubleshooting](../runbooks/troubleshooting.md#codex-checkout-refused)). The
    request tells Codex that the worktree is the base and to read the change
    with `git diff <base>...<head>` and `git show <head>:<path>`.

  It runs `codex exec` once with `--sandbox read-only`,
  `approval_policy="never"`, `--ignore-user-config`, `--ignore-rules`,
  `--ephemeral` and an output schema. It checks the report (status `completed`,
  the reviewed head, a blocking flag on every finding, well-formed prior
  findings, the result limit) and prints it as one frame,
  `[pohunek-codex-review-result-v1:<base64>]`, followed by the line
  `[pohunek-codex-review exited with status N]`. The Codex run gets the session
  environment without the variables that the Claude adapter drops (see above).
  The adapter then stays alive for the retention time, so that the coordinator
  can read the output before it removes the session. It exits with the status of
  Codex, or with status 64 when the request's trusted lines, the checkout check
  before or after the run, or the report fail. Codex's stderr goes on to the
  session output as it arrives; the adapter keeps its last 64 KiB. When Codex
  exits with a status other than 0, the line
  `[pohunek-codex-review-reason-v1:<base64>]` comes right before the exit line,
  with the reason that the engine logs next to `review_result_invalid` (see
  [troubleshooting](../runbooks/troubleshooting.md#codex-program-failed)). A
  refusal after the run prints the same line with its own message,
  `codex-adapter: <reason>`, before the head marker of a missing head.
  After the run the checkout gets
  the same check again, and a base checkout must still be at the `HEAD`
  recorded before the run.
  A refused checkout prints `codex-adapter: <reason>`, no result frame and the
  exit line with status 64, and the adapter stays alive for the retention time
  as after a run. The engine reads that output at once as
  `review_result_invalid` (see
  [troubleshooting](../runbooks/troubleshooting.md#codex-checkout-refused)).
  When the head commit is not in the repository, the line
  `[pohunek-codex-review-refused-v1:head_unavailable]` comes right before the
  exit line, and the engine stops the review with `review_head_unavailable`
  without a fresh session.
  An invalid argument, Codex path file or input frame stops the adapter before
  that: it prints the reason and exits with status 64 at once, without the exit
  line.
- `render_profile.py` writes a profile from a base Codex profile. It copies the
  runtime package pin and the allowed environment keys (see
  [agent profiles](#agent-profiles)) of the base profile, and turns off
  resuming.

The report has the fields `findings`, `summary`, `tests`, `status`,
`reviewed_head_sha` and `prior_findings`. A finding has `severity`,
`confidence`, `blocking`, `path`, `line`, `title` and `body`. A prior finding
verdict has `key`, `status` (`fixed`, `still_open` or `answered`), `note` and
`relies_on`, as in the Claude result. The engine reads both fields and refuses
a report without them (`review_adapter_outdated`).

Read-only is fixed in the adapter: the arguments above are not configurable, and
the adapter accepts exactly the six arguments that the renderer writes, no
more, followed at most by one `--no-daemon`. Pohunek's Codex hook handler
appends that flag to every Codex launch, profile sessions included. The adapter
drops it: it keeps an interactive Codex off the shared app-server daemon, and
`codex exec` always runs its app-server in its own process and has no such
option. The renderer is an allowlist for the arguments of the base profile: it
accepts these spellings, each setting at most once, and drops them because the adapter passes them itself. Every other
argument is refused, among them any other sandbox mode or approval policy,
`--dangerously-bypass-approvals-and-sandbox`, `--approve-for-me`, `--add-dir`,
`--cd`, `--profile`, `--model` and every other `--config` key.

- Sandbox: `--sandbox read-only`, `--sandbox=read-only`, `-s read-only` and
  `-s=read-only`.
- Approval policy: `--config` or `-c`, with the value as the next argument or
  after `=` (`--config=...`, `-c=...`), and the value `approval_policy="never"`
  or `approval_policy=never`.

Configuration comes from the profile and a file next to the program, never from
a default in the code:

| Setting | Where | Meaning |
|---|---|---|
| Codex executable | `--codex-binary` of `render_profile.py`: written to the file `<wrapper>.codex-binary` next to the resolved wrapper, and passed as the first program argument | Absolute path. The file must be a regular file owned by the current user and not writable by the group or others. A missing or invalid file, or a first argument that differs from the file, stops the run with a message. |
| Input limit | `--maximum-input-bytes` | The largest request that the adapter accepts. A re-review request also carries the prior findings and the discussion (bounded by `limits.max_discussion_characters`), so the limit must cover them. |
| Result limit | `--maximum-result-bytes` | The largest report that the adapter prints. |
| Retention | `--maximum-retention-seconds` | How long the adapter stays alive after the result, so that the coordinator can read it. |
| Model and reasoning effort | `--model`, `--reasoning-effort` | `--reasoning-effort` is one of `low`, `medium`, `high`, `xhigh`. |
| Telemetry and config home | `env` table of the base profile | Optional. Only `OTEL_*` and `CODEX_HOME` are copied; every other key is dropped and its name printed on stderr. Without them the run has no telemetry and uses `~/.codex`. |

The base profile must use `base = "codex"`. A bare `--version` call gets no
profile arguments and no profile environment, only the launch `PATH`, so the
adapter finds the Codex executable through the file next to itself, never
through the home directory, the working directory or a `PATH` lookup, and prints
the version line of that executable. It runs the executable with the environment
of a review run, without the variables that the Claude adapter drops.

The tests of the adapter are in `adapters/codex/tests` and run with
`scripts/gates adapters`. They use fake executables only. The adapter keeps the
frame and exit line texts above, which the engine reads; the rename is planned
in #24.

The `agent_profile` key of a reviewer names the profile that its sessions start
with. The profile must exist on every review host that runs the reviewer.

## Finding prefixes

Every reviewer has a `finding_prefix`, one uppercase letter, unique across the
policy. The prefix becomes the first letter of every finding id the reviewer
produces, so findings from different reviewers stay apart in the body, in the
notifications and in the verifier claims.

```yaml
reviewers:
  opus:
    protocol: claude-structured
    agent_profile: autoreview-opus
    label: Opus
    finding_prefix: O
  sonnet:
    protocol: claude-structured
    agent_profile: autoreview-sonnet
    label: Sonnet
    finding_prefix: S
  codex:
    protocol: codex-envelope
    agent_profile: autoreview-codex
    label: Codex
    finding_prefix: X
```

The validation rules accept any uppercase letter, so a reviewer could take the
letter `P`. The engine uses `P` for the prior findings it publishes again (see
[incremental reviews](incremental-reviews.md)), so a reviewer with prefix `P`
can produce an id that is already in use. Choose prefixes other than `P`.

## Mixing models

Several reviewers in one pipeline give independent opinions on the same
change. Each reviewer runs its own session with its own agent profile, so the
reviewers can use different models, and different agents of the same protocol.
The published review still reads as one: a finding that several reviewers report
on the same path, line and severity appears once, labelled with every reviewer
(see [how it works](how-it-works.md#5-result-parsing-and-limits)).

```yaml
pipelines:
  core:
    reviewers: [opus, sonnet]     # two Claude models, same protocol
    incremental_reviewers: [opus] # cheaper re-review, see incremental reviews
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
    verifier:
      reviewer: opus
      prompt: prompts/verifier.md
      escalate_severities: [critical]
      escalate_count: 3
```

Constraints on the mix:

- All reviewers of a pipeline use the same protocol. The prompt is written for
  one request and one result contract, and the engine combines the results of
  all reviewers in one step. A mixed pipeline fails validation with
  `all reviewers must use the same protocol`. Split the pipeline instead.
- `incremental_reviewers` is a subset of `reviewers`, and it has at least one
  member.
- The verifier must use `claude-structured`. Only that protocol carries the
  verdict schema that the verifier needs.
- A reviewer with `codex-envelope` cannot act as the verifier.

## Why one pipeline uses one protocol

The engine combines the results of both protocols the same way: the decision,
the blocking flag, the settling of prior findings, the scope of an incremental
review and the notes hand-off do not depend on the protocol. Two differences
remain:

- **One protocol per pipeline.** The pipeline prompt is written for one request
  and one result format, and the engine combines the results of all reviewers of
  a pipeline in one step. A mixed pipeline fails validation with
  `all reviewers must use the same protocol`. Split the pipeline instead.
- **The verifier is `claude-structured`.** Only that protocol enforces the verdict
  schema the verifier needs (see [policy: verifier](../../policy.md#verifier)).
  A `codex-envelope` reviewer cannot be the verifier.

## Adding another agent later

Not in v0.1. autoreview 0.2.0 ships exactly two protocols, and the policy
validator accepts only those two names. A new agent would need a new protocol,
and the design below is the outline that such a protocol would have to meet.

1. **A run contract.** One non-interactive, read-only run per session, started
   through the agent's official pohunek runtime package, with a timeout.
2. **A request.** The trusted context (repository, pull request number, head SHA,
   base branch, review scope, prior findings) and the pipeline prompt, rendered
   the same way for every protocol. The pull request content is marked as
   untrusted.
3. **A result.** A machine-checkable format with the reviewed head SHA, a summary,
   findings with severity, confidence, path, line, title and body, and the list of
   tests the agent ran. The engine must be able to reject anything that does not
   match exactly.
4. **Verdicts, when the protocol supports them.** One verdict per prior finding.
   A protocol without verdicts can run only in a pipeline that has no prior
   findings to settle, or it must add the verdicts first.
5. **A blocking flag.** The result carries it, as both protocols do.
6. **Environment rules.** No GitHub token, no pohunek identity variables, and
   bounded output.
7. **A profile installer.** Support for the new runtime in
   `autoreview profiles install`.
8. **Validation.** The new protocol name is added to the policy validator, and
   the pipeline rules above (one protocol per pipeline, verifier protocol) are
   checked for it.

The notifier design follows the same idea of a registry, see
[notifications](notifications.md#adding-a-backend-design).

## Related pages

- How the results of several reviewers are combined: [how it works](how-it-works.md#5-result-parsing-and-limits).
- Which decision a set of findings leads to: [decisions](decisions.md).
- The verifier and its verdicts: [verification](verification.md).
- Writing the prompt for a pipeline: [writing prompts](../guides/writing-prompts.md).
- Agent runs and untrusted input: [prompt injection](../safety/prompt-injection.md).
