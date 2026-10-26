---
type: Concept
id: concept/reviewers-and-adapters
title: Reviewers and adapters
description: What a reviewer is, the two protocols (claude-structured and codex-envelope), what an adapter enforces, the pohunek agent profiles that autoreview installs, finding prefixes, mixing models, and how a new agent would be added.
source_kind: manual
intents: [setup, help]
---

# Reviewers and adapters

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

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

Both protocols return one result contract: a summary, findings with their blocking
flag, the tests the reviewer ran, and one verdict per prior finding. The protocol
only decides how the result is produced.

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

```bash
autoreview profiles install --host review-host --agent claude --model <model-id> --name autoreview-opus
autoreview profiles install --host review-host --agent codex --model <model-id> --name autoreview-codex
```

The command creates the profile and a small adapter wrapper on that host. Running
it again with the same arguments changes nothing. It refuses to overwrite a
profile that it did not create unless `--replace` is given. `autoreview validate`
prints the profiles the policy needs, and `autoreview init` prints the same list
for a new installation. The reference is
[CLI: profiles install](../../cli.md#autoreview-profiles-install).

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

Both protocols return the same result contract, and the engine handles their
findings the same way: the blocking flag and the decision, the notes, the
verdicts on prior findings, incremental re-reviews and the notes hand-off do not
depend on the protocol. Two differences remain:

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
5. **A blocking flag.** The result carries it, as both current protocols do.
6. **Environment rules.** No GitHub token, no pohunek identity variables, and
   bounded output.
7. **A profile installer.** `autoreview profiles install` support for the new
   runtime, with the same idempotency and `--replace` rule.
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
