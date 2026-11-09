---
type: Guide
id: guide/combining
title: Combining patterns
description: Recipes that layer several autoreview scenarios in one policy - human approval, auto-approve and a wildcard, trusted and untrusted authors in one repository, cheap and strict pipelines, notes hand-off with coding-agent drafts, experiments, notification routing, two review hosts - and the validation rules that interact.
source_kind: manual
intents: [setup, help]
---

# Combining patterns

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

The recipes layer the [scenario guides](index.md) in one `policy.yaml`. Each
shows only the parts that change; the top-level keys are as in the
[complete example](../../policy.md#a-complete-example), and the reviewers are
its `opus`, `sonnet` and `codex`.

Rules are checked against each pull request in the order they are written. The
first rule whose repository matches is the one that applies.

## Recipe a: human approval, auto-approve and a wildcard in one file

The core repository waits for a person, the website approves a clean review
itself, and the rest of the organization is only commented.

```yaml
pipelines:                                 # prompt, checkout base and project_sources are shared
  core: {reviewers: [opus, sonnet], incremental_reviewers: [opus], prompt: prompts/generic.md, checkout: base, project_sources: [manual], verifier: {reviewer: opus, prompt: prompts/verifier.md, escalate_severities: [critical], escalate_count: 3}}
  site: {reviewers: [sonnet], incremental_reviewers: [sonnet], prompt: prompts/generic.md, checkout: base, project_sources: [manual], verifier: {reviewer: sonnet, prompt: prompts/verifier.md, escalate_severities: [critical, high], escalate_count: 1}}

repositories:
  - match: zajca/pohunek                   # 1. specific rule
    trigger: review-requested
    pipeline: core
    on_findings: request_changes
    on_clean: await
    authors: {trusted: {permission: write}, untrusted: skip}
  - match: pohunekd/pohunek.dev            # 2. specific rule above its wildcard
    trigger: review-requested
    pipeline: site                         # incremental equal to reviewers: on_clean approve needs it
    on_findings: request_changes
    on_clean: approve
    authors: {trusted: {permission: write}, untrusted: skip}
  - match: pohunekd/*                      # 3. wildcard last, comment only
    trigger: label:ai:review
    pipeline: site
    on_findings: comment
    on_clean: comment
    authors: {trusted: {permission: write}, untrusted: skip}
```

Specific rules go above the wildcard; otherwise startup fails with
`unreachable, pohunekd/* matches first`.

The `site` verifier escalates a demoted high or critical finding, which turns an
automatic approval into a wait for a person. The wildcard reuses `site` for its
reviewers, but its comment outcomes never approve. Its label trigger means a
person chooses which pull requests are reviewed.

## Recipe b: trusted maintainers approve, strangers get comments, one repository

```yaml
repositories:
  - match: pohunekd/pohunek-companion-android
    trigger: review-requested
    pipeline: strict                       # from recipe c: incremental equal to reviewers
    on_findings: request_changes
    on_clean: approve
    authors:
      trusted:
        logins: [zajca]                    # named maintainers
        permission: maintain               # or anyone with at least maintain
      untrusted:
        pipeline: cheap                    # must check out the base
        on_findings: comment
        on_clean: comment
```

Untrusted pull requests never approve, get no hand-off, and are checked out at
the base. A fork is untrusted unless its author is in `logins`.

## Recipe c: a cheap first pass, two models only where it approves

Two models and a verifier pay off only where an approval happens without a person.

```yaml
pipelines:
  cheap:                                   # protocol codex-envelope
    {reviewers: [codex], incremental_reviewers: [codex], prompt: prompts/generic.md, checkout: base, project_sources: [manual]}
  strict:                                  # protocol claude-structured; equal lists for approve
    {reviewers: [opus, sonnet], incremental_reviewers: [opus, sonnet], prompt: prompts/generic.md, checkout: base, project_sources: [manual], verifier: {reviewer: opus, prompt: prompts/verifier.md, escalate_severities: [critical, high], escalate_count: 2}}

repositories:
  - match: pohunekd/pohunek-companion
    trigger: review-requested
    pipeline: strict
    on_findings: request_changes
    on_clean: approve
    authors: {trusted: {permission: write}, untrusted: skip}
  - match: pohunekd/*
    trigger: label:ai:review
    pipeline: cheap
    on_findings: comment
    on_clean: comment
    authors: {trusted: {permission: write}, untrusted: skip}
```

A pipeline cannot mix `codex` and `opus` (`all reviewers must use the same protocol`),
and the verifier must be a Claude reviewer, so it belongs to `strict`.
The cheap pass costs less, but each Codex finding blocks at any severity and its
re-reviews settle no earlier findings (both planned in #23). Like `strict`, it
reviews the whole pull request again after a rebase or a force push. It has no
verifier, because a verifier must be a Claude reviewer and therefore sits in `strict`.

## Recipe d: notes for humans, drafts of coding agents marked ready

```yaml
ready_draft_authors: [devin-ai-integration]  # plain login; GitHub search shows app/<login>
repositories:
  - match: zajca/pohunek-work
    trigger: review-requested              # on_notes needs this trigger ...
    pipeline: core
    on_findings: request_changes
    on_clean: await                        # ... and this outcome
    on_notes: comment
    authors: {trusted: {permission: write}, untrusted: skip}
notes_handoff: {reminder_hours: 48}        # required because a rule sets on_notes
```

Humans get non-blocking notes first, as a comment; their renewed review request
ends the hand-off. A new head after a hand-off waits for approval, so hand-offs
never repeat back to back.

The coding agent is not handed off: autoreview skips the hand-off for GraphQL
`Bot` authors, because a bot does not act on a comment. Its notes-only review
goes straight to the approval wait.

`ready_draft_authors` marks a draft ready only when it also requests a review.

## Recipe e: an experiment next to the real flow

A label trigger for trying a prompt, next to the real `review-requested` flow of
the owner's other repositories. A repository has one rule, so the experiment
needs a repository of its own:

```yaml
repositories:
  - match: pohunekd/playground             # experiments only (example name)
    trigger: label:ai:experiment
    pipeline: cheap
    on_findings: comment
    on_clean: comment
    authors: {trusted: any}
  - match: pohunekd/*
    trigger: review-requested
    pipeline: core
    on_findings: request_changes
    on_clean: await
    authors: {trusted: {permission: write}, untrusted: skip}
```

Two consequences follow:

- A playground pull request that requests a review from the bot is not
  reviewed by the wildcard. The reference does not say whether a rule with an
  absent trigger falls through to a later rule; confirm with `autoreview candidates`.
- The playground repository cannot use `review-requested`: a second rule for
  it would be unreachable.

## Recipe f: one policy, different notification routing per level

The policy sets the level of each event; `config.yaml` sets where each level goes.
A single event can also go to its own room, see
[event routes](../../configuration.md#event-routes).

```yaml
# policy.yaml: levels only; list every event of the policy reference
notifications: {approval_requested: action, approved: digest, started: log}
```

```yaml
# config.yaml: routing only
notify:
  backends:
    - {name: matrix-action, type: matrix, homeserver_url: https://matrix.example.org, access_token_env: AUTOREVIEW_MATRIX_TOKEN, room: "!action:example.org", mention_user: "@you:example.org"}
    - {name: matrix-digest, type: matrix, homeserver_url: https://matrix.example.org, access_token_env: AUTOREVIEW_MATRIX_TOKEN, room: "!digest:example.org", mention_user: "@you:example.org"}
  routes: {action: [matrix-action], digest: [matrix-digest], log: [matrix-digest]}   # only action mentions a person
  review_queue: {backends: [matrix-digest], max_items: 50}
```

Change a level in the policy and a channel in the configuration independently.
The [notifiers guide](notifiers.md) has the full backend blocks.

## Recipe g: two review hosts with a route override

A repository is registered on both review hosts. Without an override its reviews
run on the first host of `hosts` that answers, so pin it to the host that must
always run them:

```yaml
pohunek:
  hosts: [review-host-a, review-host-b]
  route_overrides:
    zajca/pohunek:
      host: review-host-a
      project_id: <project-id-on-review-host-a>
```

A pinned repository has no fallback: when its host fails, the review waits for
its next retry. Install the profiles on both hosts ([multihost guide](multihost.md)).

## Recipe h: different accounts per owner

Planned in #13.

## Rules that interact

Startup checks these, naming the key on error.
| Constraint | Bites when | Fix |
|---|---|---|
| `on_clean: approve` needs `incremental_reviewers` equal to `reviewers` | A fast re-review on an approving repository | Use the same list, or `on_clean: await` |
| `on_notes: comment` needs `on_clean: await` and `review-requested`; `reminder_hours` then required | A hand-off on a label rule, or a file without the section | Change the trigger or drop `on_notes`; add `notes_handoff` |
| Untrusted authors never approve and get no `on_notes`; their pipeline checks out the base | Strangers on an approving repository | Keep the untrusted section as in recipe b |
| `trusted: any` takes no `untrusted` section | Adding one to an open repository | Remove it, or list trusted logins |
| An unreachable rule is an error | A specific rule below its wildcard | Move the specific rule above |
| One reviewer protocol per pipeline; the verifier needs `claude-structured` | Mixing `codex` and `opus`, or a verifier on Codex | Split the pipeline (recipe c) |
| Own pull requests always get a comment | Testing approval with a bot-authored pull request | Test with another author |

Policy edits apply to the next review; test on a live pull request as in
[writing prompts](writing-prompts.md#testing-a-prompt-on-one-pull-request).
