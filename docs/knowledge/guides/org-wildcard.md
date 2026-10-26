---
type: Guide
id: guide/org-wildcard
title: Organisation rules
description: One organisation matched by a wildcard, with two repositories that have their own pipelines and prompts and a generic rule for everything else - with the policy, the first-match-wins order, a walk-through per repository, the prompt files that init does not copy, and the commands to check the rules.
source_kind: manual
intents: [setup, project, help]
---

# Organisation rules

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

One rule covers every repository of an organisation, and more specific rules
above it cover the repositories that need their own pipeline and prompt. This
is the pattern for a team that owns several repositories with different
languages: a Rust desktop application, an Android application and everything
else.

Example policy: [examples/org-wildcard/policy.yaml](../../../examples/org-wildcard/policy.yaml),
with prompts in [examples/org-wildcard/prompts/](../../../examples/org-wildcard/prompts/).

## When to use it

- A GitHub organisation has several repositories and you want one policy file for
  all of them.
- Some repositories need a different reviewer, a different checklist or a
  different outcome from the rest.
- New repositories should be covered by default, with the generic rule.

Do not use it when:

- Only one or two repositories are involved. Write one rule per repository
  instead; see [single repository comment](single-repo-comment.md).
- You cannot say what the generic rule should do to a repository you have not
  looked at yet. The wildcard approves clean reviews in this example.

## The policy

```yaml
reviewers:
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

pipelines:
  companion:
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]
    prompt: prompts/companion.md     # Rust and Slint checklist, not written by init
    checkout: base
    project_sources: [manual]
  android:
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]
    prompt: prompts/android.md       # Kotlin checklist, not written by init
    checkout: base
    project_sources: [manual]
  generic:
    reviewers: [codex]
    incremental_reviewers: [codex]
    prompt: prompts/generic.md       # written by init
    checkout: base
    project_sources: [manual]

repositories:
  # Specific rules come first. A pohunekd/* rule above them would make them unreachable.
  - match: pohunekd/pohunek-companion
    trigger: label:ai:review
    pipeline: companion
    on_findings: request_changes
    on_clean: comment
    authors:
      trusted:
        permission: write
      untrusted: skip
  - match: pohunekd/pohunek-companion-android
    trigger: label:ai:review
    pipeline: android
    on_findings: request_changes
    on_clean: comment
    authors:
      trusted:
        permission: write
      untrusted: skip
  - match: pohunekd/*                # every other repository of the organisation
    trigger: label:ai:review
    pipeline: generic
    on_findings: request_changes
    on_clean: approve                # valid: generic's incremental reviewers equal its reviewers
    authors:
      trusted:
        permission: write
      untrusted:
        pipeline: generic            # base checkout, so it is safe for other authors
        on_findings: comment
        on_clean: comment

unmatched: skip
```

Key lines:

- **First match wins.** A pull request in `pohunekd/pohunek-companion` matches
  the first rule and never reaches the wildcard. Put the wildcard last.
- **Shadowing is an error.** If `pohunekd/*` were listed first, validation would
  stop at the first specific rule below it with
  `repositories[1] (pohunekd/pohunek-companion): unreachable, pohunekd/* matches first`.
  Run `autoreview validate` after every reordering.
- **Only the generic rule approves.** The two specific rules comment, because a
  companion or Android change is worth a person's look even when it is clean.

## Walk-through per repository

Three pull requests, all labelled `ai:review` by a write-permission maintainer.

**`pohunekd/pohunek-companion`, PR #17 (Rust and Slint).**

1. The label makes PR #17 a candidate. The first rule matches, so the pipeline
   is `companion`.
2. Sonnet reads the repository from the base branch, then reviews with
   `prompts/companion.md`, which tells it to look at error handling, threads and
   the Slint event loop first.
3. Blocking findings are published as REQUEST_CHANGES. A clean review is published
   as a comment with the summary. Nothing is approved.
4. Notifications: `changes_requested` (digest) or `commented` (digest). With the
   default `notify` section they reach the log only.

**`pohunekd/pohunek-companion-android`, PR #9 (Kotlin).** The same flow with the
`android` pipeline and `prompts/android.md`.

**`pohunekd/pohunek.dev`, PR #31 (website).** None of the specific rules match,
so the `pohunekd/*` rule applies: Codex from the base checkout with the generic
prompt.

1. Clean review: APPROVE from the review account. The `approved` event (digest).
2. Blocking findings: REQUEST_CHANGES. The `changes_requested` event (digest).
3. A pull request by an author without write permission gets the untrusted
   pipeline: a comment, never an approval, from the base checkout.

The approval on the website is the reason this rule is the riskiest one in the
example. The [auto approve](auto-approve.md) guide shows the verifier and the
escalation that add a person back in when a serious finding is demoted.

## Prompts

`autoreview init` writes `prompts/generic.md` and `prompts/verifier.md` only. The
companion and Android prompts are part of this example and must be copied next
to the policy. Without them, validation fails because the policy names prompt files
that do not exist:

```sh
autoreview init --dir /etc/autoreview --login <github-login> \
  --pohunek-host <pohunek-host> --example org-wildcard
cp examples/org-wildcard/prompts/companion.md examples/org-wildcard/prompts/android.md \
  /etc/autoreview/prompts/
autoreview --config /etc/autoreview/config.yaml validate
```

Prompt paths are relative to the policy file. The two example prompts state the
read-only and untrusted-content rules; [writing prompts](writing-prompts.md)
explains how to adapt them.

## Commands

```sh
autoreview --config /etc/autoreview/config.yaml candidates        # what the next pass would review
autoreview review --repository pohunekd/pohunek-companion 17      # engine state for PR #17
autoreview review --repository pohunekd/pohunek.dev 31
autoreview retry --repository pohunekd/pohunek-companion 17 3f2a9c1d0b7e
```

`candidates` is the check to run after you add a repository: it shows which rule
each pull request would match, before any review runs.

## Variations

- **A third specific repository.** Insert a `match: pohunekd/<name>` rule above
  `pohunekd/*`, with its own pipeline or with the generic one. Validate afterwards.
- **Generic rule that waits.** Change the generic rule to `on_clean: await`, so the
  organisation's clean reviews wait for a person.
- **Generic rule with two reviewers.** Add a Sonnet reviewer to the generic
  pipeline's `reviewers` and `incremental_reviewers`, in the same order, so the
  approve rule stays valid.

## Combine with

- [Several review hosts](multihost.md) when the repositories are spread over hosts.
- [Auto approve](auto-approve.md) for the verifier and escalation on the generic
  approval rule.
- [Public repositories](public-repositories.md) for the untrusted side of a rule
  that accepts pull requests from strangers.
- [Writing prompts](writing-prompts.md) to adapt the companion and Android prompts.
