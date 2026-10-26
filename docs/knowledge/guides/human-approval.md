---
type: Guide
id: guide/human-approval
title: Human approval
description: Two Claude reviewers and a verifier review a core repository, blocking findings request changes, and a person approves each clean head from the CLI - with the policy, a walk-through of one pull request, the exact approve and retry commands and the variations.
source_kind: manual
intents: [setup, help]
---

# Human approval

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

The reviewers do the reading; a person makes the decision. Two Claude reviewers
review the core repository, a verifier re-checks blocking findings, and a clean
review waits. Nothing is approved until someone runs `autoreview approve` with
the head commit they checked.

Example policy: [examples/human-approval/policy.yaml](../../../examples/human-approval/policy.yaml).

## When to use it

- The repository is important enough that an automated approval is not
  acceptable, but you still want a second and third opinion before you look.
- You want a stronger model for the first review and a faster one for
  the rest, or two different models for the same change.
- You are willing to run one command per clean review.

Do not use it when:

- Nobody will run the approval. A clean review then waits until
  `approval.timeout_hours` passes and expires (`approval_expired`). Use
  [auto approve](auto-approve.md) or [solo maintainer](solo-maintainer.md).
- Pull requests come from strangers. This example skips them; see
  [public repositories](public-repositories.md) for a safe way to review them.

## The policy

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

pipelines:
  core:
    reviewers: [opus, sonnet]
    # A re-review settles earlier blocking findings, which needs the stronger model.
    incremental_reviewers: [opus]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
    verifier:
      reviewer: opus                 # must use claude-structured, which enforces the verdict schema
      prompt: prompts/verifier.md
      escalate_severities: [critical]
      escalate_count: 3

repositories:
  - match: zajca/pohunek
    trigger: review-requested
    pipeline: core
    on_findings: request_changes
    on_clean: await                  # a clean review waits for autoreview approve
    authors:
      trusted:
        permission: write            # only people who can write to the repository
      untrusted: skip                # everyone else is not reviewed at all

unmatched: skip

approval:
  timeout_hours: 168                 # a wait longer than a week expires
```

Key lines:

- `reviewers` lists two Claude reviewers, so the pipeline uses one protocol.
  Both are full reviewers.
- `incremental_reviewers: [opus]` means a new push is re-reviewed by Opus alone.
  Re-reviews settle the earlier blocking findings, and that work needs the
  stronger model.
- `verifier.reviewer: opus` re-checks every blocking finding before anything
  is published. A finding it cannot confirm becomes a note with the reason.
  Nothing is dropped silently.
- `escalate_severities` and `escalate_count` decide when an approval turns into
  a wait. They only matter on a rule that approves on its own. On this rule the
  wait already exists, so the escalation changes nothing. The variations show
  how to make it count.
- `on_clean: await` is the decision that needs a person. A waiting review raises the
  `approval_requested` event at level `action`.
- `approval.timeout_hours` is the longest wait. After it, the wait expires and
  the `approval_expired` digest is recorded.

## Walk-through of one pull request

Maintainer `zajca` opens PR #42 in `zajca/pohunek` and requests a review from
the review account.

1. **Candidate and debounce.** The review request makes PR #42 a candidate on
   the next discovery pass. The debounce waits for the pushes to settle before
   the review starts.
2. **Two reviews, one verifier.** Opus and Sonnet each review the change in their
   own pohunek session, read-only, from the base branch. Their findings are
   numbered with their prefixes, `O001`, `S001` and so on.
3. **Verification.** The verifier (Opus) re-checks every blocking finding. Each
   one is confirmed or demoted to a note with the reason. No verifier output
   is dropped.
4. **Decision.** With blocking findings left, the engine publishes one review
   with decision REQUEST_CHANGES. The author sees the inline comments, the
   summary per reviewer, the verification line and the signature
   (`Written by AI: autoreview (posted from review-bot's GitHub account, not by a human)`).
   The `changes_requested` digest is recorded.
5. **Clean review.** With no blocking findings left, the review does not
   approve. It waits. The `approval_requested` event at level `action` goes to
   the notifier, which mentions the person configured in its backend. The
   reference does not say what the author sees on GitHub during the wait; check
   that on a test repository before you rely on it.
6. **You inspect the wait.** `autoreview awaiting` lists the waiting reviews.
   `autoreview review` shows the head you are about to approve.
7. **You approve the head you checked.** The approval binds to the exact head
   SHA. Commands are in the next section.
8. **A new commit supersedes the wait.** Pushing to PR #42 supersedes the wait:
   the `superseded` event is recorded at level `log`, and the new head is
   reviewed. An approval for the old head is never published on the new one.

## Approve, retry and inspect

```sh
# 1. List what is waiting for a person.
autoreview --config /etc/autoreview/config.yaml awaiting

# 2. Look at the review you are about to approve.
autoreview review --repository zajca/pohunek 42

# 3. Resolve the head prefix from the notification (at least 12 characters)
#    to the full head SHA. This is read-only.
autoreview resolve --repository zajca/pohunek 42 3f2a9c1d0b7e

# 4. Approve that exact head. The engine publishes on its next tick.
autoreview approve --repository zajca/pohunek 42 <full-head-sha>
# -> {"publication_status": "pending", "review_id": "zajca/pohunek#PR42@3f2a9c1d0b7e"}

# 5. Run the same approve again to see the publication.
autoreview approve --repository zajca/pohunek 42 <full-head-sha>
# -> publication_status "published", with github_review_id
```

Repeating an approval is safe. The errors you can meet:

| Code | Meaning | What to do |
|---|---|---|
| `review_superseded` | A newer commit replaced the review, or the pull request stopped asking for one | Review the new head, then approve it |
| `approval_expired` | The wait passed `approval.timeout_hours` | Nothing was published; the head needs a new review before it can be approved |
| `head_changed` | The head you passed is not the pull request head | Resolve the current head and approve that |
| `review_not_awaiting_approval`, `approval_not_required` | The review does not wait for a person | Nothing to approve |

If a review stopped after its retries, the `review_stopped` action event is
raised. Retry the head it stopped on:

```sh
autoreview retry --repository zajca/pohunek 42 3f2a9c1d0b7e
```

## Variations

- **Make escalation count.** Add `on_clean: approve` and put both reviewers in
  `incremental_reviewers` in the same order, so the approve rule is valid.
  A demoted `critical` finding or three demoted blocking findings then turn the
  approval into a wait for a person, while a clean run is approved automatically.
  This is the [auto approve](auto-approve.md) pattern with a stricter verifier.
- **Cheaper re-reviews.** Set `incremental_reviewers: [sonnet]`. Earlier blocking
  findings are then settled by the weaker model, which is usually not what you
  want on a core repository.
- **Review requests become labels.** Set `trigger: label:ai:review`. Anyone
  with label permission can then start a review.
- **Notes to the author first.** Add `on_notes: comment` and
  `notes_handoff.reminder_hours`, as in [notes hand-off](notes-handoff.md). It
  requires `on_clean: await` and `review-requested`, which this rule already has.
- **Approve from chat.** Run the same `resolve` and `approve` commands from an
  integration; see [approve from chat with Hermes](hermes-approval.md).

## Combine with

- [Auto approve](auto-approve.md) for the same verifier rules on a repository
  where automatic approval is acceptable.
- [Notes hand-off](notes-handoff.md) to let non-blocking notes go to the author
  before a human approval.
- [Coding-agent drafts](coding-agent-drafts.md) when the author is a coding agent
  whose pull requests must still be approved by you.
- [Combining patterns](combining.md) for more recipes, and
  [writing prompts](writing-prompts.md) for the verifier prompt.
