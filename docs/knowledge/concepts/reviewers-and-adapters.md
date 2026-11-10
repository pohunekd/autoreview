---
type: Concept
id: concept/reviewers-and-adapters
title: Reviewers and adapters
description: What a reviewer is, the two protocols (claude-structured and codex-envelope), what an adapter enforces, the Claude and Codex adapters and the pohunek agent profiles they run (an installer is planned in #25), finding prefixes, mixing models, and how a new agent would be added.
source_kind: manual
intents: [setup, help]
---

# Reviewers and adapters

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

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

Both protocols return a summary, findings and the tests the reviewer ran. A
`claude-structured` result also carries the blocking flag of each finding and one
verdict per prior finding. The Codex adapter produces both as well; the engine
reads them from a `codex-envelope` result with #23 (see
[the Codex adapter](#the-codex-adapter)).

With `claude-structured` the runtime itself refuses output that does not match
the schema, and the engine also checks the output against the request. The
free-text answer of the run may be prose. When it is JSON it has to agree with
the structured output, or the run is rejected.

With `codex-envelope` the engine looks for one result frame at the end of the
output and decodes it strictly. The frame must not be larger than
`limits.result_max_bytes`, must contain only the known fields, and must name the
reviewed head SHA.

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
  unusable and gets a fresh session, up to `session.reviewer_run_max_attempts`.
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

autoreview does not create the profiles; `autoreview profiles install` is planned
in #25. `autoreview validate` prints the profiles the policy needs.

### The Claude adapter

The adapter for `claude-structured` lives in `adapters/claude/` and uses only
the Python standard library (Python 3.11 or newer). It has two parts:

- `claude-adapter` is the program a profile starts. It runs under `python3 -I`
  (its first line is `#!/usr/bin/env -S python3 -I`). It reads one request that
  pohunek writes into the session, runs Claude Code once in print mode with
  `--setting-sources user`, `--no-session-persistence` and, for a structured
  request, `--json-schema`, and prints one result line that carries the exit
  code and the output of the run. The Claude Code run gets the session
  environment without the pohunek session identity variables, the GitHub token
  variables (`GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`,
  `GITHUB_ENTERPRISE_TOKEN`), `MATRIX_ACCESS_TOKEN` and every `AUTOREVIEW_`
  variable. The adapter then stays alive so that the coordinator can read the
  result before it removes the session.
- `render_profile.py` writes a profile from a base Claude profile. It copies the
  runtime package pin, the arguments and the environment of the base profile,
  refuses an argument that the adapter controls (`--print`, `--output-format`,
  `--settings`, `--setting-sources`, `--json-schema`, `--resume`, `--continue`
  and their short forms), and sets the model and the effort level of the new
  profile. The profile also turns off resuming and disables prompt-box
  detection, because the adapter reads its input without rendering one.

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
| Telemetry | `env` table of the base profile | Optional. Whatever the table holds is copied to the new profile; without a table the run has no telemetry. |

The base profile must use `base = "claude"`. A bare `--version` call, which
pohunek uses to detect the runtime, gets no profile arguments and no profile
environment, only the launch `PATH`. The adapter therefore finds the Claude Code
executable through the file next to itself, never through the home directory,
the working directory or a `PATH` lookup, and prints the version line of that
executable.

The tests of the adapter are in `adapters/claude/tests` and run with
`scripts/gates adapters`. They use fake executables only. The adapter keeps the
result protocol name `claude-otel-noninteractive/v2`, which the engine reads; the
rename is planned in #24.

### The Codex adapter

The adapter for `codex-envelope` lives in `adapters/codex/` and uses only the
Python standard library (Python 3.11 or newer). It has two parts:

- `codex-adapter` is the program a profile starts. It runs under `python3 -I`.
  It reads one request that pohunek writes into the session and checks the
  review checkout before it starts Codex: `HEAD` must be the trusted head SHA of
  the request, the work tree must be clean and the trusted base commit must be
  present. It runs `codex exec` once with `--sandbox read-only`,
  `approval_policy="never"`, `--ignore-user-config`, `--ignore-rules`,
  `--ephemeral` and an output schema. It checks the report (status `completed`,
  the reviewed head, a blocking flag on every finding, well-formed prior
  findings, the result limit) and prints it as one frame,
  `[pohunek-codex-review-result-v1:<base64>]`, followed by the line
  `[pohunek-codex-review exited with status N]`. The Codex run gets the session
  environment without the variables that the Claude adapter drops (see above).
  The adapter then stays alive for the retention time, so that the coordinator
  can read the output before it removes the session. It exits with the status of
  Codex, or with status 64 when the report or the checkout check fails after
  the run.
- `render_profile.py` writes a profile from a base Codex profile. It copies the
  runtime package pin and the environment of the base profile, and turns off
  resuming.

The report has the fields `findings`, `summary`, `tests`, `status`,
`reviewed_head_sha` and `prior_findings`. A finding has `severity`,
`confidence`, `blocking`, `path`, `line`, `title` and `body`. A prior finding
verdict has `key`, `status` (`fixed`, `still_open` or `answered`), `note` and
`relies_on`, as in the Claude result. The engine reads `blocking` and
`prior_findings` of a Codex result with #23; until then it refuses a frame with
fields it does not know, so a `codex-envelope` reviewer that runs on this
adapter needs that change.

Read-only is fixed in the adapter: the arguments above are not configurable, and
the adapter accepts exactly the six arguments that the renderer writes, no
more. The renderer is an allowlist for the arguments of the base profile: it
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
| Input limit | `--maximum-input-bytes` | The largest request that the adapter accepts. |
| Result limit | `--maximum-result-bytes` | The largest report that the adapter prints. |
| Retention | `--maximum-retention-seconds` | How long the adapter stays alive after the result, so that the coordinator can read it. |
| Model and reasoning effort | `--model`, `--reasoning-effort` | `--reasoning-effort` is one of `low`, `medium`, `high`, `xhigh`. |
| Telemetry | `env` table of the base profile | Optional. Whatever the table holds is copied to the new profile; without a table the run has no telemetry. |

The base profile must use `base = "codex"`. A bare `--version` call gets no
profile arguments and no profile environment, only the launch `PATH`, so the
adapter finds the Codex executable through the file next to itself, never
through the home directory, the working directory or a `PATH` lookup, and prints
the version line of that executable.

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

The engine combines the results of both protocols the same way: the decision
and the notes hand-off do not depend on the protocol. A `codex-envelope` result
is read without a blocking flag and without verdicts on prior findings, so every
Codex finding blocks at any severity and a Codex pipeline gets no prior
findings; reading both is planned in #23. The commit history check of an
incremental review is the same for both protocols: a rebase or a force push
widens the review to full. Two more differences remain:

- **One protocol per pipeline.** The pipeline prompt is written for one request
  and one result format, and the engine combines the results of all reviewers of
  a pipeline in one step. A mixed pipeline fails validation with
  `all reviewers must use the same protocol`. Split the pipeline instead.
- **The verifier is `claude-structured`.** Only that protocol enforces the verdict
  schema the verifier needs (see [policy: verifier](../../policy.md#verifier)).
  A `codex-envelope` reviewer cannot be the verifier.

## Adding another agent later

Not in v0.1. The first release ships exactly two protocols, and the policy
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
5. **A blocking flag.** The result carries it, as `claude-structured` does; the
   engine reads it from `codex-envelope` with #23.
6. **Environment rules.** No GitHub token, no pohunek identity variables, and
   bounded output.
7. **A profile installer.** Support for the new runtime in
   `autoreview profiles install` (planned in #25).
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
