---
type: Guide
id: guide/public-repositories
title: Public repositories
description: Public repositories where strangers open pull requests - a label starts every review, maintainers get a Sonnet review, untrusted authors and forks get a Codex comment from the base checkout, and a private repository skips everyone it does not trust - with the policy, the author classes and the commands to check them.
source_kind: manual
intents: [setup, help]
---

# Public repositories

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

On a public repository anyone can open a pull request, including from a fork.
That author has no reason to be trusted, and their code must never reach the
reviewer's working tree as trusted input or be approved. This pattern gives
maintainers a full review, gives everyone else a comment from a read-only
reviewer on the base branch, and never approves anything from the untrusted side.

Example policy: [examples/public-repositories/policy.yaml](../../../examples/public-repositories/policy.yaml).

## When to use it

- The repository is public and takes pull requests from people you do not know.
- You want the review service to read those pull requests, but not to act on
  them beyond a comment.

Do not use it when:

- Every author is trusted. A rule with `trusted: any` is simpler; see
  [single repository comment](single-repo-comment.md).
- You want strangers' pull requests approved. Approval is not allowed for the
  untrusted side in any version of this policy.

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
  core:
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]
    prompt: prompts/generic.md
    checkout: base                   # the reviewer starts from the trusted base branch
    project_sources: [manual]
  untrusted:
    # Base checkout: the reviewer never works in code from the pull request.
    reviewers: [codex]
    incremental_reviewers: [codex]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]

repositories:
  - match: zajca/pohunek             # outside pohunekd/*, so it needs its own rule
    trigger: label:ai:review
    pipeline: core
    on_findings: request_changes
    on_clean: comment
    authors:
      trusted:
        permission: write
      untrusted: skip                # nobody else is reviewed here
  - match: pohunekd/*
    trigger: label:ai:review         # a maintainer labels the pull request to start the review
    pipeline: core
    on_findings: request_changes
    on_clean: comment                # no approval on any path of this rule
    authors:
      trusted:
        permission: maintain         # maintainers and admins
      untrusted:
        pipeline: untrusted          # base checkout, comment only
        on_findings: comment
        on_clean: comment

unmatched: skip
```

Key lines:

- **Trust is decided per pull request.** The author class is resolved when the
  review is dispatched and stored with it. A fork pull request is untrusted unless
  its author is listed in `trusted.logins`: repository permission alone does not
  make a fork trusted, and this policy does not list any login.
- **The untrusted pipeline checks out the base.** `checkout: base` is required by
  the validator for the untrusted side, so code from the pull request never
  becomes the reviewer's working tree.
- **Untrusted outcomes are comments.** `on_clean` on the untrusted side is `comment`
  or `await`. It is `comment` here, and `approve` is never allowed there.
- **The label is the gate.** The review starts when a maintainer adds `ai:review`.
  A stranger cannot start a review this way, because adding a label needs
  permission on the repository.

## Walk-through: a stranger's pull request

A stranger opens PR #88 in `pohunekd/pohunek.dev` from a fork.

1. **Nothing happens yet.** No label, so the pull request is not a candidate.
   `autoreview candidates` shows it only once a maintainer has labelled it.
2. **A maintainer labels it.** The label makes PR #88 a candidate. The author is
   not in the trusted logins and has no maintain permission, so the untrusted
   section applies.
3. **Codex reviews from the base branch.** The untrusted pipeline runs Codex
   read-only with the generic prompt. The prompt treats the pull request content as
   untrusted data.
4. **The outcome is a comment.** Whatever the findings, the review is published as
   COMMENT, with the findings as inline comments where they anchor. No review
   requests changes and nothing is approved.
5. **Notifications.** `commented` (digest). The `findings_on_own_pr` event does not
   apply, because the review account did not write the pull request.

## Walk-through: a maintainer's pull request

A maintainer with `maintain` permission opens PR #89 in `pohunekd/pohunek.dev`.

1. The label makes it a candidate. The author is trusted by permission, so the
   `core` pipeline runs Sonnet from the base branch.
2. Blocking findings request changes (`changes_requested`, digest). A clean review
   is a comment (`commented`, digest). A maintainer decides the merge.

## Walk-through: the owner's repository

`zajca/pohunek` has its own rule with `untrusted: skip`. A pull request by an
author without write permission is not reviewed at all: no review, no comment,
and no notification. A person with write permission gets the core review.

## Projects must be registered

`project_sources: [manual]` only serves repositories registered with
`pohunek project add` on a review host. A repository that is not registered
fails with `project_route_not_found`. Register each public repository before you
add its label rule.

## Commands

```sh
autoreview --config /etc/autoreview/config.yaml validate
autoreview --config /etc/autoreview/config.yaml candidates           # labelled pull requests the next pass takes
autoreview review --repository pohunekd/pohunek.dev 88               # author class and state for PR #88
autoreview retry --repository pohunekd/pohunek.dev 88 3f2a9c1d0b7e   # after review_stopped, same head only
```

`review` shows the author class the review was given. Check it on the first
fork pull request you review.

## Variations

- **Trust a regular contributor from a fork.** Add their login to
  `authors.trusted.logins` in the `pohunekd/*` rule. Their pull requests then get
  the core pipeline, and they are still never approved by this policy.
- **Let untrusted reviews wait for a person.** Change the untrusted `on_clean` to
  `await`. This is allowed on the untrusted side, but it sends an `approval_requested`
  for a stranger's head, so use it only when someone checks those requests.
- **Skip strangers completely.** Replace the untrusted section with
  `untrusted: skip`. The `untrusted` pipeline is then unused and can be removed.
- **Apply the same policy to an organisation.** Use the `pohunekd/*` rule as the
  last rule of an [organisation rules](org-wildcard.md) policy, with its specific
  rules above it.

## Combine with

- [Single repository comment](single-repo-comment.md) for a private or trusted
  repository that uses the same comment-only outcome.
- [Solo maintainer](solo-maintainer.md) for a repository where the owner's own
  pull requests need a separate outcome.
- [Organisation rules](org-wildcard.md) for the same untrusted side across an
  organisation.
- [Notifiers](notifiers.md) so the `commented` and `started` events reach a person.
