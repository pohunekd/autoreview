---
type: Guide
id: guide/writing-prompts
title: Writing prompts
description: How review and verifier prompts work - what autoreview appends as trusted context, what a prompt should and must not ask for, per-repository prompts, the verifier prompt, path and size rules, and how to test a prompt on one pull request.
source_kind: manual
intents: [setup, help]
---

# Writing prompts

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A pipeline names one review prompt, and the verifier names one more. The
prompt tells the reviewer how to review; autoreview decides what to review
and what to publish. Start from the generic prompt that `autoreview init`
writes into `prompts/generic.md`, and change it only where your repositories
need something different.

## What the reviewer receives

The reviewer gets the prompt file followed by a trusted request context that
autoreview appends. The context contains:

- the repository and the pull request number,
- the head SHA that is reviewed, and the trusted base commit,
- the reviewer's label, as shown in the review body,
- the review scope: `full`, or `incremental` with the previous reviewed head,
- for an incremental review, the prior blocking findings and the instruction
  to settle each one as `fixed`, `still_open` or `answered`,
- the discussion of earlier findings: thread replies and recent comments.

The pull request title, description, code, comments and every file at the
head are untrusted data. The discussion is untrusted too, even though
autoreview selects it: a comment can say anything, including instructions.
The prompt must tell the reviewer to treat all of it as data. The generic
prompt does this in its opening paragraphs; keep those paragraphs in every
custom prompt.

Because the context is appended by autoreview, a prompt does not repeat the
repository name, the head SHA or the scope. It refers to them as the trusted
request context.

## What a prompt should do

- State that the reviewer is read-only and must not change files, commit,
  push or publish anything. The reviewer runs in a session that autoreview
  reads back; publishing belongs to autoreview.
- Name the repository guidance to read from the trusted base, for example
  `AGENTS.md`, `CLAUDE.md` and `docs/`, read with `git show <base-sha>:<path>`.
  This works with `checkout: base`; see [policy pipelines](../../policy.md#pipelines).
- Say which checks matter for your code, and what makes a finding blocking.
  The blocking definition in the generic prompt matches what autoreview
  enforces: data loss or corruption, a security vulnerability, a break of an
  existing consumer, a wrong result or crash on a reachable path, a broken
  locking guarantee, or a test whose only proof is lost. Anything else is a
  note.
- Ask for anchored findings on changed right-side lines. A finding outside
  the diff is moved to the closest changed line, so the reviewer should name
  the other location in the body.
- Keep the summary substantive and bounded. `limits.min_summary_characters` and
  `limits.max_summary_characters` apply. A result outside a limit is rejected,
  and the reviewer gets a fresh session.

## What a prompt must not do

- It must not define a result format. A pipeline's reviewers share one
  request and result contract, which the protocol enforces (a JSON schema for
  `claude-structured`, an envelope for `codex-envelope`). Output that does not
  match the contract is unusable, and the reviewer gets a fresh session.
- It must not ask for GitHub actions, such as posting a comment, requesting a
  change or approving. The reviewer has no such authority, and approvals are
  decided by the policy.
- It must not ask the reviewer to report CI status, check runs or workflow
  logs. Those are not part of the review.
- It must not contain secrets, tokens or internal host names. The prompt file
  is committed to a repository, so anything in it is shared.
- It must not tell the reviewer to follow instructions found in the
  repository or the pull request. The generic prompt says that repository
  content is data.
- It must not mention people for notification. Model output is neutralized
  on publication: @mentions, raw HTML and images are removed, so a prompt that
  relies on them does nothing.

Keep the prompt short and specific. The generic prompt is about 25 lines; a
prompt for one codebase usually adds a few lines of local conventions, such as
the test helpers the repository uses.

## Per-repository prompts

Each pipeline has exactly one prompt, and each rule uses one pipeline. A
repository that needs its own prompt therefore needs its own pipeline:

```yaml
pipelines:
  core:
    reviewers: [opus]
    incremental_reviewers: [opus]
    prompt: prompts/core.md          # only for zajca/pohunek
    checkout: base
    project_sources: [manual]
  generic:
    reviewers: [opus]
    incremental_reviewers: [opus]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
```

Several repositories may share one prompt by sharing one pipeline. Do not
create a pipeline per repository just for the prompt file: the rule-level
settings are what differs between repositories, and the pipeline holds the
reviewers, checkout and verifier.

## The verifier prompt

The verifier re-checks every blocking finding, and every prior finding that the
reviewers call answered, before anything is published. It has its own prompt,
`verifier.prompt` in the pipeline. The shipped verifier prompt has the same
structure as the reviewer prompt: read-only, trusted base, a checkout of the
head, and the rule for blocking harms.

A verifier prompt must ask the verifier to trace each claim to the code at the
head and to decide one of two things: the scenario cannot happen (refuted,
with the guard, caller, schema or configuration that rules it out), or it
happens with a result that is or is not blocking. Claims that stay unconfirmed
become notes with the reason; nothing is dropped. The verifier must use the
`claude-structured` protocol.

## Path and size rules

- `prompt` is a relative path. It resolves inside the directory of
  `policy.yaml`, and it must not leave that directory.
- The file is UTF-8 and not empty.
- The file is at most 64 KiB. Validation reads the prompt and rejects a larger
  one; split the guidance into the repository's own `AGENTS.md` instead, which
  the reviewer reads from the base.

Put prompts in a `prompts/` directory next to `policy.yaml`, as `autoreview
init` does. `autoreview validate` reads every prompt and reports a missing or
oversized one.

## Testing a prompt on one pull request

Test a change to a prompt on a single pull request before it applies to a whole
repository. Use a label trigger for the test, because it only starts a review
when you ask for one:

1. Edit the rule for the repository. Set `trigger: label:ai:review-test`, using
   a label name that exists on the repository. Keep the pipeline with the
   prompt you are testing. A repository has one rule, so this replaces its
   `review-requested` trigger while the test runs.
2. Run `autoreview validate`, then `autoreview candidates`. The pull request
   should appear with the label trigger.
3. Add the label to the pull request and run `autoreview poll`.
4. Follow the review with `autoreview review --repository <owner>/<repo> <pr>`.
   Read the summary, the findings and the anchors.
5. Restore the trigger to `review-requested` and validate again.

A policy edit applies to the next review only, so a review already running keeps
the old rule. Run the test on a pull request that already has a real diff, so
the prompt has something to check. For a verifier change, use a pull request
that already has at least one blocking finding.

For the review flow itself, see [how it works](../concepts/how-it-works.md).
For the policy keys, see the [policy reference](../../policy.md).
