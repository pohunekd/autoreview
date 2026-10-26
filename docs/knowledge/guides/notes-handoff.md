---
type: Guide
id: guide/notes-handoff
title: Notes hand-off
description: Findings that are only notes go to the author as a comment first, the author's renewed review request moves the pull request to approval, and an unanswered hand-off is reminded once - with the policy, the hand-off states step by step, the commands to see them and the variations.
source_kind: manual
intents: [setup, help]
---

# Notes hand-off

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A review can be clean of blocking findings and still have useful notes. With
`on_notes: comment`, a review whose findings are all notes is not sent to a
person for approval straight away. It goes to the author as a comment first. The
author then decides whether to accept it, by asking for a review again, or to
change the code, by pushing. Only a human approval can finish the hand-off.

Example policy: [examples/notes-handoff/policy.yaml](../../../examples/notes-handoff/policy.yaml).

## When to use it

- The repository's reviews often contain notes that a person should read before
  approving, for example style and documentation points.
- You want the author to see the notes on the pull request before you spend an
  approval on the head.
- Authors are people, not bots. Bot authors skip the hand-off, so use this only
  for repositories whose pull requests come from people.

Do not use it when:

- Every clean review should wait for approval without a comment. Use
  [human approval](human-approval.md) without `on_notes`.
- The rule uses `on_clean: approve` or a label trigger. The validator rejects
  `on_notes` unless the rule is `on_clean: await` with `trigger: review-requested`.

## The policy

```yaml
reviewers:
  sonnet:
    protocol: claude-structured
    agent_profile: autoreview-sonnet
    label: Sonnet
    finding_prefix: S

pipelines:
  core:
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]

repositories:
  - match: pohunekd/pohunek-companion
    trigger: review-requested        # required with on_notes: the author's renewed request ends the hand-off
    pipeline: core
    on_findings: request_changes
    on_clean: await                  # required with on_notes
    on_notes: comment                # the only value; notes-only reviews go to the author first
    authors:
      trusted:
        permission: write
      untrusted: skip

unmatched: skip

approval:
  timeout_hours: 168

notes_handoff:
  reminder_hours: 24                 # one reminder after 24 hours without a renewed request
```

Key lines:

- `on_notes` is only valid with `on_clean: await` and `trigger: review-requested`.
  Both are needed for the hand-off to end where it should: at the author's
  renewed review request, followed by a human approval.
- `notes_handoff` is required exactly when a rule sets `on_notes`. Without a rule
  that sets it, the section is an error too. Remove both to turn the hand-off off.
- `reminder_hours` (1 to 720) is how long an unanswered hand-off waits before the
  one reminder, the `handoff_reminder` event at level `action`.
- Bot authors skip the hand-off, so a bot's notes-only review goes straight to the
  approval wait. The `untrusted: skip` line keeps strangers out of the rule
  entirely.

## The states of a hand-off

Assume PR #23 in `pohunekd/pohunek-companion`, requested for review from the
review account by a maintainer.

1. **Notes only, first head.** The review has no blocking findings, only notes.
   It is published as a comment to the author, with the notes as inline comments
   and the summary. The pull request is not approved, and no approval is waiting.
2. **The author asks for a review again on the same head.** The author sends a
   new review request without pushing, which is the renewed review request. The
   hand-off ends: the engine adopts the stored result and the pull request moves
   to the approval wait. The `approval_requested` event at level `action` is
   raised for you.
3. **The author pushes instead.** A new head is reviewed. If that review is again
   notes only, it does not hand off a second time, because two hand-offs in a row
   are not allowed. It goes to the approval wait directly.
4. **Nobody answers.** After `reminder_hours` (24 here) without a renewed request,
   the `handoff_reminder` event is raised once at level `action`.
5. **A blocking finding.** `on_findings: request_changes` applies as usual; the
   hand-off is only for reviews with notes.

Approving at the end of any of these paths is the same as in
[human approval](human-approval.md):

```sh
autoreview --config /etc/autoreview/config.yaml awaiting
autoreview resolve --repository pohunekd/pohunek-companion 23 3f2a9c1d0b7e
autoreview approve --repository pohunekd/pohunek-companion 23 <full-head-sha>
autoreview approve --repository pohunekd/pohunek-companion 23 <full-head-sha>   # shows published
```

Check where a pull request stands before you approve it:

```sh
autoreview review --repository pohunekd/pohunek-companion 23
```

The output lists the current head, the reviews, the published decisions, the
waits and the hand-offs. A hand-off that the author has answered by a renewed
request shows as an approval wait, not as a hand-off.

If the review stopped after its retries (`review_stopped`), retry the head it
stopped on:

```sh
autoreview retry --repository pohunekd/pohunek-companion 23 3f2a9c1d0b7e
```

## Walk-through of the notification flow

1. A maintainer requests a review. `started` (level `log`) is recorded.
2. The review finds only notes. The author sees a COMMENT review with the notes.
   Nothing is posted for approval yet.
3. The author replies on the pull request or requests a review again.
   `approval_requested` (level `action`) reaches the notifier you configured,
   which mentions the person named in its backend.
4. You open the review with `autoreview review` and approve the head, as above.
   `approved` (digest) is recorded when it is published.
5. If the author is silent, `handoff_reminder` (action) reaches you once after
   24 hours. Then the review waits. `approval.timeout_hours` (168 hours in this
   example) still applies, after which `approval_expired` (digest) is recorded.

The notification levels above are the defaults that `init` writes. The default
`notify` section routes `action` only to the log, so configure a backend before
you rely on the reminder; see the [notifiers guide](notifiers.md).

## Variations

- **Shorter reminder.** Set `notes_handoff.reminder_hours: 4` for a busy repository.
- **No hand-off, but the same wait.** Remove `on_notes` and the section. Notes-only
  reviews then count as clean and wait for approval directly, with no comment first.
- **Hand-off for everyone.** Change `authors.trusted` to `any` and remove the
  `untrusted` section, so every author gets the hand-off. Use this only where
  every author is trusted.
- **Several reviewers.** Add a Claude reviewer to `reviewers` and to
  `incremental_reviewers`. The hand-off works the same way with more reviewers.

## Combine with

- [Human approval](human-approval.md) for the verifier and the approval flow that
  the hand-off ends in.
- [Solo maintainer](solo-maintainer.md) for contributors whose notes should reach
  them before you approve.
- [Coding-agent drafts](coding-agent-drafts.md) where bot authors skip the
  hand-off and go straight to approval.
- [Notifiers](notifiers.md) so that `handoff_reminder` reaches a person.
