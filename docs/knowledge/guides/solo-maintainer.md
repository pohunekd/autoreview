---
type: Guide
id: guide/solo-maintainer
title: Solo maintainer
description: One maintainer's repositories - own pull requests always get a comment, trusted contributors can be approved or sent back, untrusted authors get a comment from a cheaper reviewer - with the policy, walk-throughs for each kind of author and the commands to inspect and retry.
source_kind: manual
intents: [setup, help]
---

# Solo maintainer

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A single maintainer reviews the repositories they own. Their own pull requests
get a comment, because GitHub does not let an author approve or request changes
on their own pull request. Contributors with write permission get a real
decision: request changes on findings, approve a clean review. Everyone else is
reviewed with a comment from a cheaper reviewer.

Example policy: [examples/solo-maintainer/policy.yaml](../../../examples/solo-maintainer/policy.yaml).

## When to use it

- You maintain the repositories alone or mostly alone, and want the review
  service to do the first pass on everything, including your own work.
- Contributors have write permission, and you trust their pull requests enough
  to let a clean review approve them.

Do not use it when:

- A person must approve every clean review. Use [human approval](human-approval.md).
- Strangers open pull requests. Use [public repositories](public-repositories.md),
  which never approves an untrusted author.

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
  standard:
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]  # equal to reviewers, required by on_clean approve
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
  light:
    reviewers: [codex]
    incremental_reviewers: [codex]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]

repositories:
  - match: zajca/pohunek
    trigger: review-requested        # a review request for the credential's login starts the review
    pipeline: standard
    on_findings: request_changes
    on_clean: approve
    authors:
      trusted:
        logins: [zajca]              # the owner
        permission: write            # and anyone who can write to the repository
      untrusted:
        pipeline: light              # checkout is base, so this is safe for anyone
        on_findings: comment
        on_clean: comment            # untrusted authors are never approved
  - match: zajca/pohunek-work
    trigger: label:ai:review         # one trigger per rule: this repository uses a label
    pipeline: standard
    on_findings: request_changes
    on_clean: approve
    authors:
      trusted:
        logins: [zajca]
        permission: write
      untrusted:
        pipeline: light
        on_findings: comment
        on_clean: comment

unmatched: skip
```

Key lines:

- **One trigger per repository.** The validator compares repository patterns
  only, so a second rule for `zajca/pohunek` would be rejected as
  `unreachable, zajca/pohunek matches first`. Choose one trigger per repository.
  This example uses a review request for the core repository and a label for the
  work repository to show both. To put both triggers on one repository, you
  would need two repositories or a policy change, not a second rule.
- **`on_clean: approve` needs equal reviewer lists.** `standard` lists
  `[sonnet]` twice. Changing one list without the other fails validation with
  `on_clean approve needs incremental_reviewers equal to reviewers`.
- **The untrusted section never approves.** `on_clean` is `comment` or `await`
  there, never `approve`, and its pipeline checks out the base.
- **Own pull requests are not approved.** This is enforced by the engine, not
  by the rule. A pull request authored by the reviewing account gets a comment
  and raises `findings_on_own_pr` when it has findings. The `on_findings` and
  `on_clean` values above do not apply to it.

## Walk-through: your own pull request

`zajca` opens PR #57 in `zajca/pohunek` and requests a review from the review
account.

1. The review request makes PR #57 a candidate. The debounce lets the first
   pushes settle before the review starts.
2. Sonnet reviews it in its own pohunek session, read-only, from the base branch.
3. If there are blocking findings, the engine would normally request changes,
   but the pull request is authored by the reviewing account, so it is published
   as a comment instead. An own pull request with findings also raises the
   `findings_on_own_pr` event at level `action`, which means you are expected to
   act.
4. A clean review is published as a comment, not an approval.
5. The notifications with the default `notify` section reach the log only. Add
   a backend (see the [notifiers guide](notifiers.md)) to get `findings_on_own_pr`
   where you read messages.

## Walk-through: a contributor with write permission

A contributor with write permission, opens PR #58 and requests a
review from the review account.

1. Their author class is trusted (the permission matches), and the rule is the
   `zajca/pohunek` rule with the `standard` pipeline.
2. Findings: the review is published as REQUEST_CHANGES. The `changes_requested`
   event (digest) records it. The author sees the inline comments and the
   signature line.
3. Clean: `on_clean: approve` publishes an approval from the review account
   without a human step. The `approved` event (digest) records it. Nobody confirms
   this approval, which is why the reviewers of `standard` are equal for both
   the full and the incremental review.
4. A new push to #58 is reviewed by the same pipeline. Whatever the earlier
   decision was, the rule decides the new one.

## Walk-through: an untrusted author

Someone without write permission opens PR #59 in `zajca/pohunek`, and the review
request is sent to the review account.

1. The author is not in `logins` and has no write permission, so the untrusted
   section applies: the `light` pipeline runs Codex from the base checkout.
2. The outcome is a comment, whatever the findings are. The engine never
   requests changes or approves for this author class.
3. The author class is resolved when the review is dispatched and stored with
   it, so a later permission change applies to the next review only.

## Commands

```sh
autoreview --config /etc/autoreview/config.yaml validate
autoreview review --repository zajca/pohunek 57     # engine state for PR #57
autoreview review --repository zajca/pohunek-work 12
autoreview reviews 20                               # newest reviews across repositories
```

The reference has no command that withdraws a published review. A new commit
starts a new review of the new head, and that review is published on its own.

If a review stopped, retry it on the head it stopped on:

```sh
autoreview retry --repository zajca/pohunek 58 <head-prefix>
```

## Variations

- **Human approval for contributors.** Change `on_clean: approve` to
  `on_clean: await`. Clean contributor reviews then wait for `autoreview approve`,
  as in [human approval](human-approval.md).
- **Label for both repositories.** Change the `zajca/pohunek` trigger to
  `label:ai:review`. The rule then needs the label, not a review request.
- **Stop reviewing strangers.** Change `untrusted:` to `untrusted: skip`. The
  untrusted pipeline is then unused and can be deleted.
- **Notes to the author first.** Add `on_notes: comment` to a rule that uses
  `on_clean: await` and `review-requested`, and add `notes_handoff.reminder_hours`.
  See [notes hand-off](notes-handoff.md).

## Combine with

- [Single repository comment](single-repo-comment.md) to start on one repository
  before adding decisions.
- [Human approval](human-approval.md) for a stronger reviewer and a person who
  approves.
- [Public repositories](public-repositories.md) for the untrusted side in a
  repository that takes pull requests from strangers.
- [Notes hand-off](notes-handoff.md) for non-blocking notes.
- [Combining patterns](combining.md) for more recipes.
