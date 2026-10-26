---
type: Guide
id: guide/single-repo-comment
title: Comment on one repository
description: Run autoreview on one repository with one Codex reviewer and comments only, the safest first rollout, with the policy, a step-by-step pull request walk-through and the variations that add authority.
source_kind: manual
intents: [setup, help]
---

# Comment on one repository

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

This is the first setup to run. One repository, one reviewer, a label that
starts a review, and only comments on the pull request. Nothing is approved and
nothing is blocked, so a wrong review costs a reader a minute, not a merge.

Example policy: [examples/single-repo-comment/policy.yaml](../../../examples/single-repo-comment/policy.yaml).
Shared configuration: [examples/config.yaml](../../../examples/config.yaml).

## When to use it

- You want to see what autoreview says about real pull requests before it can
  affect a merge.
- The repository is yours or your team's, and every author is trusted enough to
  have their code read by a read-only reviewer.
- One model is enough. Codex is the reviewer here.

Do not use it when:

- Reviews must block a merge. Use [human approval](human-approval.md) or
  [solo maintainer](solo-maintainer.md), which request changes.
- The repository accepts pull requests from strangers. Start from
  [public repositories](public-repositories.md) instead.

## The policy

```yaml
reviewers:
  codex:
    protocol: codex-envelope         # runs Codex and prints a result envelope
    agent_profile: autoreview-codex  # the pohunek profile created by autoreview profiles install
    label: Codex
    finding_prefix: X                # findings are X001, X002, ...

pipelines:
  default:
    reviewers: [codex]
    incremental_reviewers: [codex]   # a new push is reviewed by the same reviewer
    prompt: prompts/generic.md       # written by autoreview init
    checkout: base                   # repository guidance comes from the trusted base
    project_sources: [manual]        # only repositories registered with pohunek project add

repositories:
  - match: zajca/pohunek
    trigger: label:ai:review         # a maintainer adds the ai:review label to start a review
    pipeline: default
    on_findings: comment             # blocking findings are published as a comment, not a block
    on_clean: comment                # a clean review is a comment as well
    authors:
      trusted: any                   # comments cannot merge or block, so every author is reviewed

unmatched: skip                      # pull requests no rule matches are never reviewed
```

Key lines:

- `checkout: base` starts the reviewer's worktree at the base branch tip. A
  pull request cannot change the instructions the reviewer reads first.
- `on_findings: comment` and `on_clean: comment` are the reason this setup is
  safe to start with. The review never requests changes and never approves.
- `trusted: any` is allowed here only because the outcome is a comment. Once a
  rule can request changes or approve, state who is trusted (see the
  [solo maintainer](solo-maintainer.md) guide).
- `incremental_reviewers` must be a subset of `reviewers`. Here both lists are
  `[codex]`, so every re-review uses the same reviewer as the full review.
- `unmatched: skip` is the only value. Pull requests in other repositories are
  ignored.

The file also lists every event with a level and an `approval.timeout_hours`. The
full file is in the example directory.

## Walk-through of one pull request

Assume maintainer `zajca` opens PR #42 in `zajca/pohunek` from a branch.

1. **Opening the pull request does nothing yet.** The rule's trigger is the
   label `ai:review`, so the pull request is not a candidate. Check with
   `autoreview candidates` (read-only): it lists what the next discovery pass
   would consider.
2. **The label starts the review.** `zajca` adds `ai:review`. The next discovery
   pass (the `init` configuration polls every 60 seconds) makes the pull request
   a candidate.
3. **The debounce waits for the pushes to settle.** The `init` configuration
   sets `debounce_seconds: 120`: the review starts after two quiet minutes, so a
   burst of pushes is reviewed once.
4. **Codex reviews in its own pohunek session.** The adapter runs Codex read-only
   in a worktree that starts at the base branch. The review cannot push, comment
   or approve on its own.
5. **Findings are checked and published.** No verifier is configured, so every
   finding is published as it came. Findings on changed lines become inline
   comments; findings elsewhere are listed in the body.
6. **GitHub shows one review.** The author sees a single review with the
   decision COMMENT. The body has a summary, the findings as inline comments,
   and the signature as its last line, for example
   `Written by AI: autoreview (posted from review-bot's GitHub account, not by a human)`.
   The review does not block the merge.
7. **Notifications.** The start is a `started` event (level `log`). The
   published result is a `commented` event (level `digest`). With the default
   `notify` section that `init` writes, both only reach the structured log. Add
   a backend to see them elsewhere; see the [notifiers guide](notifiers.md).
8. **A new push is re-reviewed.** After the debounce, Codex reviews only the new
   commits and settles earlier findings, as a Claude reviewer does; see
   [incremental reviews](../concepts/incremental-reviews.md). As in every pipeline,
   only blocking findings of severity high or critical block; the others are
   notes. Here the rule never requests changes, so the outcome stays a comment.

If a review stops after its retries you get a `review_stopped` action event.
The rule has no approval step, so nothing waits on you otherwise.

## Commands

```sh
autoreview --config /etc/autoreview/config.yaml validate        # policy and prompts load
autoreview --config /etc/autoreview/config.yaml candidates      # what the next pass would review
autoreview reviews 20                                           # newest reviews
autoreview review --repository zajca/pohunek 42                 # everything about PR #42
```

If PR #42 stopped, retry it on the head it stopped on (the form shown in the
notification is the first 12 characters of the head):

```sh
autoreview retry --repository zajca/pohunek 42 3f2a9c1d0b7e
```

`retry` refuses with `head_changed` when the pull request has a newer head than
the one you pass. Run `autoreview review` again and retry the current head.

## Variations

- **Review requests instead of labels.** Change the trigger to
  `trigger: review-requested`. The pull request then needs a review requested
  from the `login` of the credential that serves the repository. Nothing else changes.
- **Blocking reviews.** Set `on_findings: request_changes`. Blocking findings
  now request changes; the rule still never approves.
- **Two reviewers.** Add a second `codex-envelope` reviewer under `reviewers`,
  put both in `pipelines.default.reviewers`, and list the same reviewers in
  `incremental_reviewers`, in the same order. A Claude reviewer cannot join this
  pipeline, because all reviewers of one pipeline must use the same protocol.
- **Trust by permission.** Replace `trusted: any` with
  `trusted: {permission: write}` and `untrusted: skip` to review only the
  people who can write to the repository.

## Combine with

- [Solo maintainer](solo-maintainer.md) when the same repository needs
  approvals for contributors.
- [Public repositories](public-repositories.md) when strangers can open pull
  requests.
- [Notifiers](notifiers.md) to receive the `commented` and `started` events
  outside the log.
- [Quickstart](quickstart.md) for the same steps as a short checklist.
- [Writing prompts](writing-prompts.md) to change what Codex checks.
